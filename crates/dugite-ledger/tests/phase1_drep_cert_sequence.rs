//! The DRep certificates of ONE transaction are applied left to right, each
//! against the registry the previous one produced.
//!
//! cardano-ledger f649f9751074d2ab3de033fc3912f29c9862c1f5 (the revision
//! cardano-node 11.1.3 builds with):
//!
//! * `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Certs.hs` L242-246: the
//!   `gamma :|> txCert` case runs CERTS on the prefix `gamma` first, then runs
//!   CERT on `txCert` against that result.
//! * `.../Rules/GovCert.hs` L210-219 (`RegDRep`): `Map.notMember cred
//!   (certState ^. certVStateL . vsDRepsL)`; L234-242 (`UnRegDRep`):
//!   `isJust mDRepState ?! ConwayDRepNotRegistered`, refund compared with the
//!   deposit held in that DRep's state.
//! * `.../Rules/Deleg.hs` L215-226: the DRep delegatee check reads `vsDRepsL`
//!   of the same evolving state.
//!
//! So `[RegDRep X, UnRegDRep X]` and `[UnRegDRep X, RegDRep X]` (X already
//! registered) are valid, and dugite used to reject both because it read the
//! pre-tx registry. The negative controls are the sequences Haskell rejects.
//!
//! Every positive case is driven through the mempool validator AND through
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
    Certificate, DRep, OutputDatum, Transaction, TransactionBody, TransactionInput,
    TransactionOutput, TransactionWitnessSet, VKeyWitness,
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
    ledger_with_registered_drep_at(x, 10)
}

/// As [`ledger_with_registered_drep`], at protocol version `pv`.
fn ledger_with_registered_drep_at(
    x: &Signer,
    pv: u64,
) -> (LedgerState, UtxoSet, TransactionInput, u64, Hash32) {
    let (mut ledger, _) = ledger_at(pv);
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

fn is_already(e: &ValidationError) -> bool {
    matches!(e, ValidationError::DRepAlreadyRegistered { .. })
}

fn is_not_registered(e: &ValidationError) -> bool {
    matches!(e, ValidationError::DRepNotRegistered { .. })
}

// ---------------------------------------------------------------------------
// Positive: Haskell accepts these
// ---------------------------------------------------------------------------

/// `[RegDRep X, UnRegDRep X]` for an X that is not registered.
#[test]
fn reg_then_unreg_in_one_tx_is_accepted_by_the_mempool_validator() {
    let x = Signer::new(0x51);
    let (ledger, utxo) = ledger_at(10);
    let tx = tx_with(
        0x21,
        seed_input(),
        10_000_000,
        vec![reg(&x), unreg(&x, DREP_DEPOSIT)],
        0,
        &[&x],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "[RegDRep X, UnRegDRep X] is valid in Haskell (CERTS folds left to right); got {errors:?}"
    );
}

#[test]
fn reg_then_unreg_in_one_tx_is_accepted_by_apply_block() {
    let x = Signer::new(0x52);
    let (mut ledger, _) = ledger_at(10);
    let tx = tx_with(
        0x22,
        seed_input(),
        10_000_000,
        vec![reg(&x), unreg(&x, DREP_DEPOSIT)],
        0,
        &[&x],
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, 1, Hash32::ZERO);
    assert!(result.is_ok(), "ValidateAll must accept it: {result:?}");
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 failure for this tx; got {warns:?}"
    );
    assert!(
        !is_registered(&ledger, &x),
        "the unregistration is the last certificate, so X must end unregistered"
    );
}

/// `[UnRegDRep X, RegDRep X]` for an X that IS registered.
#[test]
fn unreg_then_reg_of_a_registered_drep_is_accepted_by_the_mempool_validator() {
    let x = Signer::new(0x53);
    let (ledger, utxo, out, amount, _) = ledger_with_registered_drep(&x);
    let tx = tx_with(
        0x23,
        out,
        amount,
        vec![unreg(&x, DREP_DEPOSIT), reg(&x)],
        0,
        &[&x],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "[UnRegDRep X, RegDRep X] is valid in Haskell when X is registered; got {errors:?}"
    );
}

#[test]
fn unreg_then_reg_of_a_registered_drep_is_accepted_by_apply_block() {
    let x = Signer::new(0x54);
    let (mut ledger, _utxo, out, amount, prev) = ledger_with_registered_drep(&x);
    let tx = tx_with(
        0x24,
        out,
        amount,
        vec![unreg(&x, DREP_DEPOSIT), reg(&x)],
        0,
        &[&x],
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, 2, prev);
    assert!(result.is_ok(), "ValidateAll must accept it: {result:?}");
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 failure for this tx; got {warns:?}"
    );
    assert!(
        is_registered(&ledger, &x),
        "the registration is the last certificate, so X must end registered"
    );
}

// ---------------------------------------------------------------------------
// Negative controls: Haskell rejects these, dugite must keep rejecting
// ---------------------------------------------------------------------------

