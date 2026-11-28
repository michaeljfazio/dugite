//! Before PV9 a duplicate TxIn in a body's input set is COLLAPSED, not rejected;
//! from PV9 it is a decode failure.
//!
//! cardano-ledger f649f9751074d2ab3de033fc3912f29c9862c1f5 (the revision
//! cardano-node 11.1.3 builds with):
//!
//! * `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/Decoder.hs`
//!   L952-962, `decodeSet`: decoder version in [2, 9) is
//!   `Set.fromList <$> decodeCollection ..` (duplicates silently ignored);
//!   from 9 it is `decodeSetEnforceNoDuplicates`.
//! * `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/DecCBOR.hs`
//!   L415, `instance DecCBOR (Set a)` is `decodeSet decCBOR`, and the Alonzo
//!   and Babbage body decoders read keys 0 (inputs), 13 (collateral) and 18
//!   (reference inputs) with `From`, so all three are `Set TxIn`.
//!
//! Every consumer that addresses an input by position (the Spend redeemer
//! pointer, `redeemersNeeded`, the Plutus `txInfoInputs`) therefore sees the
//! sorted, deduplicated set. dugite keeps the wire `Vec`, so each such consumer
//! must dedup. The fixture is the shape of preview Babbage tx
//! `4c4afd467a06e93891221141305c9242ed633e0788c9d8eb10fd66a3117e77b6` (block
//! 2,160,646): ONE script-locked input listed twice, ONE Spend redeemer at
//! index 0.
//!
//! Every transaction here is built as wire CBOR and decoded by the production
//! decoder, so the duplicate reaches the validator exactly as it does on a
//! live block. Both production paths are driven: the mempool entry point
//! (`validate_transaction_with_context` over `mempool_validation_context`) and
//! `apply_block(ValidateAll)` with a single-transaction block.

use std::sync::{Arc, Mutex, OnceLock};

use dugite_ledger::plutus::SlotConfig;
use dugite_ledger::state::{BlockValidationMode, LedgerState};
use dugite_ledger::utxo::UtxoSet;
use dugite_ledger::validation::{validate_transaction_with_context, ValidationError};
use dugite_primitives::address::{Address, ByronAddress, EnterpriseAddress};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_224, Hash28, Hash32};
use dugite_primitives::network::NetworkId;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    OutputDatum, PlutusData, Transaction, TransactionInput, TransactionOutput,
};
use dugite_primitives::value::Value;
use dugite_serialization::{compute_script_data_hash, decode_transaction};
use dugite_uplc::term::{Constant, Term};
use dugite_uplc::Program;
use minicbor::Encoder;
use std::rc::Rc;

const SCRIPT_INPUT_ID: [u8; 32] = [0x38; 32];
const COLLATERAL_ID: [u8; 32] = [0xC0; 32];
const FEE: u64 = 300_000;
const SCRIPT_UTXO_VALUE: u64 = 10_000_000;
const BABBAGE_ERA_ID: u16 = 5;
const CONWAY_ERA_ID: u16 = 6;

// ---------------------------------------------------------------------------
// Capture of the WARN lines `apply_block` emits for a Phase-1 failure it
// tolerates. One global subscriber per test binary; every test filters the
// captured text by its own unique tx hash.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;
    fn make_writer(&'a self) -> Sink {
        self.clone()
    }
}

fn captured() -> &'static Arc<Mutex<Vec<u8>>> {
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    BUF.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sub = tracing_subscriber::fmt()
            .with_writer(Sink(buf.clone()))
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        buf
    })
}

fn log_lines_for(tx_hash: &Hash32) -> Vec<String> {
    let text = String::from_utf8_lossy(&captured().lock().unwrap()).into_owned();
    let needle = tx_hash.to_hex();
    text.lines()
        .filter(|l| l.contains(&needle))
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// `(program 1.0.0 (lam _ (lam _ (lam _ (con unit ())))))`: a PlutusV2
/// validator that ignores datum, redeemer and context.
fn always_succeeds_v2() -> Vec<u8> {
    let term = Term::Lam(Rc::new(Term::Lam(Rc::new(Term::Lam(Rc::new(
        Term::Const(Constant::Unit),
    ))))));
    Program {
        version: Program::version_triple(1, 0, 0),
        term,
    }
    .to_cbor()
    .expect("encode script")
}

fn script_hash(script: &[u8]) -> Hash28 {
    let mut tagged = vec![0x02u8];
    tagged.extend_from_slice(script);
    blake2b_224(&tagged)
}

fn byron_out(lovelace: u64) -> TransactionOutput {
    TransactionOutput {
        // Payload with no extractable root: no bootstrap witness is required.
        address: Address::Byron(ByronAddress {
            payload: vec![0x82, 0x00, 0x01],
        }),
        value: Value::lovelace(lovelace),
        datum: OutputDatum::None,
        script_ref: None,
        is_legacy: false,
        raw_cbor: None,
    }
}

fn script_out(hash: Hash28) -> TransactionOutput {
    TransactionOutput {
        address: Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Mainnet,
            payment: Credential::Script(hash),
        }),
        value: Value::lovelace(SCRIPT_UTXO_VALUE),
        datum: OutputDatum::InlineDatum {
            data: PlutusData::Constr(0, vec![]),
            raw_cbor: None,
        },
        script_ref: None,
        is_legacy: false,
        raw_cbor: None,
    }
}

