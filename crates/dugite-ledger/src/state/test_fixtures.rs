//! A `LedgerState` with every snapshot field populated.
//!
//! # Why this exists (#967)
//!
//! `snapshot_format_hash_stability` guards the on-disk snapshot layout by
//! hashing a serialized `LedgerStateSnapshot` against a pinned constant. Its
//! docstring says a failure means "the serialization format has changed, which
//! will break deserialization of existing snapshots on disk."
//!
//! Its fixture was `LedgerState::new(ProtocolParameters::mainnet_defaults())` —
//! every `Option` `None`, every collection empty. **bincode does not serialize
//! the contents of a `None` or the elements of an empty collection**, so any
//! positional layout change inside a nested structure was invisible to the
//! hash.
//!
//! That is demonstrated, not theoretical: #966 added `treasury: u64` to
//! `PulsingSnapshot` — a genuine positional bincode change inside
//! `GovernanceState`, requiring SNAPSHOT_VERSION 31 -> 32 — and the test stayed
//! green through it, because `ratification_snapshot` was `None`. Several past
//! SNAPSHOT_VERSION bumps were about exactly the structures the fixture left
//! empty (`PulsingRewUpdate` in v27, `future_gen_delegs` in v28,
//! `previous_epoch_nonce` in v30), which means those bumps were caught by
//! review, not by this test.
//!
//! A guard that cannot go red for the change it exists to catch is not a guard.
//!
//! # How the coverage is held
//!
//! Populating the fixture once is not enough — the next field added would sit
//! outside it and the blind spot would silently reopen. So
//! `snapshot_format::tests::fixture_populates_every_snapshot_field`
//! **exhaustively destructures** `LedgerStateSnapshot` with no `..` rest
//! pattern. Adding a field to that struct makes the test fail to *compile*
//! until the fixture populates it. The compiler holds the invariant, not
//! discipline.
//!
//! # Determinism
//!
//! Every value here is a fixed constant — no timestamps, no RNG, no iteration
//! over a `HashMap` whose order could vary. The pinned hash must be stable
//! across runs and platforms.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;
use dugite_primitives::transaction::{
    Anchor, Constitution, DRep, GovActionId, Voter, VotingProcedure,
};

use super::{DRepRegistration, LedgerState, Lovelace, PendingRewardUpdate, PoolRegistration};

fn h28(b: u8) -> Hash28 {
    Hash28::from_bytes([b; 28])
}

fn h32(b: u8) -> Hash32 {
    Hash32::from_bytes([b; 32])
}

/// A non-trivial `NonMyopic`: one pool, a full-length `Likelihood` whose
/// log-weights are all DIFFERENT, and a non-zero reward pot.
///
/// The weights vary across the sample positions on purpose. A constant-filled
/// `Likelihood` would serialize identically under a bincode layout change that
/// reordered or resized the sequence, which is the exact blindness #967 exists
/// to remove.
fn sample_non_myopic(pool_byte: u8, reward_pot: u64) -> super::non_myopic::NonMyopic {
    use super::non_myopic::{Likelihood, NonMyopic, SAMPLE_SIZE};
    let weights: Vec<f32> = (0..SAMPLE_SIZE).map(|i| -(i as f32) * 0.25).collect();
    let mut likelihoods = HashMap::new();
    likelihoods.insert(h28(pool_byte), Likelihood(weights));
    NonMyopic {
        likelihoods,
        reward_pot: Lovelace(reward_pot),
    }
}

