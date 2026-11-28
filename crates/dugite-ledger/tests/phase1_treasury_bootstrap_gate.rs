//! `TreasuryWithdrawalReturnAccountsDoNotExist` is skipped in the Conway
//! bootstrap phase (PV9).
//!
//! cardano-ledger f649f9751074d2ab3de033fc3912f29c9862c1f5:
//!
//! * `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs` L256-258:
//!   `hardforkConwayBootstrapPhase pv = pvMajor pv == natVersion @9`.
//! * `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs` L504-520: the
//!   account checks of `processProposal`, `ProposalReturnAccountDoesNotExist`
//!   and `TreasuryWithdrawalReturnAccountsDoNotExist`, both sit under
//!   `unless (hardforkConwayBootstrapPhase $ pp ^. ppProtocolVersionL)`.
//! * Gov.hs L440-442 and L633-639: at PV9 a `TreasuryWithdrawals` proposal is
//!   rejected with `DisallowedProposalDuringBootstrap` (only `ParameterChange`,
//!   `HardForkInitiation` and `InfoAction` are allowed). So the verdict for
//!   such a tx is "reject" at both PVs, but the PV9 failure list must not
//!   carry the account failure. The PV9 failure list is pinned upstream in
//!   `testlib/.../Imp/GovSpec.hs` L1078-1082 and L1104-1105.
//!
//! Both production paths are driven: the mempool validator and
//! `apply_block(ValidateAll)` with a single-tx block.

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
    Anchor, Certificate, GovAction, GovActionId, OutputDatum, ProposalProcedure, Transaction,
    TransactionBody, TransactionInput, TransactionOutput, TransactionWitnessSet, VKeyWitness, Vote,
    Voter, VotingProcedure,
};
use dugite_primitives::value::{Lovelace, Value};

