//! A `ParameterChange` whose cost-model vector is shorter than the language's
//! full parameter count is WELL-FORMED, so the Phase-1 validator must accept it.
//!
//! cardano-ledger f649f9751074d2ab3de033fc3912f29c9862c1f5 (the revision
//! cardano-node 11.1.3 builds with):
//!
//! * `eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` L934-962,
//!   `instance ConwayEraPParams ConwayEra`, `ppuWellFormed`: the clause list
//!   is maxBBSize, maxTxSize, maxBHSize, maxValSize, collateralPercentage,
//!   committeeMaxTermLength, govActionLifetime, poolDeposit, govActionDeposit,
//!   dRepDeposit, coinsPerUTxOByte (post-bootstrap), `ppu /= emptyPParamsUpdate`
//!   and (PV >= 11) nOpt. It has no cost-model clause.
//! * `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs` L393-399,
//!   `actionWellFormed`, is the only producer of `MalformedProposal`, and it
//!   calls `ppuWellFormed` for a `ParameterChange`.
//!
//! The tests drive the two production paths: the mempool entry point
//! (`validate_transaction_with_context` over `mempool_validation_context`) and
//! `apply_block(ValidateAll)` with a single-transaction block. The negative
//! control is a `ParameterChange` Haskell DOES reject (`maxTxSize = 0`): it
//! must still raise `MalformedProposal`.

use std::sync::{Arc, Mutex, OnceLock};

use dugite_ledger::state::{BlockValidationMode, LedgerState};
use dugite_ledger::utxo::UtxoSet;
use dugite_ledger::validation::{validate_transaction_with_context, ValidationError};
use dugite_primitives::address::{Address, ByronAddress};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    Anchor, CostModels, GovAction, OutputDatum, ProposalProcedure, ProtocolParamUpdate,
    Transaction, TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet,
};
use dugite_primitives::value::{Lovelace, Value};

const GOV_DEPOSIT: u64 = 1_000_000;
const RETURN_ADDR: [u8; 29] = [0xe0; 29];

// ---------------------------------------------------------------------------
// Capture of the WARN line `apply_block` emits for a Phase-1 failure it
// tolerates. One global subscriber per test binary (apply_block may run work on
// other threads, which a thread-local default would miss); every test filters
// the captured text by its own unique tx hash.
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