/// `[RegDRep X, RegDRep X]`: the second sees X registered.
#[test]
fn double_registration_in_one_tx_is_still_rejected() {
    let x = Signer::new(0x55);
    let (ledger, utxo) = ledger_at(10);
    let tx = tx_with(
        0x25,
        seed_input(),
        10_000_000,
        vec![reg(&x), reg(&x)],
        2 * DREP_DEPOSIT as i64,
        &[&x],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        has(&errors, is_already),
        "[RegDRep X, RegDRep X] must raise DRepAlreadyRegistered; got {errors:?}"
    );
}

/// `[UnRegDRep X]` with X never registered.
#[test]
fn unregistering_a_drep_that_was_never_registered_is_still_rejected() {
    let x = Signer::new(0x56);
    let (ledger, utxo) = ledger_at(10);
    let tx = tx_with(
        0x26,
        seed_input(),
        10_000_000,
        vec![unreg(&x, DREP_DEPOSIT)],
        -(DREP_DEPOSIT as i64),
        &[&x],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        has(&errors, is_not_registered),
        "an unknown DRep must raise DRepNotRegistered; got {errors:?}"
    );
}

/// `[UnRegDRep X, UnRegDRep X]` with X registered: the second sees X gone.
#[test]
fn double_unregistration_in_one_tx_is_still_rejected() {
    let x = Signer::new(0x57);
    let (ledger, utxo, out, amount, _) = ledger_with_registered_drep(&x);
    let tx = tx_with(
        0x27,
        out,
        amount,
        vec![unreg(&x, DREP_DEPOSIT), unreg(&x, DREP_DEPOSIT)],
        -2 * DREP_DEPOSIT as i64,
        &[&x],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert_eq!(
        errors.iter().filter(|e| is_not_registered(e)).count(),
        1,
        "exactly the second unregistration fails; got {errors:?}"
    );
}

/// `[UnRegDRep X, RegDRep X, RegDRep X]` with X registered: the last
/// registration sees the one before it.
#[test]
fn registering_twice_after_an_unregistration_is_still_rejected() {
    let x = Signer::new(0x58);
    let (ledger, utxo, out, amount, _) = ledger_with_registered_drep(&x);
    let tx = tx_with(
        0x28,
        out,
        amount,
        vec![unreg(&x, DREP_DEPOSIT), reg(&x), reg(&x)],
        DREP_DEPOSIT as i64,
        &[&x],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert_eq!(
        errors.iter().filter(|e| is_already(e)).count(),
        1,
        "exactly the second registration fails; got {errors:?}"
    );
}

/// Delegating to a DRep that an EARLIER certificate of the same tx removed:
/// `[RegDRep X, UnRegDRep X, VoteDelegation d -> X]`. The delegatee check
/// reads the evolving registry (Deleg.hs L215-226), so the delegation fails.
#[test]
fn delegating_to_a_drep_unregistered_earlier_in_the_tx_is_still_rejected() {
    let x = Signer::new(0x59);
    let d = Signer::new(0x5A);
    let (mut ledger, utxo) = ledger_at(10);
    ledger
        .certs
        .reward_accounts
        .insert(d.cred().to_typed_hash32(), Lovelace(0));
    let deleg = Certificate::VoteDelegation {
        credential: d.cred(),
        drep: DRep::KeyHash(x.cred().to_typed_hash32()),
    };
    let tx = tx_with(
        0x29,
        seed_input(),
        10_000_000,
        vec![reg(&x), unreg(&x, DREP_DEPOSIT), deleg],
        0,
        &[&x, &d],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        has(&errors, |e| matches!(
            e,
            ValidationError::DelegateeDRepNotRegistered { .. }
        )),
        "a delegation after the DRep was unregistered must raise \
         DelegateeDRepNotRegistered; got {errors:?}"
    );
}

/// The refund of an UnRegDRep that follows an in-tx RegDRep is compared with
/// the deposit that RegDRep declared (GovCert.hs L234-242). The deposit is
/// known from the transaction itself, so the comparison must not depend on
/// `drep_deposits`, which the production contexts do not carry.
fn wrong_refund_tx(x: &Signer, tag: u8, refund: u64) -> Transaction {
    tx_with(
        tag,
        seed_input(),
        10_000_000,
        vec![reg(x), unreg(x, refund)],
        DREP_DEPOSIT as i64 - refund as i64,
        &[x],
    )
}

#[test]
fn wrong_refund_on_an_in_tx_registered_drep_is_rejected_by_the_mempool_validator() {
    let x = Signer::new(0x5B);
    let (ledger, utxo) = ledger_at(10);
    // Production context: `drep_deposits` is None.
    assert!(ledger.mempool_validation_context().drep_deposits.is_none());
    let right = mempool_errors(&ledger, &utxo, &wrong_refund_tx(&x, 0x2A, DREP_DEPOSIT));
    assert!(
        right.is_empty(),
        "matching refund must be accepted; got {right:?}"
    );
    let wrong = mempool_errors(&ledger, &utxo, &wrong_refund_tx(&x, 0x2B, DREP_DEPOSIT + 1));
    assert!(
        has(&wrong, |e| matches!(
            e,
            ValidationError::DRepIncorrectRefund { declared, expected, .. }
                if *declared == DREP_DEPOSIT + 1 && *expected == DREP_DEPOSIT
        )),
        "a refund that differs from the in-tx deposit must raise DRepIncorrectRefund; got {wrong:?}"
    );
}

#[test]
fn wrong_refund_on_an_in_tx_registered_drep_is_flagged_by_apply_block() {
    let x = Signer::new(0x5C);
    let (mut ledger, _) = ledger_at(10);
    let (result, warns) = apply_single_tx_block(
        &mut ledger,
        wrong_refund_tx(&x, 0x2C, DREP_DEPOSIT + 1),
        1,
        Hash32::ZERO,
    );
    let flagged = result.is_err()
        || warns
            .iter()
            .any(|l| l.contains("DRepIncorrectRefund") || l.contains("IncorrectRefund"));
    assert!(
        flagged,
        "ValidateAll must flag the wrong refund; result {result:?}, warns {warns:?}"
    );
}

// ---------------------------------------------------------------------------
// The delegatee check reads the evolving registry (Deleg.hs L215-226)
// ---------------------------------------------------------------------------

fn vote_delegation(d: &Signer, x: &Signer) -> Certificate {
    Certificate::VoteDelegation {
        credential: d.cred(),
        drep: DRep::KeyHash(x.cred().to_typed_hash32()),
    }
}

fn register_delegator(ledger: &mut LedgerState, d: &Signer) {
    ledger
        .certs
        .reward_accounts
        .insert(d.cred().to_typed_hash32(), Lovelace(0));
}

fn is_delegatee_missing(e: &ValidationError) -> bool {
    matches!(e, ValidationError::DelegateeDRepNotRegistered { .. })
}

/// `[RegDRep X, VoteDelegation d -> X]`: X is in `vsDRepsL` when the
/// delegation is checked, so Haskell accepts it. A check that reads only the
/// pre-tx registry rejects it.
#[test]
fn vote_delegation_to_a_drep_registered_earlier_in_the_tx_is_accepted() {
    let x = Signer::new(0x5D);
    let d = Signer::new(0x5E);
    let (mut ledger, utxo) = ledger_at(10);
    register_delegator(&mut ledger, &d);
    let tx = tx_with(
        0x2D,
        seed_input(),
        10_000_000,
        vec![reg(&x), vote_delegation(&d, &x)],
        DEP_I64,
        &[&x, &d],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "[RegDRep X, VoteDelegation->X] is valid in Haskell; mempool got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, 1, Hash32::ZERO);
    assert!(result.is_ok(), "ValidateAll must accept it: {result:?}");
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 failure for this tx; got {warns:?}"
    );
}

const DEP_I64: i64 = DREP_DEPOSIT as i64;

/// `[UnRegDRep X, VoteDelegation d -> X]` with X registered before the tx:
/// X is gone when the delegation is checked, so a PV10 node rejects it with
/// `DelegateeDRepNotRegisteredDELEG`. A check that reads only the pre-tx
/// registry accepts it.
#[test]
fn vote_delegation_to_a_drep_unregistered_earlier_in_the_tx_is_rejected_at_pv10() {
    let x = Signer::new(0x5F);
    let d = Signer::new(0x60);
    let (mut ledger, utxo, out, amount, prev) = ledger_with_registered_drep(&x);
    register_delegator(&mut ledger, &d);
    let tx = tx_with(
        0x2E,
        out,
        amount,
        vec![unreg(&x, DREP_DEPOSIT), vote_delegation(&d, &x)],
        -DEP_I64,
        &[&x, &d],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        has(&errors, is_delegatee_missing),
        "PV10 must raise DelegateeDRepNotRegistered; mempool got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, 2, prev);
    assert!(
        result.is_err()
            || warns
                .iter()
                .any(|l| l.contains("DelegateeDRepNotRegistered")),
        "ValidateAll must flag it; result {result:?}, warns {warns:?}"
    );
}

/// The same sequence at PV9 (Conway bootstrap): `checkDRepRegistered` is
/// skipped (`unless (hardforkConwayBootstrapPhase pv)`), so the tx is valid.
#[test]
fn the_same_vote_delegation_is_accepted_at_pv9() {
    let x = Signer::new(0x5F);
    let d = Signer::new(0x60);
    let (mut ledger, utxo, out, amount, prev) = ledger_with_registered_drep_at(&x, 9);
    register_delegator(&mut ledger, &d);
    let tx = tx_with(
        0x2F,
        out,
        amount,
        vec![unreg(&x, DREP_DEPOSIT), vote_delegation(&d, &x)],
        -DEP_I64,
        &[&x, &d],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "PV9 skips the delegatee check; mempool got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx, 2, prev);
    assert!(result.is_ok(), "ValidateAll must accept it: {result:?}");
    assert!(
        warns.is_empty(),
        "apply_block must not log a Phase-1 failure for this tx; got {warns:?}"
    );
}