const GOV_DEPOSIT: u64 = 1_000_000;
const KEY_DEPOSIT: u64 = 2_000_000;
const DREP_DEPOSIT: u64 = 3_000_000;
const POOL_DEPOSIT: u64 = 4_000_000;
const FEE: u64 = 200_000;
const INPUT_AMOUNT: u64 = 100_000_000;

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
    fn key(&self) -> Hash32 {
        self.cred().to_typed_hash32()
    }
    /// Reward address (`0xe0` header, key credential) of this signer's stake key.
    fn reward_addr(&self) -> Vec<u8> {
        let mut a = vec![0xe0];
        a.extend_from_slice(self.sk.verification_key().hash().as_bytes());
        a
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

/// A Conway ledger at `pv` plus the same UTxO for the mempool path.
fn ledger_at(pv: u64) -> (LedgerState, UtxoSet) {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = pv;
    params.gov_action_deposit = Lovelace(GOV_DEPOSIT);
    params.key_deposit = Lovelace(KEY_DEPOSIT);
    params.drep_deposit = Lovelace(DREP_DEPOSIT);
    params.pool_deposit = Lovelace(POOL_DEPOSIT);
    let mut ledger = LedgerState::new(params.clone());
    ledger.epochs.protocol_params = params;
    ledger.epoch_length = 100;
    ledger
        .utxo
        .utxo_set
        .insert(seed_input(), byron_out(INPUT_AMOUNT));
    let mut utxo_set = UtxoSet::new();
    utxo_set.insert(seed_input(), byron_out(INPUT_AMOUNT));
    (ledger, utxo_set)
}

/// Register `s`'s stake credential in the ledger as it was before the tx.
fn pre_register_account(ledger: &mut LedgerState, s: &Signer) {
    ledger.certs.reward_accounts.insert(s.key(), Lovelace(0));
}

/// One transaction: certificates, proposals, votes on proposal #0 of the same
/// tx, witnessed by every signer. `net_deposit` is deposits minus refunds.
fn gov_tx(
    tag: u8,
    certs: Vec<Certificate>,
    proposals: Vec<ProposalProcedure>,
    voters: Vec<Voter>,
    net_deposit: u64,
    signers: &[&Signer],
) -> Transaction {
    let hash = Hash32::from_bytes([tag; 32]);
    let total_deposit = net_deposit + proposals.len() as u64 * GOV_DEPOSIT;
    let action = GovActionId {
        transaction_id: hash,
        action_index: 0,
    };
    let voting_procedures = voters
        .into_iter()
        .map(|v| {
            (
                v,
                std::iter::once((
                    action.clone(),
                    VotingProcedure {
                        vote: Vote::Yes,
                        anchor: None,
                    },
                ))
                .collect(),
            )
        })
        .collect();
    Transaction {
        era: Era::Conway,
        hash,
        body: TransactionBody {
            inputs: vec![seed_input()],
            outputs: vec![byron_out(INPUT_AMOUNT - FEE - total_deposit)],
            fee: Lovelace(FEE),
            certificates: certs,
            proposal_procedures: proposals,
            voting_procedures,
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

fn proposal(return_addr: Vec<u8>, gov_action: GovAction) -> ProposalProcedure {
    ProposalProcedure {
        deposit: Lovelace(GOV_DEPOSIT),
        return_addr,
        gov_action,
        anchor: Anchor {
            url: String::new(),
            data_hash: Hash32::ZERO,
        },
    }
}

fn info_proposal(return_to: &Signer) -> ProposalProcedure {
    proposal(return_to.reward_addr(), GovAction::InfoAction)
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

fn apply_single_tx_block(
    ledger: &mut LedgerState,
    tx: Transaction,
) -> (Result<(), String>, Vec<String>) {
    let _ = captured();
    let hash = tx.hash;
    let result = ledger
        .apply_block(&block_of(tx), BlockValidationMode::ValidateAll)
        .map_err(|e| e.to_string());
    (result, log_lines_for(&hash))
}

fn treasury_proposal(return_to: &Signer, destination: &Signer) -> ProposalProcedure {
    proposal(
        return_to.reward_addr(),
        GovAction::TreasuryWithdrawals {
            withdrawals: std::iter::once((destination.reward_addr(), Lovelace(1_000))).collect(),
            policy_hash: None,
        },
    )
}

fn names(errors: &[ValidationError]) -> Vec<String> {
    errors
        .iter()
        .map(|e| {
            let s = format!("{e:?}");
            s.split([' ', '{', '(']).next().unwrap_or("").to_owned()
        })
        .collect()
}

fn is_treasury_dest_missing(e: &ValidationError) -> bool {
    matches!(
        e,
        ValidationError::TreasuryWithdrawalReturnAccountsDoNotExist { .. }
    )
}

fn is_bootstrap_disallowed(e: &ValidationError) -> bool {
    matches!(e, ValidationError::DisallowedProposalDuringBootstrap { .. })
}

/// A treasury withdrawal to an unregistered account, return account registered
/// by the caller.
fn unregistered_destination_tx(tag: u8) -> (Transaction, Signer, Signer) {
    let dest = Signer::new(0x91);
    let ret = Signer::new(0x92);
    let tx = gov_tx(
        tag,
        vec![],
        vec![treasury_proposal(&ret, &dest)],
        vec![],
        0,
        &[],
    );
    (tx, dest, ret)
}

/// PV9: Haskell rejects the proposal with `DisallowedProposalDuringBootstrap`
/// and never evaluates the account checks.
#[test]
fn pv9_mempool_reports_bootstrap_rejection_without_the_account_failure() {
    let (tx, _dest, ret) = unregistered_destination_tx(0x61);
    let (mut ledger, utxo) = ledger_at(9);
    pre_register_account(&mut ledger, &ret);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    // Haskell's PV9 failure list for this tx is exactly the bootstrap
    // rejection (GovSpec.hs L1078-1082).
    assert_eq!(
        names(&errors),
        vec!["DisallowedProposalDuringBootstrap".to_owned()],
        "the whole PV9 failure list must be the bootstrap rejection"
    );
}

#[test]
fn pv9_apply_block_reports_bootstrap_rejection_without_the_account_failure() {
    let (tx, _dest, ret) = unregistered_destination_tx(0x62);
    let (mut ledger, _utxo) = ledger_at(9);
    pre_register_account(&mut ledger, &ret);
    let tx_hex = tx.hash.to_hex();
    let (result, warns) = apply_single_tx_block(&mut ledger, tx);
    // A Phase-1 failure rejects the block; the rejection lists every failure
    // of the tx, joined by "; ": exactly one, the bootstrap rejection.
    let err = result.expect_err("the PV9 bootstrap restriction must reject the block");
    let listed = err
        .split_once(&format!("{tx_hex}: "))
        .map(|(_, l)| l)
        .unwrap_or_else(|| panic!("the rejection must name the tx; got {err}"));
    assert!(
        listed.starts_with("DisallowedProposalDuringBootstrap") && !listed.contains("; "),
        "the whole PV9 failure list must be the bootstrap rejection; got {listed}"
    );
    assert!(
        !err.contains("TreasuryWithdrawalReturnAccountsDoNotExist")
            && !warns
                .join("\n")
                .contains("TreasuryWithdrawalReturnAccountsDoNotExist"),
        "the account check is skipped at PV9; got {err}, warns {warns:?}"
    );
}

/// PV10: the check applies (control).
#[test]
fn pv10_unregistered_treasury_destination_is_still_rejected_on_both_paths() {
    let (tx, _dest, ret) = unregistered_destination_tx(0x63);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &ret);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.iter().any(is_treasury_dest_missing),
        "PV10 must raise TreasuryWithdrawalReturnAccountsDoNotExist; got {:?}",
        names(&errors)
    );
    assert!(
        !errors.iter().any(is_bootstrap_disallowed),
        "no bootstrap restriction at PV10; got {:?}",
        names(&errors)
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx);
    assert!(
        result.is_err()
            || warns
                .iter()
                .any(|l| l.contains("TreasuryWithdrawalReturnAccountsDoNotExist")),
        "ValidateAll must flag it at PV10; result {result:?}, warns {warns:?}"
    );
}

/// PV10: a registered destination is accepted (control).
#[test]
fn pv10_registered_treasury_destination_is_accepted() {
    let (tx, dest, ret) = unregistered_destination_tx(0x64);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &ret);
    pre_register_account(&mut ledger, &dest);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        !errors.iter().any(is_treasury_dest_missing),
        "a registered destination must pass; got {:?}",
        names(&errors)
    );
}

/// PV9 control: an InfoAction with an unregistered return account is accepted
/// (both account checks are skipped), the reachable bootstrap case.
#[test]
fn pv9_info_action_with_unregistered_return_account_is_accepted() {
    let ret = Signer::new(0x93);
    let tx = gov_tx(0x65, vec![], vec![info_proposal(&ret)], vec![], 0, &[]);
    let (ledger, utxo) = ledger_at(9);
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, ValidationError::ProposalReturnAccountDoesNotExist { .. }))
            && !errors.iter().any(is_treasury_dest_missing),
        "PV9 skips the account checks; got {:?}",
        names(&errors)
    );
}
