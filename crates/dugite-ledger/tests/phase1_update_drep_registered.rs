//! `UpdateDRep` requires the DRep to be registered at that point of the
//! transaction's certificate walk.
//!
//! cardano-ledger f649f9751074d2ab3de033fc3912f29c9862c1f5,
//! `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/GovCert.hs` L256-258:
//! `Map.member cred (certState ^. certVStateL . vsDRepsL) ?!
//! (injectFailure . ConwayDRepNotRegistered) cred`, with no protocol-version
//! gate. The state is the one the earlier certificates of the same tx left
//! (`Rules/Certs.hs` L242-246), so `[RegDRep X, UpdateDRep X]` is valid and
//! `[UnRegDRep X, UpdateDRep X]` is not.
//!
//! Every case is driven through the mempool validator AND
//! `apply_block(ValidateAll)` with a single-transaction block.

use std::sync::{Arc, Mutex, OnceLock};

use dugite_ledger::state::{BlockValidationMode, LedgerState};
use dugite_ledger::utxo::UtxoSet;
use dugite_ledger::validation::{validate_transaction_with_context, ValidationError};
use dugite_primitives::address::{Address, ByronAddress};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    Certificate, OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet, VKeyWitness,
};
use dugite_primitives::value::{Lovelace, Value};

const DREP_DEPOSIT: u64 = 2_000_000;
const FEE: u64 = 200_000;

// ---------------------------------------------------------------------------
// WARN capture (see phase1_ppu_cost_models.rs for the rationale)
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

/// A signing identity: its credential and a witness maker.
struct Signer {
    sk: dugite_crypto::keys::PaymentSigningKey,
}

impl Signer {
    fn new(seed: u8) -> Self {
        Self {
            sk: dugite_crypto::keys::PaymentSigningKey::from_bytes(&[seed; 32]).unwrap(),
        }
    }
    fn cred(&self) -> Credential {
        Credential::VerificationKey(self.sk.verification_key().hash())
    }
    fn witness(&self, tx_hash: &Hash32) -> VKeyWitness {
        VKeyWitness {
            vkey: self.sk.verification_key().to_bytes().to_vec(),
            signature: self.sk.sign(tx_hash.as_bytes()),
        }
    }
}