fn txin(id: [u8; 32]) -> TransactionInput {
    TransactionInput {
        transaction_id: Hash32::from_bytes(id),
        index: 0,
    }
}

fn params_at(pv: u64) -> ProtocolParameters {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = pv;
    // The always-succeeds script calls no builtin, so any well-formed V2 cost
    // model evaluates it; the script-data hash is computed over this same model.
    params.cost_models.plutus_v2 = Some(vec![100; 175]);
    params
}

/// The ledger plus a standalone `UtxoSet` seeded with the script-locked input
/// and a collateral input of `collateral_value` lovelace.
fn ledger_at(pv: u64, script: &[u8], collateral_value: u64) -> (LedgerState, UtxoSet) {
    let params = params_at(pv);
    let mut ledger = LedgerState::new(params.clone());
    ledger.epochs.protocol_params = params;
    ledger.epoch_length = 100;
    let mut utxo_set = UtxoSet::new();
    for (input, output) in [
        (txin(SCRIPT_INPUT_ID), script_out(script_hash(script))),
        (txin(COLLATERAL_ID), byron_out(collateral_value)),
    ] {
        ledger.utxo.utxo_set.insert(input.clone(), output.clone());
        utxo_set.insert(input, output);
    }
    (ledger, utxo_set)
}

fn tx_in_cbor(enc: &mut Encoder<&mut Vec<u8>>, id: &[u8; 32]) {
    enc.array(2).unwrap();
    enc.bytes(id).unwrap();
    enc.u8(0).unwrap();
}

/// Wire CBOR of a Babbage-shaped tx that spends the script-locked input
/// `spend_copies` times (a plain definite array, NO set tag) and lists the
/// collateral input `collateral_copies` times, with one Spend redeemer at
/// index 0. `pv` selects the cost-model view the script-data hash is over.
fn build_tx_cbor(pv: u64, script: &[u8], spend_copies: usize, collateral_copies: usize) -> Vec<u8> {
    let params = params_at(pv);
    let redeemer_ex = (100_000u64, 10_000_000u64);

    // Redeemers: Babbage list form `[[tag, index, data, [mem, steps]]]`.
    let mut redeemers = Vec::new();
    {
        let mut e = Encoder::new(&mut redeemers);
        e.array(1).unwrap();
        e.array(4).unwrap();
        e.u8(0).unwrap(); // Spend
        e.u8(0).unwrap(); // index 0
        e.tag(minicbor::data::Tag::new(121)).unwrap();
        e.array(0).unwrap(); // Constr 0 []
        e.array(2).unwrap();
        e.u64(redeemer_ex.0).unwrap();
        e.u64(redeemer_ex.1).unwrap();
    }
    let redeemer = dugite_primitives::transaction::Redeemer {
        tag: dugite_primitives::transaction::RedeemerTag::Spend,
        index: 0,
        data: PlutusData::Constr(0, vec![]),
        ex_units: dugite_primitives::transaction::ExUnits {
            mem: redeemer_ex.0,
            steps: redeemer_ex.1,
        },
    };
    let sdh = compute_script_data_hash(
        &[redeemer],
        &[],
        &params.cost_models,
        false,
        true,
        false,
        Some(&redeemers),
        None,
        false,
    );

    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(4).unwrap();
    // body
    enc.map(5).unwrap();
    enc.u8(0).unwrap();
    enc.array(spend_copies as u64).unwrap();
    for _ in 0..spend_copies {
        tx_in_cbor(&mut enc, &SCRIPT_INPUT_ID);
    }
    enc.u8(1).unwrap();
    enc.array(1).unwrap();
    enc.map(2).unwrap();
    enc.u8(0).unwrap();
    let mut recipient = vec![0x61u8];
    recipient.extend_from_slice(&[0xBB; 28]);
    enc.bytes(&recipient).unwrap();
    enc.u8(1).unwrap();
    enc.u64(SCRIPT_UTXO_VALUE - FEE).unwrap();
    enc.u8(2).unwrap();
    enc.u64(FEE).unwrap();
    enc.u8(11).unwrap();
    enc.bytes(sdh.as_bytes()).unwrap();
    enc.u8(13).unwrap();
    enc.array(collateral_copies as u64).unwrap();
    for _ in 0..collateral_copies {
        tx_in_cbor(&mut enc, &COLLATERAL_ID);
    }
    // witness set: redeemers (5), plutus v2 scripts (6)
    enc.map(2).unwrap();
    enc.u8(5).unwrap();
    enc.writer_mut().extend_from_slice(&redeemers);
    enc.u8(6).unwrap();
    enc.array(1).unwrap();
    enc.bytes(script).unwrap();
    enc.bool(true).unwrap();
    enc.null().unwrap();
    buf
}

