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

use super::{
    DRepRegistration, LedgerState, Lovelace, PEdges, PendingRewardUpdate, PoolRegistration,
};

fn h28(b: u8) -> Hash28 {
    Hash28::from_bytes([b; 28])
}

fn h32(b: u8) -> Hash32 {
    Hash32::from_bytes([b; 32])
}

/// A non-trivial `NonMyopic`: **two** pools, full-length `Likelihood`s whose
/// log-weights are all DIFFERENT, and a non-zero reward pot.
///
/// The weights vary across the sample positions on purpose. A constant-filled
/// `Likelihood` would serialize identically under a bincode layout change that
/// reordered or resized the sequence, which is the exact blindness #967 exists
/// to remove. Two pools (not one, #1088): `likelihoods` is a `HashMap`, and a
/// single-entry map has nothing to reorder, so it cannot exercise the
/// snapshot-ordering determinism guard.
fn sample_non_myopic(pool_byte: u8, reward_pot: u64) -> super::non_myopic::NonMyopic {
    use super::non_myopic::{Likelihood, NonMyopic, SAMPLE_SIZE};
    let weights: Vec<f32> = (0..SAMPLE_SIZE).map(|i| -(i as f32) * 0.25).collect();
    let weights2: Vec<f32> = (0..SAMPLE_SIZE).map(|i| -(i as f32) * 0.5 - 1.0).collect();
    let mut likelihoods = HashMap::new();
    likelihoods.insert(h28(pool_byte), Likelihood(weights));
    likelihoods.insert(h28(pool_byte.wrapping_add(1)), Likelihood(weights2));
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
        .genesis_delegates
        .insert(h28(0x07), (h28(0x08), h32(0x09)));
    state
        .future_gen_delegs
        .insert((4_492_800, h28(0x04)), (h28(0x05), h32(0x06)));
    state
        .future_gen_delegs
        .insert((4_492_900, h28(0x0a)), (h28(0x0b), h32(0x0c)));

    // ── UTxO sub-state ──────────────────────────────────────────────────
    state.utxo.epoch_fees = Lovelace(1_234_567);
    state.utxo.pending_donations = Lovelace(7_777);

    // ── cert sub-state ──────────────────────────────────────────────────
    //
    // Every map here carries TWO entries (#1088): with 0 or 1 entries there
    // is nothing for a hash-ordered container to reorder, so a single-entry
    // fixture cannot exercise the snapshot determinism guard — which is
    // exactly how this test stayed green for years while the bug shipped.
    state.certs.delegations.insert(h32(0x10), h28(0x11));
    state.certs.delegations.insert(h32(0x1a), h28(0x1b));
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
    let pool2 = PoolRegistration {
        pool_id: h28(0x1c),
        vrf_keyhash: h32(0x1d),
        pledge: Lovelace(250_000_000),
        cost: Lovelace(170_000_000),
        margin_numerator: 1,
        margin_denominator: 50,
        reward_account: vec![0xe0; 29],
        owners: vec![h28(0x1e)],
        relays: Vec::new(),
        metadata_url: None,
        metadata_hash: None,
    };
    Arc::make_mut(&mut state.certs.pool_params).insert(h28(0x11), pool.clone());
    Arc::make_mut(&mut state.certs.pool_params).insert(h28(0x1c), pool2.clone());
    state
        .certs
        .future_pool_params
        .insert(h28(0x11), pool.clone());
    state
        .certs
        .future_pool_params
        .insert(h28(0x1c), pool2.clone());
    state
        .certs
        .pending_retirements
        .insert(h28(0x15), EpochNo(320));
    state
        .certs
        .pending_retirements
        .insert(h28(0x1c), EpochNo(321));
    // `psVRFKeyHashes` — a count above one on purpose: that is the state a
    // pre-PV11 duplicate leaves behind, and a layout that only ever saw 1
    // would not exercise the field's width.
    state.certs.vrf_key_hashes.insert(h32(0x72), 2);
    state.certs.vrf_key_hashes.insert(h32(0x73), 1);
    state
        .certs
        .reward_accounts
        .insert(h32(0x10), Lovelace(9_999));
    state
        .certs
        .reward_accounts
        .insert(h32(0x1a), Lovelace(4_242));
    state.certs.stake_key_deposits.insert(h32(0x10), 2_000_000);
    state.certs.stake_key_deposits.insert(h32(0x1a), 3_000_000);
    state.certs.pool_deposits.insert(h28(0x11), 500_000_000);
    state.certs.pool_deposits.insert(h28(0x1c), 500_000_000);
    state.certs.total_stake_key_deposits = 5_000_000;
    state.certs.pointer_map.insert(
        dugite_primitives::credentials::Pointer {
            slot: 42,
            tx_index: 1,
            cert_index: 2,
        },
        h32(0x16),
    );
    state.certs.pointer_map.insert(
        dugite_primitives::credentials::Pointer {
            slot: 44,
            tx_index: 3,
            cert_index: 4,
        },
        h32(0x1f),
    );
    state
        .certs
        .stake_distribution
        .stake_map
        .insert(h32(0x10), Lovelace(1_000_000_000));
    state
        .certs
        .stake_distribution
        .stake_map
        .insert(h32(0x1a), Lovelace(250_000_000));
    state.certs.script_stake_credentials.insert(h32(0x17));
    state.certs.script_stake_credentials.insert(h32(0x1a));
    state.certs.pending_mir_reserves.insert(h32(0x18), 123);
    state.certs.pending_mir_reserves.insert(h32(0x1a), 321);
    state.certs.pending_mir_treasury.insert(h32(0x19), -456);
    state.certs.pending_mir_treasury.insert(h32(0x1a), -654);
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
    Arc::make_mut(&mut state.consensus.epoch_blocks_by_pool).insert(h28(0x1c), 9);
    state.consensus.epoch_block_count = 30;
    state.consensus.opcert_counters.insert(h28(0x11), 7);
    state.consensus.opcert_counters.insert(h28(0x1c), 3);

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
    state.epochs.ptr_stake.insert(
        dugite_primitives::credentials::Pointer {
            slot: 45,
            tx_index: 4,
            cert_index: 5,
        },
        1_500_000,
    );
    state.epochs.pending_reward_update = Some(PendingRewardUpdate {
        rewards: {
            let mut m = HashMap::new();
            m.insert(h32(0x10), Lovelace(100_000));
            m.insert(h32(0x1a), Lovelace(50_000));
            m
        },
        delta_treasury: 42_000,
        delta_reserves: -17,
        non_myopic: sample_non_myopic(0x30, 7_777_777),
        // `#[serde(skip)]` — wire-only (#1071), not part of the layout hash.
        raw_rewards: HashMap::new(),
    });
    // `esNonMyopic` itself, distinct from the copy riding on the pending reward
    // update — different pool id and pot so a From impl that wires one field to
    // the other cannot pass.
    state.epochs.non_myopic = sample_non_myopic(0x31, 8_888_888);
    // #1072: `true` is the non-default state — a bool left at `false` would
    // contribute the same bytes as an absent field to the layout hash.
    state.epochs.rupd_pulser_started = true;
    state.epochs.rupd_monetary = Some(crate::state::reward_pulser::MonetaryStep {
        delta_r1: 3_000_000_000_000,
        delta_t1: 600_000_000_200,
        r: 2_400_000_000_800,
        expected_blocks: 21_600,
        // Deliberately NOT max_supply - reserves for this fixture's reserves:
        // the whole point of freezing it is that it can differ from a
        // boundary-time recomputation, so a fixture that made them equal
        // could not tell a frozen read from a live one.
        total_stake: 31_112_484_745_368_612,
    });
    state.epochs.rupd_addrs_rew = Some(Arc::new({
        let mut s = HashSet::new();
        s.insert(h32(0x10));
        s.insert(h32(0x1a));
        s
    }));
    // #1071: the WIRE-ONLY `nesRu` mirror. `Complete` rather than `Pulsing`
    // so the fixture exercises the arm the N2C encoder actually emits today;
    // every map/set field carries 2+ entries per #1088's convention —
    // `likelihoods`, `leaders` and `free_vars.addrs_rew` are all
    // `HashMap`/`HashSet` on the live type and would otherwise be invisible
    // to the layout hash.
    state.epochs.rupd_snapshot = Some(crate::state::reward_pulser::PulsingRewUpdate::Complete(
        Box::new(crate::state::reward_pulser::RewardSnapShot {
            fees: Lovelace(555_555),
            protocol_version: (10, 0),
            non_myopic: sample_non_myopic(0x34, 9_999_999),
            delta_r1: Lovelace(3_000_000_000_000),
            r: Lovelace(2_400_000_000_800),
            delta_t1: Lovelace(600_000_000_200),
            likelihoods: {
                let mut m = HashMap::new();
                m.insert(
                    h28(0x40),
                    crate::state::non_myopic::Likelihood::new(50, 0.1, 432_000),
                );
                m.insert(
                    h28(0x41),
                    crate::state::non_myopic::Likelihood::new(30, 0.2, 432_000),
                );
                m
            },
            leaders: {
                let mut m = HashMap::new();
                m.insert(
                    h32(0x42),
                    vec![crate::state::reward_pulser::RewardEntry {
                        is_member: false,
                        pool_id: h28(0x40),
                        amount: 12_345,
                    }],
                );
                m.insert(
                    h32(0x43),
                    vec![crate::state::reward_pulser::RewardEntry {
                        is_member: false,
                        pool_id: h28(0x41),
                        amount: 54_321,
                    }],
                );
                m
            },
            free_vars: crate::state::reward_pulser::FreeVars {
                addrs_rew: Some({
                    let mut s = HashSet::new();
                    s.insert(h32(0x10));
                    s.insert(h32(0x1a));
                    s
                }),
                total_stake: 31_112_484_745_368_612,
                prot_ver: (10, 0),
            },
        }),
    ));
    // Pre-Conway update proposals — BTreeMap so iteration order is fixed.
    // Two entries even so: `EpochSnapshotsWire`'s sibling maps need the
    // width and drift-proofing the whole fixture to one shape is simpler
    // than special-casing the already-ordered fields.
    let ppu = dugite_primitives::transaction::ProtocolParamUpdate {
        min_fee_a: Some(44),
        ..Default::default()
    };
    let mut pending = BTreeMap::new();
    pending.insert(
        EpochNo(318),
        vec![(h32(0x30), ppu.clone()), (h32(0x32), ppu.clone())],
    );
    state.epochs.pending_pp_updates = pending;
    let mut future = BTreeMap::new();
    future.insert(
        EpochNo(319),
        vec![(h32(0x31), ppu.clone()), (h32(0x33), ppu)],
    );
    state.epochs.future_pp_updates = future;

    // mark / set / go, so `EpochSnapshots` and `StakeSnapshot` are both covered.
    let snap = super::StakeSnapshot {
        epoch: EpochNo(316),
        delegations: Arc::new({
            let mut m = HashMap::new();
            m.insert(h32(0x10), h28(0x11));
            m.insert(h32(0x1a), h28(0x1c));
            m
        }),
        pool_stake: {
            let mut m = HashMap::new();
            m.insert(h28(0x11), Lovelace(1_000_000_000));
            m.insert(h28(0x1c), Lovelace(250_000_000));
            m
        },
        pool_params: Arc::new({
            let mut m = HashMap::new();
            m.insert(h28(0x11), pool);
            m.insert(h28(0x1c), pool2);
            m
        }),
        stake_distribution: Arc::new({
            let mut m = HashMap::new();
            m.insert(h32(0x10), Lovelace(1_000_000_000));
            m.insert(h32(0x1a), Lovelace(250_000_000));
            m
        }),
        epoch_fees: Lovelace(11_111),
        epoch_block_count: 30,
        epoch_blocks_by_pool: Arc::new({
            let mut m = HashMap::new();
            m.insert(h28(0x11), 21);
            m.insert(h28(0x1c), 9);
            m
        }),
    };
    state.epochs.snapshots.mark = Some(snap.clone());
    state.epochs.snapshots.set = Some(snap.clone());
    state.epochs.snapshots.go = Some(snap);
    state.epochs.snapshots.ss_fee = Lovelace(22_222);
    state.epochs.snapshots.bprev_block_count = 30;
    state.epochs.snapshots.bprev_blocks_by_pool = Arc::new({
        let mut m = HashMap::new();
        m.insert(h28(0x11), 21);
        m.insert(h28(0x1c), 9);
        m
    });

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
            // Two delegators on purpose (#1088): `DRepRegistration.delegs`
            // is a set nested one level below `dreps`' own keys, and a
            // single-entry set is invisible to the width check the outer
            // map's `at_least_two!` cannot see.
            delegs: {
                let mut s = imbl::HashSet::new();
                s.insert(h32(0x71));
                s.insert(h32(0x74));
                s
            },
        },
    );
    gov.dreps.insert(
        h32(0x48),
        DRepRegistration {
            credential: dugite_primitives::credentials::Credential::VerificationKey(h28(0x48)),
            deposit: Lovelace(500_000_000),
            anchor: None,
            registered_epoch: EpochNo(311),
            drep_expiry: EpochNo(331),
            active: true,
            delegs: {
                let mut s = imbl::HashSet::new();
                s.insert(h32(0x75));
                s
            },
        },
    );
    gov.vote_delegations
        .insert(h32(0x10), DRep::KeyHash(h32(0x41)));
    gov.vote_delegations
        .insert(h32(0x1a), DRep::KeyHash(h32(0x48)));
    gov.committee_hot_keys.insert(h32(0x42), h32(0x43));
    gov.committee_hot_keys.insert(h32(0x4a), h32(0x4b));
    gov.committee_expiration.insert(h32(0x42), EpochNo(400));
    gov.committee_expiration.insert(h32(0x4a), EpochNo(410));
    gov.committee_resigned
        .insert(h32(0x44), Some(anchor.clone()));
    // A `None` second entry: `Option<Anchor>` is the map's VALUE type and
    // both arms should be exercised, not just `Some`.
    gov.committee_resigned.insert(h32(0x4c), None);
    gov.script_committee_credentials.insert(h32(0x45));
    gov.script_committee_credentials.insert(h32(0x4d));
    gov.script_committee_hot_credentials.insert(h32(0x46));
    gov.script_committee_hot_credentials.insert(h32(0x4e));
    // `proposal_graph` — one purpose tree (PParam) with two nodes, so
    // `PGraph.nodes` (an `ImblHashMap`, #1088) is exercised at least once.
    // The other three purposes (hard_fork/committee/constitution) stay
    // empty, matching how a real chain rarely has deep trees in every
    // purpose simultaneously.
    gov.proposal_graph.pparam.nodes.insert(
        GovActionId {
            transaction_id: h32(0x58),
            action_index: 0,
        },
        PEdges {
            parent: Some(GovActionId {
                transaction_id: h32(0x56),
                action_index: 0,
            }),
            children: imbl::OrdSet::new(),
        },
    );
    gov.proposal_graph.pparam.nodes.insert(
        GovActionId {
            transaction_id: h32(0x59),
            action_index: 0,
        },
        PEdges {
            parent: Some(GovActionId {
                transaction_id: h32(0x56),
                action_index: 0,
            }),
            children: imbl::OrdSet::new(),
        },
    );
    gov.drep_registration_count = 2;
    gov.proposal_count = 2;
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
                anchor: Some(anchor.clone()),
            },
        );
        m
    });
    // A second action id (#1088): `votes_by_action` is `ImblOrdMap`-keyed so
    // it was never nondeterministic, but every collection here otherwise
    // carries 2+ entries and there is no reason for this one to be the
    // exception.
    gov.votes_by_action.insert(gid(0x5b), {
        let mut m = imbl::OrdMap::new();
        m.insert(
            Voter::StakePool(h32(0x59)),
            VotingProcedure {
                vote: dugite_primitives::transaction::Vote::No,
                anchor: None,
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
    // A second proposal (#1088): `proposals` is `ImblOrdMap`-keyed (never
    // nondeterministic) but every other collection here carries 2+ entries.
    gov.proposals.insert(
        gid(0x5a),
        super::ProposalState {
            procedure: dugite_primitives::transaction::ProposalProcedure {
                deposit: Lovelace(50_000_000_000),
                return_addr: vec![0xe0; 29],
                gov_action: dugite_primitives::transaction::GovAction::InfoAction,
                anchor: Anchor {
                    url: "https://example.invalid/proposal-2.json".to_string(),
                    data_hash: h32(0x5c),
                },
            },
            proposed_epoch: EpochNo(316),
            expires_epoch: EpochNo(321),
            yes_votes: 1,
            no_votes: 4,
            abstain_votes: 0,
            submission_index: 8,
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
        // A second entry (#1088): `drep_distr` is an `ImblHashMap` and a
        // single credential leaves it with nothing to reorder.
        pulser.snapshot.drep_distr.insert(h32(0x48), 750_000);
        // Frozen `dpDRepState`. Two entries on purpose, straddling the
        // consumption epoch, so the fixture exercises both arms of
        // `drep_is_expired` rather than only the live one.
        pulser
            .snapshot
            .drep_expiry
            .insert(h32(0x41), EpochNo(9_999));
        pulser.snapshot.drep_expiry.insert(h32(0x42), EpochNo(1));
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
            // Deliberately DIFFERENT from the live governance values set above
            // (roots 0x50-0x53, committee cold cred, constitution): this half
            // of `rsEnactState` is one boundary ahead of live state, so a
            // fixture that reused the live values could not tell a correct
            // capture from a live read.
            enact_state: super::EnactedGovTerms {
                committee_expiration: {
                    let mut m = imbl::HashMap::new();
                    m.insert(h32(0x66), EpochNo(400));
                    m.insert(h32(0x69), EpochNo(401));
                    m
                },
                committee_threshold: Some(dugite_primitives::transaction::Rational {
                    numerator: 3,
                    denominator: 5,
                }),
                constitution: Some(Constitution {
                    anchor: Anchor {
                        url: "https://enact.example/constitution".to_string(),
                        data_hash: h32(0x67),
                    },
                    script_hash: Some(h28(0x68)),
                }),
                prev_gov_action_ids: super::GovRelation {
                    pparam: Some(gid(0x62)),
                    hard_fork: Some(gid(0x63)),
                    committee: Some(gid(0x64)),
                    constitution: Some(gid(0x65)),
                },
            },
        };
    }

    state
}
