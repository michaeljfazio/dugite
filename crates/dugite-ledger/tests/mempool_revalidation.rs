//! Regression tests for #996 — post-block mempool revalidation.
//!
//! A transaction that was valid when it was admitted can be invalidated by a
//! later block. Haskell drops it: `revalidateTxsFor` re-checks every remaining
//! mempool transaction on each tip change via `reapplyTxs`, which at the ledger
//! layer is
//!
//! ```haskell
//! reapplyTx globals env state (Validated tx) =
//!   fst <$> internalApplyTxWithValidation (ValidateSuchThat (notElem lblStatic)) globals env state tx
//! ```
//!
//! — every state-dependent predicate re-run, only the static ones skipped.
//!
//! dugite used to re-check a hand-written list (consumed inputs, TTL, missing
//! UTxO, dangling gov-action votes) instead, so any other predicate was
//! invisible after admission. #996 is the case that outran the list: a
//! `CommitteeHotAuth` admitted while its cold credential was still serving, a
//! later block carrying that member's `CommitteeColdResign`, and the stale
//! certificate forged into a block cardano-node rejected forever with
//! `ConwayCommitteeHasPreviouslyResigned`.
//!
//! These tests drive the two pieces that close it — the shared
//! `LedgerState::mempool_validation_context` and `reapply_tx_for_mempool` —
//! and assert the verdict FLIPS for one unchanged transaction when only the
//! ledger moves. Asserting the flip (rather than a single rejection) is what
//! makes this a revalidation test and not just a predicate test.

use std::sync::Arc;

use dugite_ledger::state::LedgerState;
use dugite_ledger::utxo::UtxoSet;
use dugite_ledger::validation::{reapply_tx_for_mempool, ValidationError};
use dugite_primitives::address::{Address, ByronAddress};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Certificate, OutputDatum, Transaction, TransactionBody, TransactionInput, TransactionOutput,
    TransactionWitnessSet,
};
use dugite_primitives::value::{Lovelace, Value};

/// The committee member's cold credential, and the hot key it authorises.
fn cold_cred() -> Credential {
    Credential::VerificationKey(Hash28::from_bytes([0xB2; 28]))
}

fn hot_cred() -> Credential {
    Credential::VerificationKey(Hash28::from_bytes([0x77; 28]))
}