fn decode(era_id: u16, cbor: &[u8]) -> Transaction {
    decode_transaction(era_id, cbor).expect("decode")
}

fn block_of(tx: Transaction, era: Era, pv: u64) -> Block {
    Block {
        header: BlockHeader {
            header_hash: Hash32::from_bytes([1u8; 32]),
            prev_hash: Hash32::ZERO,
            issuer_vkey: vec![],
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vec![],
                proof: vec![],
            },
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
            block_number: BlockNo(1),
            slot: SlotNo(1),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: 0,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion {
                major: pv,
                minor: 0,
            },
            kes_signature: vec![],
        },
        transactions: vec![tx],
        era,
        raw_cbor: None,
        byron: None,
    }
}

/// Mempool path: every error `validate_transaction_with_context` gives.
fn mempool_errors(ledger: &LedgerState, utxo: &UtxoSet, tx: &Transaction) -> Vec<ValidationError> {
    let size = tx.raw_cbor.as_ref().map_or(0, |c| c.len() as u64);
    validate_transaction_with_context(
        tx,
        utxo,
        &ledger.epochs.protocol_params,
        /* current_slot */ 100,
        size,
        Some(&SlotConfig::preview()),
        ledger.mempool_validation_context(),
    )
    .err()
    .unwrap_or_default()
}

/// Block path: a single-transaction block under `ValidateAll`. Returns the
/// block-apply result rendered as text plus every WARN line that names the tx.
fn apply_single_tx_block(
    mut ledger: LedgerState,
    tx: Transaction,
    era: Era,
    pv: u64,
) -> (Result<(), String>, Vec<String>) {
    let _ = captured();
    let hash = tx.hash;
    let result = ledger
        .apply_block(&block_of(tx, era, pv), BlockValidationMode::ValidateAll)
        .map_err(|e| e.to_string());
    (result, log_lines_for(&hash))
}

// ---------------------------------------------------------------------------
// PV8 (Babbage): the duplicate collapses, so the tx is valid.
// ---------------------------------------------------------------------------

#[test]
fn pv8_duplicate_script_input_is_accepted_by_mempool_validation() {
    let script = always_succeeds_v2();
    let (ledger, utxo) = ledger_at(8, &script, 5_000_000);
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(8, &script, 2, 1));
    assert_eq!(
        tx.body.inputs.len(),
        2,
        "fixture must carry the duplicate TxIn through the production decoder"
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "decodeSet at PV8 is Set.fromList: one input, one Spend redeemer at \
         index 0 matches; got {errors:?}"
    );
}

#[test]
fn pv8_duplicate_script_input_is_accepted_by_apply_block_validate_all() {
    let script = always_succeeds_v2();
    let (ledger, _utxo) = ledger_at(8, &script, 5_000_000);
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(8, &script, 2, 1));
    assert_eq!(tx.body.inputs.len(), 2);
    let (result, warns) = apply_single_tx_block(ledger, tx, Era::Babbage, 8);
    assert!(
        result.is_ok(),
        "ValidateAll must accept the block; got {result:?}"
    );
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 or Phase-2 failure for this tx; got {warns:?}"
    );
}

/// Control: the same tx with the input listed ONCE passes the same way, so the
/// duplicate (not the fixture) is what the tests above exercise.
#[test]
fn pv8_single_script_input_control_is_accepted() {
    let script = always_succeeds_v2();
    let (ledger, utxo) = ledger_at(8, &script, 5_000_000);
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(8, &script, 1, 1));
    assert_eq!(tx.body.inputs.len(), 1);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(errors.is_empty(), "control must pass; got {errors:?}");
}

