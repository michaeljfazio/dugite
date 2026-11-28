//! The GOV predicates read the registries AFTER the transaction's own
//! certificates, not before.
//!
//! cardano-ledger f649f9751074d2ab3de033fc3912f29c9862c1f5 (the revision
//! cardano-node 11.1.3 builds with):
//!
//! * `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Ledger.hs` L394-421:
//!   `certStateAfterCERTS <- trans @(EraRule "CERTS" era) ...` and then
//!   `GovEnv ... certStateAfterCERTS ...` is the environment of GOV.
//! * `.../Rules/Gov.hs` L467-474: `knownDReps = vsDReps certVState`,
//!   `knownStakePools = psStakePools certPState`, `knownCommitteeMembers =
//!   authorizedHotCommitteeCredentials committeeState`; L504-519 read
//!   `certDState ^. accountsL` for `ProposalReturnAccountDoesNotExist` and
//!   `TreasuryWithdrawalReturnAccountsDoNotExist`; L593-604 is
//!   `VotersDoNotExist`; L478-481 is `UnelectedCommitteeVoters` (PV >= 11).
//!
//! So `[RegDRep X] + vote by X`, `[stake registration c] + a proposal returning
//! to c`, `[CommitteeHotAuth cold hot] + vote by hot` and `[RegPool p] + vote
//! by p` are all valid in one transaction, and dugite used to reject each with
//! the GOV predicate named above. The negative controls are the transactions
//! Haskell rejects: a voter or return account that is not registered after
//! CERTS, including one an earlier certificate of the same transaction removed.
//!
//! Every positive case is driven through the mempool validator AND through
//! `apply_block(ValidateAll)` with a single-transaction block.

use std::sync::{Arc, Mutex, OnceLock};