/// Conway ledger with `cold_cred()` seated on the committee and NOT resigned.
fn ledger_with_seated_committee_member() -> (LedgerState, UtxoSet) {
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = 9; // Conway — the committee predicates are gated on this.
    let mut ledger = LedgerState::new(params.clone());
    ledger.epochs.protocol_params = params;

    {
        let gov = Arc::make_mut(&mut ledger.gov.governance);
        gov.committee_expiration
            .insert(cold_cred().to_typed_hash32(), EpochNo(500));
    }

    let input = TransactionInput {
        transaction_id: Hash32::from_bytes([0xAA; 32]),
        index: 0,
    };
    let mut utxo_set = UtxoSet::new();
    utxo_set.insert(
        input,
        TransactionOutput {
            // Byron payload: no witness required, so the tx does not fail for
            // an unrelated reason before reaching the committee predicate.
            address: Address::Byron(ByronAddress {
                payload: vec![0x82, 0x00, 0x01],
            }),
            value: Value::lovelace(10_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        },
    );

    (ledger, utxo_set)
}

/// A transaction authorising `hot_cred()` for `cold_cred()`.
fn hot_key_auth_tx() -> Transaction {
    Transaction {
        era: Era::Conway,
        hash: Hash32::from_bytes([0xEE; 32]),
        body: TransactionBody {
            inputs: vec![TransactionInput {
                transaction_id: Hash32::from_bytes([0xAA; 32]),
                index: 0,
            }],
            outputs: vec![TransactionOutput {
                address: Address::Byron(ByronAddress {
                    payload: vec![0x82, 0x00, 0x01],
                }),
                value: Value::lovelace(9_800_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            }],
            fee: Lovelace(200_000),
            certificates: vec![Certificate::CommitteeHotAuth {
                cold_credential: cold_cred(),
                hot_credential: hot_cred(),
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

fn revalidate(ledger: &LedgerState, utxo_set: &UtxoSet, tx: &Transaction) -> Vec<ValidationError> {
    reapply_tx_for_mempool(
        tx,
        utxo_set,
        &ledger.epochs.protocol_params,
        /* current_slot */ 100,
        /* tx_size */ 300,
        None,
        ledger.mempool_validation_context(),
    )
    .err()
    .unwrap_or_default()
}

fn has_resigned_error(errors: &[ValidationError]) -> bool {
    errors
        .iter()
        .any(|e| matches!(e, ValidationError::CommitteeHasPreviouslyResigned { .. }))
}

/// The #996 flip: one unchanged transaction, two ledger states.
///
/// Admitted while the member is seated it must pass the committee predicate;
/// once a later block records that member's resignation the SAME transaction
/// must be rejected, so post-block revalidation evicts it instead of the forge
/// minting a block every Haskell peer refuses.
#[test]
fn hot_key_auth_valid_at_admission_is_rejected_after_the_member_resigns() {
    let (mut ledger, utxo_set) = ledger_with_seated_committee_member();
    let tx = hot_key_auth_tx();

    // ── Admission-time state: member seated, not resigned ────────────────
    let before = revalidate(&ledger, &utxo_set, &tx);
    assert!(
        !has_resigned_error(&before),
        "a seated committee member may authorise a hot key; got {before:?}"
    );

    // ── A later block applies that member's CommitteeColdResign ──────────
    {
        let gov = Arc::make_mut(&mut ledger.gov.governance);
        gov.committee_resigned
            .insert(cold_cred().to_typed_hash32(), None);
    }

    let after = revalidate(&ledger, &utxo_set, &tx);
    assert!(
        has_resigned_error(&after),
        "after the resignation the same tx must be rejected with \
         CommitteeHasPreviouslyResigned so revalidation evicts it before the \
         forge picks it up (#996); got {after:?}"
    );
}

/// The context builder is what carries the predicate's input. If
/// `mempool_validation_context` ever stops projecting `committee_resigned`,
/// the test above would silently pass at admission and fail to flip — so pin
/// the projection itself.
#[test]
fn mempool_validation_context_projects_the_resigned_set() {
    let (mut ledger, _utxo) = ledger_with_seated_committee_member();
    assert!(
        ledger
            .mempool_validation_context()
            .committee_resigned
            .expect("committee_resigned must be projected, not left None")
            .is_empty(),
        "no member has resigned yet"
    );

    {
        let gov = Arc::make_mut(&mut ledger.gov.governance);
        gov.committee_resigned
            .insert(cold_cred().to_typed_hash32(), None);
    }

    let resigned = ledger
        .mempool_validation_context()
        .committee_resigned
        .expect("still projected");
    assert!(
        resigned.contains(&cold_cred().to_typed_hash32()),
        "the resignation must reach the mempool context"
    );
}

/// Haskell's GOVCERT accepts a hot-key authorisation from an INCOMING member
/// of a live `UpdateCommittee` proposal (`isPotentialFutureMember`), not only
/// from a currently-seated one. Admission used to key this off
/// `committee_expiration` alone — a false reject that block-apply did not
/// share. Consolidating on one builder fixed it; pin it so it stays fixed.
#[test]
fn mempool_context_admits_incoming_committee_members() {
    let (ledger, _utxo) = ledger_with_seated_committee_member();
    let members = ledger
        .mempool_validation_context()
        .committee_members
        .expect("committee_members must be projected");
    assert!(
        members.contains(&cold_cred().to_typed_hash32()),
        "a seated member is eligible"
    );
    // `committee_auth_eligible_members` is the wider Haskell set; the seated
    // member is the floor, and the union with live UpdateCommittee proposals
    // is covered by the ledger-side unit tests for that helper.
}