/// A Spend redeemer at index 1 is out of range once the set collapses to one
/// input: Haskell has no input at `Set.elemAt 1`, so it is an extra redeemer.
#[test]
fn pv8_duplicate_input_does_not_inflate_the_redeemer_index_range() {
    let script = always_succeeds_v2();
    let (ledger, utxo) = ledger_at(8, &script, 5_000_000);
    let mut tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(8, &script, 2, 1));
    tx.witness_set.redeemers[0].index = 1;
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::RedeemerIndexOutOfRange { .. })),
        "index 1 is beyond the one-element input set; got {errors:?}"
    );
}

// ---------------------------------------------------------------------------
// PV8: a duplicate COLLATERAL input is also collapsed. Counting it twice would
// admit a tx whose real collateral is below the required 150% of the fee.
// ---------------------------------------------------------------------------

#[test]
fn pv8_duplicate_collateral_input_is_counted_once() {
    let script = always_succeeds_v2();
    // required = ceil(150% * 300_000) = 450_000. One copy is 300_000 (short);
    // two copies would read 600_000 (enough) if the duplicate were summed.
    let (ledger, utxo) = ledger_at(8, &script, 300_000);
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(8, &script, 1, 2));
    assert_eq!(tx.body.collateral.len(), 2);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::InsufficientCollateral { .. })),
        "collateral is a Set: 300_000 < 450_000 must be InsufficientCollateral; got {errors:?}"
    );
}

#[test]
fn pv8_duplicate_collateral_input_does_not_trip_max_collateral_inputs() {
    let script = always_succeeds_v2();
    let (mut ledger, utxo) = ledger_at(8, &script, 5_000_000);
    ledger.epochs.protocol_params.max_collateral_inputs = 1;
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(8, &script, 1, 2));
    assert_eq!(tx.body.collateral.len(), 2);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, ValidationError::TooManyCollateralInputs { .. })),
        "Set.size of the collateral set is 1; got {errors:?}"
    );
}

// ---------------------------------------------------------------------------
// PV9+ (Conway): the duplicate is rejected, on every path.
// ---------------------------------------------------------------------------

#[test]
fn pv9_wire_decoder_rejects_a_duplicate_input() {
    let script = always_succeeds_v2();
    let cbor = build_tx_cbor(9, &script, 2, 1);
    assert!(
        decode_transaction(CONWAY_ERA_ID, &cbor).is_err(),
        "decodeSetEnforceNoDuplicates: a duplicate input must not decode at PV9+"
    );
    // And the same bytes with the duplicate removed do decode (the rejection is
    // the duplicate, not the fixture).
    let ok = build_tx_cbor(9, &script, 1, 1);
    assert!(decode_transaction(CONWAY_ERA_ID, &ok).is_ok());
}

#[test]
fn pv9_duplicate_input_is_rejected_by_mempool_validation() {
    let script = always_succeeds_v2();
    let (ledger, utxo) = ledger_at(9, &script, 5_000_000);
    // The Babbage decoder keeps the duplicate; PV9 validation must reject it.
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(9, &script, 2, 1));
    assert_eq!(tx.body.inputs.len(), 2);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicateInput(_))),
        "PV9 must reject a duplicate input; got {errors:?}"
    );
}

#[test]
fn pv9_duplicate_input_is_flagged_by_apply_block_validate_all() {
    let script = always_succeeds_v2();
    let (ledger, _utxo) = ledger_at(9, &script, 5_000_000);
    let tx = decode(BABBAGE_ERA_ID, &build_tx_cbor(9, &script, 2, 1));
    let (result, warns) = apply_single_tx_block(ledger, tx, Era::Conway, 9);
    let flagged = result.is_err() || warns.iter().any(|l| l.contains("DuplicateInput"));
    assert!(
        flagged,
        "PV9 apply_block(ValidateAll) must flag the duplicate; result {result:?}, warns {warns:?}"
    );
}

// ---------------------------------------------------------------------------
// The REAL preview tx (block 2,160,646, slot 50,778,243, Babbage, PV8).
//
// Wire CBOR from Koios `tx_cbor`; the three resolved UTxOs from Koios
// `utxo_info`. Body key 0 is the same TxIn twice with no set tag. The script is
// a PlutusV2 reference script (key 18 reference input), so its hash is checked
// against the address credential below.
//
// Only the index-addressed Phase-1 consumers are asserted: the preview PV8
// PlutusV2 cost model is not reproduced here, so script-data-hash and Phase-2
// results are out of scope for this fixture (the synthetic tests above cover
// the clean-pass verdict).
// ---------------------------------------------------------------------------