fn byron_out(lovelace: u64) -> TransactionOutput {
    TransactionOutput {
        // Byron payload: no witness is required for the input.
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

fn seed_input() -> TransactionInput {
    TransactionInput {
        transaction_id: Hash32::from_bytes([0xAA; 32]),
        index: 0,
    }
}

fn ledger_at(pv: u64) -> (LedgerState, UtxoSet) {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = pv;
    params.drep_deposit = Lovelace(DREP_DEPOSIT);
    let mut ledger = LedgerState::new(params.clone());
    ledger.epochs.protocol_params = params;
    ledger.epoch_length = 100;
    ledger
        .utxo
        .utxo_set
        .insert(seed_input(), byron_out(10_000_000));
    let mut utxo_set = UtxoSet::new();
    utxo_set.insert(seed_input(), byron_out(10_000_000));
    (ledger, utxo_set)
}

/// A transaction spending `input` (worth `in_amount`) with `certs`, whose
/// certificate deposit/refund balance is `net_deposit` (deposits minus
/// refunds), witnessed by every signer.
fn tx_with(
    tag: u8,
    input: TransactionInput,
    in_amount: u64,
    certs: Vec<Certificate>,
    net_deposit: i64,
    signers: &[&Signer],
) -> Transaction {
    let out = (in_amount as i64 - FEE as i64 - net_deposit) as u64;
    let hash = Hash32::from_bytes([tag; 32]);
    Transaction {
        era: Era::Conway,
        hash,
        body: TransactionBody {
            inputs: vec![input],
            outputs: vec![byron_out(out)],
            fee: Lovelace(FEE),
            certificates: certs,
            ..Default::default()
        },
        witness_set: TransactionWitnessSet {
            vkey_witnesses: signers.iter().map(|s| s.witness(&hash)).collect(),
            native_scripts: Vec::new(),
            bootstrap_witnesses: Vec::new(),
            plutus_v1_scripts: Vec::new(),
            plutus_v2_scripts: Vec::new(),
            plutus_v3_scripts: Vec::new(),
            plutus_data: Vec::new(),
            redeemers: Vec::new(),
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            original_script_data_hash: None,
        },
        is_valid: true,
        auxiliary_data: None,
        raw_cbor: None,
        raw_body_cbor: None,
        raw_witness_cbor: None,
    }
}

fn output_of(tx: &Transaction) -> (TransactionInput, u64) {
    (
        TransactionInput {
            transaction_id: tx.hash,
            index: 0,
        },
        tx.body.outputs[0].value.coin.0,
    )
}

fn reg(x: &Signer) -> Certificate {
    Certificate::RegDRep {
        credential: x.cred(),
        deposit: Lovelace(DREP_DEPOSIT),
        anchor: None,
    }
}

fn unreg(x: &Signer, refund: u64) -> Certificate {
    Certificate::UnregDRep {
        credential: x.cred(),
        refund: Lovelace(refund),
    }
}

fn block_of(tx: Transaction, block_no: u64, prev: Hash32) -> Block {
    Block {
        header: BlockHeader {
            header_hash: Hash32::from_bytes([block_no as u8; 32]),
            prev_hash: prev,
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
            block_number: BlockNo(block_no),
            slot: SlotNo(block_no),
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
                major: 10,
                minor: 0,
            },
            kes_signature: vec![],
        },
        transactions: vec![tx],
        era: Era::Conway,
        raw_cbor: None,
        byron: None,
    }
}

fn mempool_errors(ledger: &LedgerState, utxo: &UtxoSet, tx: &Transaction) -> Vec<ValidationError> {
    validate_transaction_with_context(
        tx,
        utxo,
        &ledger.epochs.protocol_params,
        100,
        300,
        None,
        ledger.mempool_validation_context(),
    )
    .err()
    .unwrap_or_default()
}

/// `apply_block(ValidateAll)` on a one-tx block: the result as text and every
/// WARN line naming the tx.
fn apply_single_tx_block(
    ledger: &mut LedgerState,
    tx: Transaction,
    block_no: u64,
    prev: Hash32,
) -> (Result<(), String>, Vec<String>) {
    let _ = captured();
    let hash = tx.hash;
    let result = ledger
        .apply_block(
            &block_of(tx, block_no, prev),
            BlockValidationMode::ValidateAll,
        )
        .map_err(|e| e.to_string());
    (result, log_lines_for(&hash))
}

fn is_registered(ledger: &LedgerState, x: &Signer) -> bool {
    ledger
        .gov
        .governance
        .dreps
        .contains_key(&x.cred().to_typed_hash32())
}

/// Registers `x` in block 1 and returns the ledger, the mempool-side UTxO set,
/// the output the registration left behind, and block 1's header hash.
fn ledger_with_registered_drep(
    x: &Signer,
) -> (LedgerState, UtxoSet, TransactionInput, u64, Hash32) {
    let (mut ledger, _) = ledger_at(10);
    let tx1 = tx_with(
        0x11,
        seed_input(),
        10_000_000,
        vec![reg(x)],
        DREP_DEPOSIT as i64,
        &[x],
    );
    let (out, out_amount) = output_of(&tx1);
    let (result, warns) = apply_single_tx_block(&mut ledger, tx1, 1, Hash32::ZERO);
    assert!(result.is_ok(), "setup RegDRep block must apply: {result:?}");
    assert!(warns.is_empty(), "setup RegDRep must be clean: {warns:?}");
    assert!(is_registered(&ledger, x), "setup must register the DRep");
    let mut utxo = UtxoSet::new();
    utxo.insert(out.clone(), byron_out(out_amount));
    (ledger, utxo, out, out_amount, Hash32::from_bytes([1u8; 32]))
}

fn has(errors: &[ValidationError], f: impl Fn(&ValidationError) -> bool) -> bool {
    errors.iter().any(f)
}

fn is_not_registered(e: &ValidationError) -> bool {
    matches!(e, ValidationError::DRepNotRegistered { .. })
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

fn update(x: &Signer) -> Certificate {
    Certificate::UpdateDRep {
        credential: x.cred(),
        anchor: None,
    }
}

/// A fresh ledger (DRep not registered): input, amount, block number, parent.
fn fresh(pv: u64) -> (LedgerState, UtxoSet, TransactionInput, u64, u64, Hash32) {
    let (ledger, utxo) = ledger_at(pv);
    (ledger, utxo, seed_input(), 10_000_000, 1, Hash32::ZERO)
}

/// The ledger with `x` registered by block 1.
fn registered(x: &Signer) -> (LedgerState, UtxoSet, TransactionInput, u64, u64, Hash32) {
    let (ledger, utxo, out, amount, prev) = ledger_with_registered_drep(x);
    (ledger, utxo, out, amount, 2, prev)
}

type Setup = (LedgerState, UtxoSet, TransactionInput, u64, u64, Hash32);

/// Both production paths must accept the tx.
fn assert_accepted(setup: Setup, tag: u8, certs: Vec<Certificate>, net: i64, x: &Signer) {
    let (mut ledger, utxo, input, amount, block_no, prev) = setup;
    let tx = tx_with(tag, input, amount, certs, net, &[x]);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "Haskell accepts this tx; mempool got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, block_no, prev);
    assert!(result.is_ok(), "ValidateAll must accept it: {result:?}");
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 failure for this tx; got {warns:?}"
    );
}

/// Both production paths must flag `ConwayDRepNotRegistered`.
fn assert_not_registered(setup: Setup, tag: u8, certs: Vec<Certificate>, net: i64, x: &Signer) {
    let (mut ledger, utxo, input, amount, block_no, prev) = setup;
    let tx = tx_with(tag, input, amount, certs, net, &[x]);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        has(&errors, is_not_registered),
        "Haskell rejects this tx with ConwayDRepNotRegistered; mempool got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, block_no, prev);
    assert!(
        result.is_err() || warns.iter().any(|l| l.contains("ConwayDRepNotRegistered")),
        "ValidateAll must flag it; result {result:?}, warns {warns:?}"
    );
}

const DEP: i64 = DREP_DEPOSIT as i64;

// ---- Haskell accepts ------------------------------------------------------

#[test]
fn update_of_a_registered_drep_is_accepted() {
    let x = Signer::new(0xA1);
    assert_accepted(registered(&x), 0x31, vec![update(&x)], 0, &x);
}

/// `[RegDRep X, UpdateDRep X]`: valid only because of the earlier certificate.
#[test]
fn update_after_an_in_tx_registration_is_accepted() {
    let x = Signer::new(0xA2);
    assert_accepted(fresh(10), 0x32, vec![reg(&x), update(&x)], DEP, &x);
}

/// `[UnRegDRep X, RegDRep X, UpdateDRep X]` with X registered.
#[test]
fn update_after_unregister_and_reregister_is_accepted() {
    let x = Signer::new(0xA3);
    assert_accepted(
        registered(&x),
        0x33,
        vec![unreg(&x, DREP_DEPOSIT), reg(&x), update(&x)],
        0,
        &x,
    );
}

/// `[UpdateDRep X, UnRegDRep X]` with X registered: the update comes first.
#[test]
fn update_before_unregistration_is_accepted() {
    let x = Signer::new(0xA4);
    assert_accepted(
        registered(&x),
        0x34,
        vec![update(&x), unreg(&x, DREP_DEPOSIT)],
        -DEP,
        &x,
    );
}

// ---- Haskell rejects ------------------------------------------------------

/// `[UpdateDRep X]` with X never registered.
#[test]
fn update_of_a_drep_that_was_never_registered_is_rejected() {
    // One tx tag (so one tx hash) per protocol version: the WARN buffer is
    // process-global and filtered by tx hash, so a shared tag would let the
    // PV9 log line satisfy the PV10 and PV11 block-path assertions.
    for (pv, tag) in [(9, 0x3A), (10, 0x3B), (11, 0x3C)] {
        let x = Signer::new(0xA5);
        assert_not_registered(fresh(pv), tag, vec![update(&x)], 0, &x);
    }
}

/// `[UnRegDRep X, UpdateDRep X]` with X registered: invalid only because of
/// the earlier certificate.
#[test]
fn update_after_an_in_tx_unregistration_is_rejected() {
    let x = Signer::new(0xA6);
    assert_not_registered(
        registered(&x),
        0x36,
        vec![unreg(&x, DREP_DEPOSIT), update(&x)],
        -DEP,
        &x,
    );
}

/// `[UpdateDRep X, RegDRep X]` with X new: the update comes first.
#[test]
fn update_before_an_in_tx_registration_is_rejected() {
    let x = Signer::new(0xA7);
    assert_not_registered(fresh(10), 0x37, vec![update(&x), reg(&x)], DEP, &x);
}