fn byron_out(lovelace: u64) -> TransactionOutput {
    TransactionOutput {
        // Byron payload: no witness is required, so nothing fails for an
        // unrelated reason before the proposal predicates.
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

/// A Conway ledger at `pv` whose proposal-return account is registered.
fn ledger_at(pv: u64) -> (LedgerState, UtxoSet) {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = pv;
    params.gov_action_deposit = Lovelace(GOV_DEPOSIT);
    let mut ledger = LedgerState::new(params.clone());
    ledger.epochs.protocol_params = params;
    ledger.epoch_length = 100;
    ledger.certs.reward_accounts.insert(
        LedgerState::reward_account_to_hash(&RETURN_ADDR),
        Lovelace(0),
    );
    ledger
        .utxo
        .utxo_set
        .insert(seed_input(), byron_out(10_000_000));
    let mut utxo_set = UtxoSet::new();
    utxo_set.insert(seed_input(), byron_out(10_000_000));
    (ledger, utxo_set)
}

fn parameter_change_tx(tag: u8, ppu: ProtocolParamUpdate) -> Transaction {
    Transaction {
        era: Era::Conway,
        hash: Hash32::from_bytes([tag; 32]),
        body: TransactionBody {
            inputs: vec![seed_input()],
            // input 10_000_000 = output + fee 200_000 + gov deposit 1_000_000
            outputs: vec![byron_out(8_800_000)],
            fee: Lovelace(200_000),
            proposal_procedures: vec![ProposalProcedure {
                deposit: Lovelace(GOV_DEPOSIT),
                return_addr: RETURN_ADDR.to_vec(),
                gov_action: GovAction::ParameterChange {
                    prev_action_id: None,
                    protocol_param_update: Box::new(ppu),
                    policy_hash: None,
                },
                anchor: Anchor {
                    url: String::new(),
                    data_hash: Hash32::ZERO,
                },
            }],
            ..Default::default()
        },
        witness_set: TransactionWitnessSet {
            vkey_witnesses: Vec::new(),
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

fn short_cost_models() -> ProtocolParamUpdate {
    ProtocolParamUpdate {
        cost_models: Some(CostModels {
            plutus_v1: Some(vec![100; 1]),
            plutus_v2: Some(vec![100; 100]),
            plutus_v3: Some(vec![100; 250]),
            plutus_v4: None,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn zero_max_tx_size() -> ProtocolParamUpdate {
    ProtocolParamUpdate {
        max_tx_size: Some(0),
        ..Default::default()
    }
}

fn block_of(tx: Transaction) -> Block {
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
                major: 11,
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

/// Mempool path: the verdict `validate_transaction_with_context` gives.
fn mempool_errors(ledger: &LedgerState, utxo: &UtxoSet, tx: &Transaction) -> Vec<ValidationError> {
    validate_transaction_with_context(
        tx,
        utxo,
        &ledger.epochs.protocol_params,
        /* current_slot */ 100,
        /* tx_size */ 300,
        None,
        ledger.mempool_validation_context(),
    )
    .err()
    .unwrap_or_default()
}

fn malformed(errors: &[ValidationError]) -> bool {
    errors
        .iter()
        .any(|e| matches!(e, ValidationError::MalformedProposal { .. }))
}

/// Block path: a single-transaction block under `ValidateAll`. Returns the
/// block-apply result rendered as text plus every WARN line that names the tx.
fn apply_single_tx_block(
    mut ledger: LedgerState,
    tx: Transaction,
) -> (Result<(), String>, Vec<String>) {
    let _ = captured();
    let hash = tx.hash;
    let result = ledger
        .apply_block(&block_of(tx), BlockValidationMode::ValidateAll)
        .map_err(|e| e.to_string());
    (result, log_lines_for(&hash))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn pv11_short_cost_models_accepted_by_mempool_validation() {
    let (ledger, utxo) = ledger_at(11);
    let tx = parameter_change_tx(0x31, short_cost_models());
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "ppuWellFormed has no cost-model clause (cardano-ledger f649f975, \
         Conway/PParams.hs L934-962): a short cost model must be accepted; got {errors:?}"
    );
}

#[test]
fn pv11_short_cost_models_accepted_by_apply_block_validate_all() {
    let (ledger, _utxo) = ledger_at(11);
    let tx = parameter_change_tx(0x32, short_cost_models());
    let (result, warns) = apply_single_tx_block(ledger, tx);
    assert!(
        result.is_ok(),
        "ValidateAll must accept the block; got {result:?}"
    );
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 failure for this tx; got {warns:?}"
    );
}

/// Negative control: Haskell DOES reject a zero `maxTxSize` (`ppuWellFormed`
/// `isValid (/= 0) ppuMaxTxSizeL`), so this must keep failing on both paths.
#[test]
fn pv11_zero_max_tx_size_still_malformed_on_both_paths() {
    let (ledger, utxo) = ledger_at(11);
    let tx = parameter_change_tx(0x33, zero_max_tx_size());
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        malformed(&errors),
        "maxTxSize=0 must stay MalformedProposal in the mempool path; got {errors:?}"
    );

    let (result, warns) = apply_single_tx_block(ledger, tx);
    let rejected = result.is_err() || warns.iter().any(|l| l.contains("MalformedProposal"));
    assert!(
        rejected,
        "maxTxSize=0 must still be flagged by apply_block(ValidateAll); \
         result {result:?}, warns {warns:?}"
    );
}

/// Negative control: an EMPTY update is rejected by Haskell
/// (`ppu /= emptyPParamsUpdate`) and must stay rejected.
#[test]
fn pv11_empty_update_still_malformed() {
    let (ledger, utxo) = ledger_at(11);
    let tx = parameter_change_tx(0x34, ProtocolParamUpdate::default());
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        malformed(&errors),
        "an empty ParameterChange must stay MalformedProposal; got {errors:?}"
    );
}

#[test]
fn pv10_short_cost_models_accepted() {
    let (ledger, utxo) = ledger_at(10);
    let tx = parameter_change_tx(0x35, short_cost_models());
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(errors.is_empty(), "PV10 must accept it too; got {errors:?}");
}