mod real_preview_tx {
    use super::*;
    use dugite_primitives::transaction::ScriptRef;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/pre_pv9_duplicate_inputs"
    );

    fn fixture(name: &str) -> Vec<u8> {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("fixture");
        hex::decode(text.trim()).expect("hex")
    }

    fn id(hex_id: &str) -> [u8; 32] {
        hex::decode(hex_id).unwrap().try_into().unwrap()
    }

    fn key_addr() -> Address {
        Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(Hash28::from_bytes(
                hex::decode("ff76e52a2bbd75feedecd082d9e87a4b3c2232a048898570c24b7478")
                    .unwrap()
                    .try_into()
                    .unwrap(),
            )),
        })
    }

    fn setup() -> (LedgerState, UtxoSet, Transaction) {
        let script = fixture("ref_script_a094991e_plutus_v2.hex");
        let sh = {
            let mut tagged = vec![0x02u8];
            tagged.extend_from_slice(&script);
            blake2b_224(&tagged)
        };
        assert_eq!(
            sh.to_hex(),
            "a094991ec7080fb73342091a410be074bcd17eb36ea5ddaeb3cb0c56",
            "fixture script must hash to the on-chain credential"
        );
        let datum_cbor = fixture("spent_input_inline_datum.hex");
        let datum = dugite_serialization::decode_plutus_data_cbor(&datum_cbor).expect("datum");

        let spent = TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(sh),
            }),
            value: Value::lovelace(10_000_000),
            datum: OutputDatum::InlineDatum {
                data: datum,
                raw_cbor: Some(datum_cbor),
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let ref_holder = TransactionOutput {
            address: spent.address.clone(),
            value: Value::lovelace(50_000_000),
            datum: OutputDatum::None,
            script_ref: Some(ScriptRef::PlutusV2(script)),
            is_legacy: false,
            raw_cbor: None,
        };
        let collateral = TransactionOutput {
            address: key_addr(),
            value: Value::lovelace(9_820_527),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let utxos = [
            (
                "38242dd3b8a4189fd854c61cdf9cd086b975d459f9dac664b1d0666ba0c0f6d9",
                spent,
            ),
            (
                "432243de9812d5c4d46de2562d25e4f27468d745822888acb471360cbf37295e",
                ref_holder,
            ),
            (
                "4d0f37e9378fecfe701fdd96ca473f6211f8ebb8ab1f35e5bce793702a39e42d",
                collateral,
            ),
        ];

        let params = params_at(8);
        let mut ledger = LedgerState::new(params.clone());
        ledger.epochs.protocol_params = params;
        ledger.epoch_length = 100;
        let mut utxo_set = UtxoSet::new();
        for (tx_id, out) in utxos {
            let input = txin(id(tx_id));
            ledger.utxo.utxo_set.insert(input.clone(), out.clone());
            utxo_set.insert(input, out);
        }

        let tx = decode_transaction(BABBAGE_ERA_ID, &fixture("tx_4c4afd46.hex")).expect("decode");
        assert_eq!(
            tx.hash.to_hex(),
            "4c4afd467a06e93891221141305c9242ed633e0788c9d8eb10fd66a3117e77b6"
        );
        assert_eq!(tx.body.inputs.len(), 2, "wire body lists the TxIn twice");
        assert_eq!(tx.body.inputs[0], tx.body.inputs[1]);
        (ledger, utxo_set, tx)
    }

    fn is_index_error(e: &ValidationError) -> bool {
        matches!(
            e,
            ValidationError::MissingSpendRedeemer { .. }
                | ValidationError::MissingRedeemer { .. }
                | ValidationError::ExtraRedeemer { .. }
                | ValidationError::RedeemerIndexOutOfRange { .. }
        )
    }

    #[test]
    fn real_tx_has_no_redeemer_index_error_on_the_mempool_path() {
        let (ledger, utxo, tx) = setup();
        let errors = mempool_errors(&ledger, &utxo, &tx);
        let bad: Vec<_> = errors.iter().filter(|e| is_index_error(e)).collect();
        assert!(bad.is_empty(), "got {bad:?} (all errors: {errors:?})");
    }

    #[test]
    fn real_tx_has_no_redeemer_index_error_on_the_apply_block_path() {
        let (ledger, _utxo, tx) = setup();
        let (_result, warns) = apply_single_tx_block(ledger, tx, Era::Babbage, 8);
        let bad: Vec<_> = warns.iter().filter(|l| l.contains("edeemer")).collect();
        assert!(bad.is_empty(), "got {bad:?}");
    }
}