use dugite_ledger::state::{BlockValidationMode, LedgerState, ProposalState};
use dugite_ledger::utxo::UtxoSet;
use dugite_ledger::validation::{validate_transaction_with_context, ValidationError};
use dugite_primitives::address::{Address, ByronAddress};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
use dugite_primitives::transaction::{
    Anchor, Certificate, GovAction, GovActionId, OutputDatum, PoolParams, ProposalProcedure,
    Rational, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet, VKeyWitness, Vote, Voter, VotingProcedure,
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

fn reg_stake(s: &Signer) -> Certificate {
    Certificate::ConwayStakeRegistration {
        credential: s.cred(),
        deposit: Lovelace(KEY_DEPOSIT),
    }
}

fn dereg_stake(s: &Signer) -> Certificate {
    Certificate::ConwayStakeDeregistration {
        credential: s.cred(),
        refund: Lovelace(KEY_DEPOSIT),
    }
}

fn reg_drep(s: &Signer) -> Certificate {
    Certificate::RegDRep {
        credential: s.cred(),
        deposit: Lovelace(DREP_DEPOSIT),
        anchor: None,
    }
}

fn unreg_drep(s: &Signer) -> Certificate {
    Certificate::UnregDRep {
        credential: s.cred(),
        refund: Lovelace(DREP_DEPOSIT),
    }
}

fn reg_pool(operator: &Signer) -> Certificate {
    let hash = operator.sk.verification_key().hash();
    Certificate::PoolRegistration(PoolParams {
        operator: hash,
        vrf_keyhash: Hash32::from_bytes([0x77; 32]),
        pledge: Lovelace(0),
        cost: ProtocolParameters::mainnet_defaults().min_pool_cost,
        margin: Rational {
            numerator: 0,
            denominator: 1,
        },
        reward_account: operator.reward_addr(),
        pool_owners: vec![hash],
        relays: vec![],
        pool_metadata: None,
    })
}

fn drep_voter(s: &Signer) -> Voter {
    Voter::DRep(s.cred())
}

fn cc_voter(hot: &Signer) -> Voter {
    Voter::ConstitutionalCommittee(hot.cred())
}

fn pool_voter(operator: &Signer) -> Voter {
    Voter::StakePool(operator.sk.verification_key().hash().to_hash32_padded())
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

/// Both production paths must accept the tx with no error and no Phase-1 warning.
fn assert_accepted_on_both_paths(ledger: LedgerState, utxo: UtxoSet, tx: Transaction) {
    let mut ledger = ledger;
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.is_empty(),
        "Haskell accepts this tx (GOV reads certStateAfterCERTS); mempool raised {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx);
    assert!(
        result.is_ok() && warns.is_empty(),
        "apply_block(ValidateAll) must accept it with no Phase-1 warning; result {result:?}, warnings {warns:?}"
    );
}

/// Both production paths must raise the GOV-class failure `pred`.
fn assert_rejected_on_both_paths(
    ledger: LedgerState,
    utxo: UtxoSet,
    tx: Transaction,
    pred: impl Fn(&ValidationError) -> bool,
    name: &str,
) {
    let mut ledger = ledger;
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.iter().any(&pred),
        "Haskell rejects this tx with {name}; mempool got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx);
    assert!(
        result.is_err() || warns.iter().any(|l| l.contains(name)),
        "apply_block(ValidateAll) must flag {name}; result {result:?}, warns {warns:?}"
    );
}

fn is_voters_missing(e: &ValidationError) -> bool {
    matches!(e, ValidationError::VotersDoNotExist { .. })
}
fn is_return_account_missing(e: &ValidationError) -> bool {
    matches!(e, ValidationError::ProposalReturnAccountDoesNotExist { .. })
}
fn is_treasury_dest_missing(e: &ValidationError) -> bool {
    matches!(
        e,
        ValidationError::TreasuryWithdrawalReturnAccountsDoNotExist { .. }
    )
}
fn is_unelected(e: &ValidationError) -> bool {
    matches!(e, ValidationError::UnelectedCommitteeVoters { .. })
}

// ---------------------------------------------------------------------------
// Positive: Haskell accepts these
// ---------------------------------------------------------------------------

/// `ConwayStakeRegistration c` + an InfoAction whose return account is `c`.
#[test]
fn stake_registration_then_proposal_returning_to_it_is_accepted() {
    let c = Signer::new(0x61);
    let (ledger, utxo) = ledger_at(10);
    let tx = gov_tx(
        0x41,
        vec![reg_stake(&c)],
        vec![info_proposal(&c)],
        vec![],
        KEY_DEPOSIT,
        &[&c],
    );
    assert_accepted_on_both_paths(ledger, utxo, tx);
}

/// `RegDRep X` + a vote by X.
#[test]
fn drep_registration_then_vote_by_it_is_accepted() {
    let x = Signer::new(0x62);
    let r = Signer::new(0x63);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let tx = gov_tx(
        0x42,
        vec![reg_drep(&x)],
        vec![info_proposal(&r)],
        vec![drep_voter(&x)],
        DREP_DEPOSIT,
        &[&x],
    );
    assert_accepted_on_both_paths(ledger, utxo, tx);
}

/// `RegPool p` + a vote by pool p.
#[test]
fn pool_registration_then_vote_by_the_pool_is_accepted() {
    let p = Signer::new(0x64);
    let r = Signer::new(0x65);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let tx = gov_tx(
        0x43,
        vec![reg_pool(&p)],
        vec![info_proposal(&r)],
        vec![pool_voter(&p)],
        POOL_DEPOSIT,
        &[&p],
    );
    assert_accepted_on_both_paths(ledger, utxo, tx);
}

/// `CommitteeHotAuth cold hot` + a vote by `hot`; `cold` sits on the enacted
/// committee.
#[test]
fn committee_hot_auth_then_vote_by_the_hot_key_is_accepted() {
    for pv in [10, 11] {
        let cold = Signer::new(0x66);
        let hot = Signer::new(0x67);
        let r = Signer::new(0x68);
        let (mut ledger, utxo) = ledger_at(pv);
        pre_register_account(&mut ledger, &r);
        Arc::make_mut(&mut ledger.gov.governance)
            .committee_expiration
            .insert(cold.key(), EpochNo(500));
        let tx = gov_tx(
            0x44,
            vec![Certificate::CommitteeHotAuth {
                cold_credential: cold.cred(),
                hot_credential: hot.cred(),
            }],
            vec![info_proposal(&r)],
            vec![cc_voter(&hot)],
            0,
            &[&cold, &hot],
        );
        assert_accepted_on_both_paths(ledger, utxo, tx);
    }
}

/// `ConwayStakeRegistration c` + a TreasuryWithdrawals paying `c`.
#[test]
fn stake_registration_then_treasury_withdrawal_to_it_is_accepted() {
    let c = Signer::new(0x69);
    let r = Signer::new(0x6A);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let withdrawal = proposal(
        r.reward_addr(),
        GovAction::TreasuryWithdrawals {
            withdrawals: std::iter::once((c.reward_addr(), Lovelace(1_000))).collect(),
            policy_hash: None,
        },
    );
    let tx = gov_tx(
        0x45,
        vec![reg_stake(&c)],
        vec![withdrawal],
        vec![],
        KEY_DEPOSIT,
        &[&c],
    );
    assert_accepted_on_both_paths(ledger, utxo, tx);
}

// ---------------------------------------------------------------------------
// Negative controls: Haskell rejects these, dugite must keep rejecting
// ---------------------------------------------------------------------------

/// A vote by a DRep that is never registered.
#[test]
fn vote_by_a_drep_that_is_never_registered_is_still_rejected() {
    let x = Signer::new(0x71);
    let r = Signer::new(0x72);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let tx = gov_tx(
        0x51,
        vec![],
        vec![info_proposal(&r)],
        vec![drep_voter(&x)],
        0,
        &[&x],
    );
    assert_rejected_on_both_paths(ledger, utxo, tx, is_voters_missing, "VotersDoNotExist");
}

/// `[RegDRep X, UnRegDRep X]` + a vote by X: X is gone after CERTS.
#[test]
fn vote_by_a_drep_unregistered_in_the_same_tx_is_rejected() {
    let x = Signer::new(0x73);
    let r = Signer::new(0x74);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let tx = gov_tx(
        0x52,
        vec![reg_drep(&x), unreg_drep(&x)],
        vec![info_proposal(&r)],
        vec![drep_voter(&x)],
        0,
        &[&x],
    );
    assert_rejected_on_both_paths(ledger, utxo, tx, is_voters_missing, "VotersDoNotExist");
}

/// A vote by a pool that is never registered.
#[test]
fn vote_by_a_pool_that_is_never_registered_is_still_rejected() {
    let p = Signer::new(0x75);
    let r = Signer::new(0x76);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let tx = gov_tx(
        0x53,
        vec![],
        vec![info_proposal(&r)],
        vec![pool_voter(&p)],
        0,
        &[&p],
    );
    assert_rejected_on_both_paths(ledger, utxo, tx, is_voters_missing, "VotersDoNotExist");
}

/// A proposal whose return account is registered nowhere.
#[test]
fn proposal_returning_to_an_unregistered_account_is_still_rejected() {
    let c = Signer::new(0x77);
    let (ledger, utxo) = ledger_at(10);
    let tx = gov_tx(0x54, vec![], vec![info_proposal(&c)], vec![], 0, &[]);
    assert_rejected_on_both_paths(
        ledger,
        utxo,
        tx,
        is_return_account_missing,
        "ProposalReturnAccountDoesNotExist",
    );
}

/// `UnRegCert c` + a proposal returning to `c`, with `c` registered before the
/// tx: `c` is gone after CERTS.
#[test]
fn proposal_returning_to_an_account_deregistered_in_the_same_tx_is_rejected() {
    let c = Signer::new(0x78);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &c);
    ledger.certs.stake_key_deposits.insert(c.key(), KEY_DEPOSIT);
    let tx = gov_tx(
        0x55,
        vec![dereg_stake(&c)],
        vec![info_proposal(&c)],
        vec![],
        0,
        &[&c],
    );
    // net deposit is a refund here; the output is sized for a +0 net, so only
    // the GOV-class verdict is asserted.
    assert_rejected_on_both_paths(
        ledger,
        utxo,
        tx,
        is_return_account_missing,
        "ProposalReturnAccountDoesNotExist",
    );
}

/// A treasury withdrawal to an account registered nowhere.
#[test]
fn treasury_withdrawal_to_an_unregistered_account_is_still_rejected() {
    let c = Signer::new(0x79);
    let r = Signer::new(0x7A);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    let withdrawal = proposal(
        r.reward_addr(),
        GovAction::TreasuryWithdrawals {
            withdrawals: std::iter::once((c.reward_addr(), Lovelace(1_000))).collect(),
            policy_hash: None,
        },
    );
    let tx = gov_tx(0x56, vec![], vec![withdrawal], vec![], 0, &[]);
    assert_rejected_on_both_paths(
        ledger,
        utxo,
        tx,
        is_treasury_dest_missing,
        "TreasuryWithdrawalReturnAccountsDoNotExist",
    );
}

/// The hot key is re-authorised to another hot key in the same tx: the old one
/// is no longer in `authorizedHotCommitteeCredentials`.
#[test]
fn vote_by_a_hot_key_replaced_in_the_same_tx_is_rejected() {
    let cold = Signer::new(0x7B);
    let hot_old = Signer::new(0x7C);
    let hot_new = Signer::new(0x7D);
    let r = Signer::new(0x7E);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    {
        let gov = Arc::make_mut(&mut ledger.gov.governance);
        gov.committee_expiration.insert(cold.key(), EpochNo(500));
        gov.committee_hot_keys.insert(cold.key(), hot_old.key());
    }
    let tx = gov_tx(
        0x57,
        vec![Certificate::CommitteeHotAuth {
            cold_credential: cold.cred(),
            hot_credential: hot_new.cred(),
        }],
        vec![info_proposal(&r)],
        vec![cc_voter(&hot_old)],
        0,
        &[&cold, &hot_old],
    );
    assert_rejected_on_both_paths(ledger, utxo, tx, is_voters_missing, "VotersDoNotExist");
}

/// The member resigns in the same tx: its hot key is no longer authorised.
#[test]
fn vote_by_a_hot_key_of_a_member_resigned_in_the_same_tx_is_rejected() {
    let cold = Signer::new(0x7F);
    let hot = Signer::new(0x80);
    let r = Signer::new(0x81);
    let (mut ledger, utxo) = ledger_at(10);
    pre_register_account(&mut ledger, &r);
    {
        let gov = Arc::make_mut(&mut ledger.gov.governance);
        gov.committee_expiration.insert(cold.key(), EpochNo(500));
        gov.committee_hot_keys.insert(cold.key(), hot.key());
    }
    let tx = gov_tx(
        0x58,
        vec![Certificate::CommitteeColdResign {
            cold_credential: cold.cred(),
            anchor: None,
        }],
        vec![info_proposal(&r)],
        vec![cc_voter(&hot)],
        0,
        &[&cold, &hot],
    );
    assert_rejected_on_both_paths(ledger, utxo, tx, is_voters_missing, "VotersDoNotExist");
}

/// PV11: a hot key authorised for a cold credential that is not on the
/// ENACTED committee raises `UnelectedCommitteeVoters`.
#[test]
fn pv11_vote_by_a_hot_key_of_an_unelected_cold_key_is_still_rejected() {
    let cold = Signer::new(0x82);
    let hot = Signer::new(0x83);
    let r = Signer::new(0x84);
    let (mut ledger, utxo) = ledger_at(11);
    pre_register_account(&mut ledger, &r);
    // authorised, but `cold` is not in `committee_expiration` (not enacted)
    Arc::make_mut(&mut ledger.gov.governance)
        .committee_hot_keys
        .insert(cold.key(), hot.key());
    let tx = gov_tx(
        0x59,
        vec![],
        vec![info_proposal(&r)],
        vec![cc_voter(&hot)],
        0,
        &[&hot],
    );
    assert_rejected_on_both_paths(ledger, utxo, tx, is_unelected, "UnelectedCommitteeVoters");
}

/// PV11: `CommitteeHotAuth cold hot` in the tx, where `cold` is only a
/// POTENTIAL member (it is in a live `UpdateCommittee` proposal, so the
/// certificate is valid: `isPotentialFutureMember`) and not on the ENACTED
/// committee, plus a vote by `hot`. After CERTS `hot` is authorised but not
/// elected, so Haskell raises `UnelectedCommitteeVoters` (Gov.hs L478-481,
/// `authorizedElectedHotCommitteeCredentials` L581-591) and, since the hot key
/// is a known committee member, no `VotersDoNotExist`.
///
/// The enacted-committee filter in `post_certs_context` is what separates the
/// two sets: without it every authorised hot key counts as elected and this
/// predicate never fires.
#[test]
fn pv11_hot_auth_for_a_cold_key_that_is_not_enacted_then_vote_is_unelected() {
    let cold = Signer::new(0x85);
    let hot = Signer::new(0x86);
    let r = Signer::new(0x87);
    let (mut ledger, utxo) = ledger_at(11);
    pre_register_account(&mut ledger, &r);
    // A live UpdateCommittee proposal that would seat `cold` makes the
    // HotAuth certificate itself valid; `cold` is still not enacted.
    let update_committee = ProposalProcedure {
        deposit: Lovelace(GOV_DEPOSIT),
        return_addr: r.reward_addr(),
        gov_action: GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add: std::iter::once((cold.cred(), 500)).collect(),
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        },
        anchor: Anchor {
            url: String::new(),
            data_hash: Hash32::ZERO,
        },
    };
    Arc::make_mut(&mut ledger.gov.governance).proposals.insert(
        GovActionId {
            transaction_id: Hash32::from_bytes([0xCC; 32]),
            action_index: 0,
        },
        ProposalState {
            procedure: update_committee,
            proposed_epoch: EpochNo(0),
            expires_epoch: EpochNo(100),
            yes_votes: 0,
            no_votes: 0,
            abstain_votes: 0,
            submission_index: 0,
        },
    );
    let tx = gov_tx(
        0x5A,
        vec![Certificate::CommitteeHotAuth {
            cold_credential: cold.cred(),
            hot_credential: hot.cred(),
        }],
        vec![info_proposal(&r)],
        vec![cc_voter(&hot)],
        0,
        &[&cold, &hot],
    );
    let errors = mempool_errors(&ledger, &utxo, &tx);
    assert!(
        errors.iter().any(is_unelected),
        "an authorised but not enacted hot key must raise UnelectedCommitteeVoters; got {errors:?}"
    );
    assert!(
        !errors.iter().any(is_voters_missing),
        "the hot key is authorised after CERTS, so it is a known voter; got {errors:?}"
    );
    let (result, warns) = apply_single_tx_block(&mut ledger, tx);
    assert!(
        result.is_err() || warns.iter().any(|l| l.contains("UnelectedCommitteeVoters")),
        "ValidateAll must flag it; result {result:?}, warns {warns:?}"
    );
}