/// A `LedgerState` in which every field the snapshot serializes is non-trivial:
/// every `Option` is `Some`, every collection has at least one entry, and every
/// scalar is distinguishable from its default.
///
/// Public (though `#[doc(hidden)]`) so the in-crate layout test and the
/// `tests/snapshot_stability.rs` hash test share ONE fixture. Two fixtures
/// would drift, and a drifting fixture reopens exactly the blind spot this
/// module closes.
#[doc(hidden)]
pub fn populated_ledger_state() -> LedgerState {
    let mut state = LedgerState::new(ProtocolParameters::mainnet_defaults());

    // ── coordination scalars ────────────────────────────────────────────
    state.epoch = EpochNo(317);
    state.epoch_length = 432_000;
    state.shelley_transition_epoch = 208;
    state.byron_epoch_length = 21_600;
    state.genesis_hash = h32(0x9a);
    state.update_quorum = 5;
    state.randomness_stabilisation_window = 172_800;
    state.stability_window_3kf = 129_600;
    state
        .genesis_delegates
        .insert(h28(0x01), (h28(0x02), h32(0x03)));
    state
        .future_gen_delegs
        .insert((4_492_800, h28(0x04)), (h28(0x05), h32(0x06)));

    // ── UTxO sub-state ──────────────────────────────────────────────────
    state.utxo.epoch_fees = Lovelace(1_234_567);
    state.utxo.pending_donations = Lovelace(7_777);

    // ── cert sub-state ──────────────────────────────────────────────────
    state.certs.delegations.insert(h32(0x10), h28(0x11));
    let pool = PoolRegistration {
        pool_id: h28(0x11),
        vrf_keyhash: h32(0x12),
        pledge: Lovelace(500_000_000),
        cost: Lovelace(340_000_000),
        margin_numerator: 3,
        margin_denominator: 100,
        reward_account: vec![0xe0; 29],
        owners: vec![h28(0x13)],
        relays: Vec::new(),
        metadata_url: Some("https://example.invalid/pool.json".to_string()),
        metadata_hash: Some(h32(0x14)),
    };
    Arc::make_mut(&mut state.certs.pool_params).insert(h28(0x11), pool.clone());
    state
        .certs
        .future_pool_params
        .insert(h28(0x11), pool.clone());
    state
        .certs
        .pending_retirements
        .insert(h28(0x15), EpochNo(320));
    state
        .certs
        .reward_accounts
        .insert(h32(0x10), Lovelace(9_999));
    state.certs.stake_key_deposits.insert(h32(0x10), 2_000_000);
    state.certs.pool_deposits.insert(h28(0x11), 500_000_000);
    state.certs.total_stake_key_deposits = 2_000_000;
    state.certs.pointer_map.insert(
        dugite_primitives::credentials::Pointer {
            slot: 42,
            tx_index: 1,
            cert_index: 2,
        },
        h32(0x16),
    );
    state
        .certs
        .stake_distribution
        .stake_map
        .insert(h32(0x10), Lovelace(1_000_000_000));
    state.certs.script_stake_credentials.insert(h32(0x17));
    state.certs.pending_mir_reserves.insert(h32(0x18), 123);
    state.certs.pending_mir_treasury.insert(h32(0x19), -456);
    state.certs.pending_mir_delta_reserves = 789;
    state.certs.pending_mir_delta_treasury = -321;

    // ── consensus sub-state ─────────────────────────────────────────────
    state.consensus.evolving_nonce = h32(0x20);
    state.consensus.candidate_nonce = h32(0x21);
    state.consensus.epoch_nonce = h32(0x22);
    state.consensus.previous_epoch_nonce = h32(0x23);
    state.consensus.lab_nonce = h32(0x24);
    state.consensus.last_epoch_block_nonce = h32(0x25);
    state.consensus.extra_entropy = h32(0x26);
    state.consensus.rolling_nonce = h32(0x27);
    state.consensus.first_block_hash_of_epoch = Some(h32(0x28));
    state.consensus.prev_epoch_first_block_hash = Some(h32(0x29));
    Arc::make_mut(&mut state.consensus.epoch_blocks_by_pool).insert(h28(0x11), 21);
    state.consensus.epoch_block_count = 21;
    state.consensus.opcert_counters.insert(h28(0x11), 7);

    // ── epoch sub-state ─────────────────────────────────────────────────
    state.epochs.treasury = Lovelace(3_347_997_655_395);
    state.epochs.reserves = Lovelace(5_996_646_007_361_582);
    state.epochs.prev_protocol_version_major = 9;
    state.epochs.prev_d = dugite_primitives::transaction::Rational {
        numerator: 1,
        denominator: 2,
    };
    state.epochs.ptr_stake_excluded = true;
    state.epochs.pending_avvm_return = 555;
    state.epochs.ptr_stake.insert(
        dugite_primitives::credentials::Pointer {
            slot: 43,
            tx_index: 2,
            cert_index: 3,
        },
        4_000_000,
    );
    state.epochs.pending_reward_update = Some(PendingRewardUpdate {
        rewards: {
            let mut m = HashMap::new();
            m.insert(h32(0x10), Lovelace(100_000));
            m
        },
        delta_treasury: 42_000,
        delta_reserves: -17,
        non_myopic: sample_non_myopic(0x30, 7_777_777),
    });
    // `esNonMyopic` itself, distinct from the copy riding on the pending reward
    // update — different pool id and pot so a From impl that wires one field to
    // the other cannot pass.
    state.epochs.non_myopic = sample_non_myopic(0x31, 8_888_888);
    // #1072: `true` is the non-default state — a bool left at `false` would
    // contribute the same bytes as an absent field to the layout hash.
    state.epochs.rupd_pulser_started = true;
    state.epochs.rupd_addrs_rew = Some(Arc::new({
        let mut s = HashSet::new();
        s.insert(h32(0x10));
        s
    }));
    // Pre-Conway update proposals — BTreeMap so iteration order is fixed.
    let ppu = dugite_primitives::transaction::ProtocolParamUpdate {
        min_fee_a: Some(44),
        ..Default::default()
    };
    let mut pending = BTreeMap::new();
    pending.insert(EpochNo(318), vec![(h32(0x30), ppu.clone())]);
    state.epochs.pending_pp_updates = pending;
    let mut future = BTreeMap::new();
    future.insert(EpochNo(319), vec![(h32(0x31), ppu)]);
    state.epochs.future_pp_updates = future;

    // mark / set / go, so `EpochSnapshots` and `StakeSnapshot` are both covered.
    let snap = super::StakeSnapshot {
        epoch: EpochNo(316),
        delegations: Arc::new({
            let mut m = HashMap::new();
            m.insert(h32(0x10), h28(0x11));
            m
        }),
        pool_stake: {
            let mut m = HashMap::new();
            m.insert(h28(0x11), Lovelace(1_000_000_000));
            m
        },
        pool_params: Arc::new({
            let mut m = HashMap::new();
            m.insert(h28(0x11), pool);
            m
        }),
        stake_distribution: Arc::new({
            let mut m = HashMap::new();
            m.insert(h32(0x10), Lovelace(1_000_000_000));
            m
        }),
        epoch_fees: Lovelace(11_111),
        epoch_block_count: 21,
        epoch_blocks_by_pool: Arc::new({
            let mut m = HashMap::new();
            m.insert(h28(0x11), 21);
            m
        }),
    };
    state.epochs.snapshots.mark = Some(snap.clone());
    state.epochs.snapshots.set = Some(snap.clone());
    state.epochs.snapshots.go = Some(snap);
    state.epochs.snapshots.ss_fee = Lovelace(22_222);

    // ── governance sub-state ────────────────────────────────────────────
    let gov = Arc::make_mut(&mut state.gov.governance);
    let anchor = Anchor {
        url: "https://example.invalid/anchor.json".to_string(),
        data_hash: h32(0x40),
    };
    gov.dreps.insert(
        h32(0x41),
        DRepRegistration {
            credential: dugite_primitives::credentials::Credential::VerificationKey(h28(0x41)),
            deposit: Lovelace(500_000_000),
            anchor: Some(anchor.clone()),
            registered_epoch: EpochNo(310),
            drep_expiry: EpochNo(330),
            active: true,
        },
    );
    gov.vote_delegations
        .insert(h32(0x10), DRep::KeyHash(h32(0x41)));
    gov.committee_hot_keys.insert(h32(0x42), h32(0x43));
    gov.committee_expiration.insert(h32(0x42), EpochNo(400));
    gov.committee_resigned
        .insert(h32(0x44), Some(anchor.clone()));
    gov.script_committee_credentials.insert(h32(0x45));
    gov.script_committee_hot_credentials.insert(h32(0x46));
    gov.drep_registration_count = 1;
    gov.proposal_count = 1;
    gov.constitution = Some(Constitution {
        anchor: anchor.clone(),
        script_hash: Some(h28(0x47)),
    });
    gov.no_confidence = true;
    gov.committee_threshold = Some(dugite_primitives::transaction::Rational {
        numerator: 2,
        denominator: 3,
    });
    let gid = |b: u8| GovActionId {
        transaction_id: h32(b),
        action_index: 0,
    };
    gov.enacted_pparam_update = Some(gid(0x50));
    gov.enacted_hard_fork = Some(gid(0x51));
    gov.enacted_committee = Some(gid(0x52));
    gov.enacted_constitution = Some(gid(0x53));
    gov.last_expired = vec![gid(0x54)];
    gov.last_ratify_delayed = true;
    gov.num_dormant_epochs = 2;
    gov.votes_by_action.insert(gid(0x55), {
        let mut m = imbl::OrdMap::new();
        m.insert(
            Voter::DRep(dugite_primitives::credentials::Credential::VerificationKey(
                h28(0x41),
            )),
            VotingProcedure {
                vote: dugite_primitives::transaction::Vote::Yes,
                anchor: Some(anchor),
            },
        );
        m
    });

    // The frozen DRep pulser result (#988). Populated for the same reason as
    // everything else here: bincode writes nothing for a `None`, so leaving it
    // unset would hide `PulsedRatifyState`'s layout from the format hash.
    // #977. `Definite` rather than `Potential`, so the fixture carries a
    // PAYLOAD — `PotentialPParamsUpdate(None)` writes no parameters and would
    // leave `ProtocolParameters`' layout inside this enum unhashed.
    gov.future_pparams = super::FuturePParams::DefinitePParamsUpdate(Box::new(
        ProtocolParameters::mainnet_defaults(),
    ));

    // A live proposal, so `PulsingSnapshot.proposals` is non-empty once frozen.
    // Without one, `ProposalState`'s layout inside the pulser is invisible to
    // the format hash — a gap that has been open since #903, found by #988
    // step 3 tightening the guard.
    gov.proposals.insert(
        gid(0x56),
        super::ProposalState {
            procedure: dugite_primitives::transaction::ProposalProcedure {
                deposit: Lovelace(100_000_000_000),
                return_addr: vec![0xe0; 29],
                gov_action: dugite_primitives::transaction::GovAction::InfoAction,
                anchor: Anchor {
                    url: "https://example.invalid/proposal.json".to_string(),
                    data_hash: h32(0x57),
                },
            },
            proposed_epoch: EpochNo(315),
            expires_epoch: EpochNo(320),
            yes_votes: 3,
            no_votes: 2,
            abstain_votes: 1,
            submission_index: 7,
        },
    );

    // Freeze the pulser LAST, so its snapshot captures the populated governance
    // state above rather than an empty one. #966 added `treasury` to it and
    // this test could not see it.
    state.capture_ratification_snapshot();

    // Then overwrite both halves with distinctive values. Every field must be
    // non-default: bincode writes nothing for a `None` and zeroes are
    // indistinguishable from an unwritten field, so a default here would hide
    // that part of the layout from the format hash.
    {
        let gov = std::sync::Arc::make_mut(&mut state.gov.governance);
        let pulser = gov
            .drep_pulsing_state
            .as_mut()
            .expect("the fixture must carry a frozen pulser");
        pulser.snapshot.drep_distr.insert(h32(0x41), 1_000_000);
        pulser.snapshot.drep_no_confidence = 10;
        pulser.snapshot.drep_abstain = 20;
        pulser.snapshot.drep_no_confidence_delegated = true;
        pulser.snapshot.drep_abstain_delegated = true;
        pulser.ratify_state = super::PulsedRatifyState {
            computed_at_epoch: EpochNo(316),
            enacted: vec![gid(0x60)],
            expired: vec![gid(0x61)],
            delayed: true,
            cur_pparams: ProtocolParameters::mainnet_defaults(),
            has_pparams_changes: true,
        };
    }

    state
}
