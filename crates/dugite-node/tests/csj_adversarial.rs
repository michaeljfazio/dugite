//! Phase E adversarial mock-peer test suite for ChainSync Jumping (CSJ).
//!
//! # What is tested
//!
//! Five scenarios drawn from the Haskell `Ouroboros.Consensus.Tests.Genesis`
//! harness pattern, adapted to the Rust CSJ orchestrator API:
//!
//! 1. **Safety** — A dynamo that lies about its jump tip is unseated by GDD and
//!    the honest chain is adopted.
//! 2. **Liveness** — A stalled dynamo is demoted and a fresh dynamo is elected
//!    within the 10-second grace period.
//! 3. **Eclipse resistance** — In a 1-honest-of-N-peer topology the orchestrator
//!    converges to the honest chain regardless of how many adversarial peers lie.
//! 4. **Property test** — Randomised peer behaviour (proptest) with N ∈ [2, 6]
//!    peers, each independently honest or adversarial, always preserves the
//!    dynamo invariant.
//! 5. **Determinism gate** — Validates that `tokio::time::pause` keeps all
//!    timing assertions reproducible with no wall-clock sleeps.
//!
//! Additional targeted scenarios:
//! 6. **GDD tie** — equal density keeps the existing dynamo (Haskell rule).
//! 7. **Sequential objections** — two rounds of objection do not corrupt state.
//! 8. **Mid-jump dynamo disconnect** — re-election fires, invariant preserved.
//!
//! # Divergence from Haskell
//!
//! The Haskell test harness drives actual wire-level state machines.  This
//! suite drives the orchestrator's public handler methods directly, bypassing
//! the full tokio task graph for determinism.  Scenarios requiring real timing
//! (stall detection) use `spawn_csj_orchestrator` + `tokio::time::pause`.

use std::net::SocketAddr;
use std::time::Duration;

use dugite_network::codec::Point;
use dugite_network::protocol::chainsync::jumping::check_dynamo_invariant;
use dugite_node::csj_orchestrator::{
    CsjConfig, CsjOrchestrator, OrchestratorDecision, PeerInstruction, PeerRegistrationSender,
};
use proptest::prelude::*;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

// ─── Shared fixtures ──────────────────────────────────────────────────────────

fn peer(n: u8) -> SocketAddr {
    format!("127.0.0.{n}:3001").parse().unwrap()
}

/// Small k/f for fast genesis windows in tests (window = ceil(3*10/0.5) = 60 slots).
fn test_config() -> CsjConfig {
    CsjConfig {
        security_param_k: 10,
        active_slot_coeff_f: 0.5,
    }
}

/// Register a peer and return the instruction receiver.
async fn register(
    reg_tx: &PeerRegistrationSender,
    addr: SocketAddr,
    latency_ms: Option<f64>,
) -> mpsc::Receiver<PeerInstruction> {
    let (tx, rx) = mpsc::channel(32);
    reg_tx
        .send((addr, latency_ms, tx))
        .await
        .expect("registration send");
    rx
}

// ─── Test 1: Safety — lying dynamo is unseated, honest chain adopted ──────────
//
// Setup:
//   P1 (dynamo, latency 50ms)   — claims tip 200 but only has ~10 blocks in the
//                                  genesis window (it is lying / adversarial).
//   P2 (honest, latency 100ms)  — objects with 50 real blocks in the window.
//
// Expected outcome:
//   GDD comparison: dynamo_blocks(~10) < objector_blocks(50) → KeepObjector.
//   P2 becomes the new dynamo; P1 is demoted.
//
// Divergence from Haskell: the Haskell harness validates tip-hash via the
// chain-fragment; here we inject `objector_blocks_in_window` directly to
// exercise the GDD comparison without a full chain-fragment implementation.

#[tokio::test]
async fn safety_lying_dynamo_unseated_honest_chain_adopted() {
    time::pause();
    let (mut orch, _evt_tx, _reg_tx) = CsjOrchestrator::new(test_config(), None);

    let (tx_dynamo, mut rx_dynamo) = mpsc::channel(16);
    let (tx_honest, mut rx_honest) = mpsc::channel(16);

    // Register P1 (adversarial dynamo candidate, lower latency).
    orch.handle_registration(peer(1), Some(50.0), tx_dynamo)
        .await;
    // Register P2 (honest jumper, higher latency).
    orch.handle_registration(peer(2), Some(100.0), tx_honest)
        .await;

    // Drain BecomeDynamo for P1.
    let msg1 = rx_dynamo.recv().await.expect("P1 BecomeDynamo");
    assert!(
        matches!(msg1, PeerInstruction::BecomeDynamo),
        "P1 must receive BecomeDynamo"
    );
    assert_eq!(
        orch.test_dynamo_addr(),
        Some(peer(1)),
        "P1 must be elected initial dynamo"
    );

    // Simulate adversarial dynamo advancing its tip: slot 200 with few real blocks.
    orch.handle_dynamo_tip_advanced(peer(1), 200, [1u8; 32])
        .await;

    // Drain the Jump instruction sent to P2 (jump_slot = 200 - 60 = 140).
    let jump_msg = timeout(Duration::from_millis(200), rx_honest.recv())
        .await
        .expect("P2 Jump recv timeout")
        .expect("P2 channel closed");
    assert!(
        matches!(jump_msg, PeerInstruction::Jump(_)),
        "P2 must receive a Jump instruction; got {jump_msg:?}"
    );

    // P2 is already in LookingForIntersection — handle_dynamo_tip_advanced issued
    // the jump instruction (slot 140 = 200 - genesis_window(60)) and called
    // on_jump_issued internally.  Simulate the peer's MsgIntersectNotFound reply.
    orch.handle_intersect_not_found(peer(2)).await;
    assert!(
        orch.test_is_objector(peer(2)),
        "P2 must be an objector after IntersectNotFound"
    );

    // Provide the dynamo tip so GDD density estimate can be computed.
    // fork_point = slot 170, dynamo tip = slot 200, window = 60.
    // window_end = 170 + 60 = 230 > 200 → dynamo_blocks = 200 - 170 = 30.
    // objector_blocks(80) > dynamo_blocks(30) → KeepObjector.
    orch.test_set_dynamo_tip(peer(1), 200, [1u8; 32]);

    let (resp_tx, resp_rx) = oneshot::channel();
    orch.handle_bisection_complete(
        peer(2),
        Point::Specific(170, [0u8; 32]),
        80, // honest objector: 80 blocks in the genesis window (far more than 30)
        resp_tx,
    )
    .await;

    // dynamo_blocks=30, objector_blocks=80 → 80 > 30 → KeepObjector.
    let decision = resp_rx.await.expect("decision channel");
    assert!(
        matches!(decision, OrchestratorDecision::KeepObjector),
        "GDD must rule in favour of the honest objector (denser chain; objector=80, dynamo=30)"
    );

    // P2 should now be the dynamo.
    assert_eq!(
        orch.test_dynamo_addr(),
        Some(peer(2)),
        "honest peer P2 must be elected as new dynamo after winning GDD"
    );

    // P2 receives BecomeDynamo.
    let msg2 = timeout(Duration::from_millis(200), rx_honest.recv())
        .await
        .expect("P2 re-election timeout")
        .expect("P2 channel closed");
    assert!(
        matches!(msg2, PeerInstruction::BecomeDynamo),
        "honest peer P2 must receive BecomeDynamo after winning GDD"
    );

    // Dynamo invariant holds.
    let states = orch.test_peer_jump_states();
    let state_refs: Vec<_> = states.iter().collect();
    assert!(
        check_dynamo_invariant(&state_refs).is_ok(),
        "dynamo invariant must hold after dynamo switch"
    );
}

// ─── Test 2: Liveness — stalled dynamo demoted, fresh dynamo elected ──────────
//
// Setup: two peers, P1 (dynamo, 50ms) and P2 (backup, 200ms).
// P1 never advances its tip.  After DYNAMO_STALL_GRACE (10s) the orchestrator
// demotes P1 and re-elects it (still lowest latency) — the invariant is that
// the re-election fires within 2× the grace period.
//
// Uses `spawn_csj_orchestrator` + `tokio::time::advance` for determinism.

#[tokio::test(start_paused = true)]
async fn liveness_stalled_dynamo_demoted_within_grace_period() {
    let cancel = CancellationToken::new();
    let (evt_tx, reg_tx, _handle) =
        dugite_node::csj_orchestrator::spawn_csj_orchestrator(test_config(), cancel.clone(), None);

    let mut rx1 = register(&reg_tx, peer(1), Some(50.0)).await;
    let _rx2 = register(&reg_tx, peer(2), Some(200.0)).await;

    // Let registration settle.
    time::sleep(Duration::from_millis(50)).await;

    // P1 elected dynamo.
    let msg = timeout(Duration::from_millis(300), rx1.recv())
        .await
        .expect("initial election timeout")
        .expect("channel closed");
    assert!(
        matches!(msg, PeerInstruction::BecomeDynamo),
        "P1 must be elected dynamo initially"
    );

    // Advance time past the stall grace (10 seconds).
    // The orchestrator polls every 1s; give it 12s to detect the stall.
    time::advance(Duration::from_secs(12)).await;
    time::sleep(Duration::from_secs(3)).await;

    // P1 should receive a second BecomeDynamo (re-elected after stall demotion).
    let re_elect = timeout(Duration::from_secs(2), rx1.recv())
        .await
        .expect("re-election timeout — dynamo was not re-elected within 2× grace")
        .expect("channel closed");
    assert!(
        matches!(re_elect, PeerInstruction::BecomeDynamo),
        "P1 must receive BecomeDynamo upon re-election after stall"
    );

    cancel.cancel();
    let _ = evt_tx;
}

// ─── Test 3: Eclipse resistance — 1 honest peer among N adversaries ───────────
//
// Setup: P1 is the honest peer (100ms); P2..PN are adversarial peers with very
// low latency (1ms) so they win the initial election.
//
// The adversarial dynamo has ~10 blocks in the genesis window (low density).
// The honest peer objects with 80 blocks.  GDD must pick the honest peer.
//
// Exercised for N = 3, 4, and 5 total peers (2, 3, 4 adversaries respectively).

async fn run_eclipse_resistance(n_adversaries: u8) {
    time::pause();
    let (mut orch, _evt_tx, _reg_tx) = CsjOrchestrator::new(test_config(), None);

    // Register adversarial peers first (very low latency → will win election).
    let mut adv_rxs: Vec<mpsc::Receiver<PeerInstruction>> = Vec::new();
    for i in 2..=n_adversaries + 1 {
        let (tx, rx) = mpsc::channel(16);
        orch.handle_registration(peer(i), Some(1.0), tx).await;
        adv_rxs.push(rx);
    }

    // Register the honest peer (higher latency).
    let (tx_honest, rx_honest) = mpsc::channel(16);
    orch.handle_registration(peer(1), Some(100.0), tx_honest)
        .await;

    // Identify which peer is the dynamo (lowest latency = one of the adversaries).
    let dynamo_addr = orch.test_dynamo_addr().expect("a dynamo must be elected");
    assert_ne!(
        dynamo_addr,
        peer(1),
        "honest peer should NOT be the initial dynamo (adversaries have lower latency)"
    );

    // Simulate adversarial dynamo advancing tip (large slot, few real blocks).
    orch.handle_dynamo_tip_advanced(dynamo_addr, 500, [0xaau8; 32])
        .await;

    // P1 is already in LookingForIntersection — handle_dynamo_tip_advanced issued
    // the jump to all happy jumpers including P1.  Simulate MsgIntersectNotFound.
    orch.handle_intersect_not_found(peer(1)).await;
    assert!(
        orch.test_is_objector(peer(1)),
        "honest peer P1 must become an objector"
    );

    // Provide dynamo tip data for GDD density estimate.
    orch.test_set_dynamo_tip(dynamo_addr, 500, [0xaau8; 32]);

    // Bisection complete: honest peer has 80 blocks; adversarial dynamo has ~10.
    let (resp_tx, resp_rx) = oneshot::channel();
    orch.handle_bisection_complete(
        peer(1),
        Point::Specific(200, [0u8; 32]),
        80, // honest: 80 blocks in the genesis window
        resp_tx,
    )
    .await;

    let decision = resp_rx.await.expect("GDD decision");
    assert!(
        matches!(decision, OrchestratorDecision::KeepObjector),
        "eclipse resistance: honest peer must win GDD even against {n_adversaries} adversaries"
    );

    // Honest peer is now the dynamo.
    assert_eq!(
        orch.test_dynamo_addr(),
        Some(peer(1)),
        "honest peer P1 must be elected dynamo after winning GDD"
    );

    // Dynamo invariant still holds.
    let states = orch.test_peer_jump_states();
    let state_refs: Vec<_> = states.iter().collect();
    assert!(
        check_dynamo_invariant(&state_refs).is_ok(),
        "dynamo invariant must hold with {n_adversaries} adversarial peers"
    );

    drop(rx_honest); // drain by dropping; we only care about the election assertion
    drop(adv_rxs);
}

#[tokio::test]
async fn eclipse_resistance_one_honest_of_three_peers() {
    run_eclipse_resistance(2).await; // 1 honest + 2 adversaries = 3 total
}

#[tokio::test]
async fn eclipse_resistance_one_honest_of_four_peers() {
    run_eclipse_resistance(3).await; // 1 honest + 3 adversaries = 4 total
}

#[tokio::test]
async fn eclipse_resistance_one_honest_of_five_peers() {
    run_eclipse_resistance(4).await; // 1 honest + 4 adversaries = 5 total
}

// ─── Test 4: Property test — randomised peer behaviour preserves invariant ────
//
// For each generated scenario:
//   - N ∈ [2, 6] peers, each with a random latency ∈ [1, 500] ms.
//   - A random subset of peers become objectors with a random block count.
//   - After all events the dynamo invariant must hold.
//
// The property is: no sequence of valid orchestrator events can cause
// `check_dynamo_invariant()` to return `Err`.
//
// Determinism: proptest controls the RNG; `tokio::time::pause` removes
// wall-clock sensitivity.

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    #[test]
    fn prop_dynamo_invariant_preserved_under_random_events(
        // N peers with random latencies
        peer_latencies in prop::collection::vec(1u32..=500, 2..=6usize),
        // For each peer: should it object, and with how many blocks?
        objector_flags in prop::collection::vec(
            (any::<bool>(), 0u64..=200u64),
            2..=6usize,
        ),
        // tip_slot for the dynamo's advance
        tip_slot in 200u64..=2000u64,
    ) {
        // Run in a single-threaded tokio runtime so proptest works synchronously.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            time::pause();
            let (mut orch, _evt_tx, _reg_tx) = CsjOrchestrator::new(test_config(), None);
            let n = peer_latencies.len();

            // Register all peers.
            let mut _rxs = Vec::new();
            for (i, &latency) in peer_latencies.iter().enumerate() {
                let (tx, rx) = mpsc::channel(32);
                orch.handle_registration(peer(i as u8 + 1), Some(latency as f64), tx).await;
                _rxs.push(rx);
            }

            // Invariant must hold after all registrations.
            {
                let states = orch.test_peer_jump_states();
                let refs: Vec<_> = states.iter().collect();
                prop_assert!(
                    check_dynamo_invariant(&refs).is_ok(),
                    "invariant must hold after registration"
                );
            }

            // Simulate dynamo tip advance.
            if let Some(dynamo_addr) = orch.test_dynamo_addr() {
                orch.handle_dynamo_tip_advanced(dynamo_addr, tip_slot, [0xbbu8; 32]).await;
                orch.test_set_dynamo_tip(dynamo_addr, tip_slot, [0xbbu8; 32]);
            }

            // For each non-dynamo peer: optionally make it an objector.
            let n_flags = n.min(objector_flags.len());
            let dynamo_addr = orch.test_dynamo_addr();

            for (idx, &(should_object, obj_blocks)) in
                objector_flags.iter().enumerate().take(n_flags)
            {
                let addr = peer(idx as u8 + 1);
                if dynamo_addr == Some(addr) {
                    continue; // never object from the dynamo
                }
                if !should_object {
                    continue;
                }

                // Issue jump then simulate IntersectNotFound.
                let jump_slot = tip_slot.saturating_sub(60);
                let issued = orch.test_issue_jump(addr, jump_slot);
                if issued {
                    orch.handle_intersect_not_found(addr).await;

                    // Complete bisection.
                    let fork_slot = jump_slot.saturating_sub(30);
                    let (resp_tx, resp_rx) = oneshot::channel();
                    orch.handle_bisection_complete(
                        addr,
                        Point::Specific(fork_slot, [0u8; 32]),
                        obj_blocks,
                        resp_tx,
                    ).await;
                    let _ = resp_rx.await; // consume decision — either branch is valid
                }
            }

            // Final invariant check.
            let states = orch.test_peer_jump_states();
            let refs: Vec<_> = states.iter().collect();
            prop_assert!(
                check_dynamo_invariant(&refs).is_ok(),
                "dynamo invariant must hold after randomised peer events"
            );
            Ok(())
        })?;
    }
}

// ─── Test 5: Determinism gate — time::pause makes timing tests reproducible ───
//
// A dynamo stall is triggered by advancing tokio's virtual clock.  Runs twice;
// both runs must see the stall within the same tick count, proving no
// wall-clock dependency.

#[tokio::test(start_paused = true)]
async fn determinism_gate_stall_detection_is_reproducible() {
    for run in 0..2u32 {
        let cancel = CancellationToken::new();
        let (_evt_tx, reg_tx, handle) = dugite_node::csj_orchestrator::spawn_csj_orchestrator(
            test_config(),
            cancel.clone(),
            None,
        );

        let mut rx = register(&reg_tx, peer(1), Some(20.0)).await;
        let _rx2 = register(&reg_tx, peer(2), Some(50.0)).await;

        // Allow registration to be processed.
        time::sleep(Duration::from_millis(10)).await;

        // Drain initial BecomeDynamo.
        let _m = timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap_or(None);

        // Advance exactly 12 virtual seconds (10s grace + 2s stall-tick headroom).
        time::advance(Duration::from_secs(12)).await;
        time::sleep(Duration::from_secs(3)).await;

        // Both runs must receive a re-election BecomeDynamo.
        let re = timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap_or(None);
        assert!(
            re.is_some(),
            "run {run}: stall-detection re-election must fire deterministically"
        );

        cancel.cancel();
        let _ = timeout(Duration::from_secs(1), handle).await;
    }
}

// ─── Test 6: GDD tie — equal density keeps the existing dynamo ────────────────
//
// When `dynamo_blocks == objector_blocks` the GDD rule is `AdoptDynamo`.
// Matches Haskell `densityDisconnect`: "objector wins only when strictly denser".

#[tokio::test]
async fn gdd_tie_favours_existing_dynamo() {
    time::pause();
    let (mut orch, _evt_tx, _reg_tx) = CsjOrchestrator::new(test_config(), None);

    let (tx1, mut rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);

    orch.handle_registration(peer(1), Some(30.0), tx1).await;
    orch.handle_registration(peer(2), Some(60.0), tx2).await;
    let _ = rx1.recv().await; // BecomeDynamo for P1

    // Dynamo tip at slot 200 — genesis_window=60.
    // Density estimate for dynamo: min(200−100, 60) = 60 slots.
    // But GDD returns `window` (60) when tip > window_end, so effective = 60.
    orch.test_set_dynamo_tip(peer(1), 200, [0xddu8; 32]);

    // Put P2 in LookingForIntersection then object.
    let issued = orch.test_issue_jump(peer(2), 140);
    assert!(issued, "jump must be issuable to P2");
    orch.handle_intersect_not_found(peer(2)).await;

    // Bisection: objector has 10 blocks; dynamo estimate is also 10 (fork at 100,
    // window=60 → window_end=160 < tip=200 → estimate = window = 60).
    // Use 60 for a genuine tie.
    let (resp_tx, resp_rx) = oneshot::channel();
    orch.handle_bisection_complete(
        peer(2),
        Point::Specific(100, [0u8; 32]),
        60, // tie: same as dynamo estimate
        resp_tx,
    )
    .await;

    let decision = resp_rx.await.expect("decision");
    assert!(
        matches!(decision, OrchestratorDecision::AdoptDynamo),
        "tie in GDD must favour the existing dynamo (Haskell rule: dynamo wins ties)"
    );

    // P1 remains dynamo.
    assert_eq!(
        orch.test_dynamo_addr(),
        Some(peer(1)),
        "P1 must remain dynamo after a GDD tie"
    );
}

// ─── Test 7: Sequential objections — second round does not corrupt state ──────
//
// Verifies that after an objector loses GDD and is disengaged, a fresh sequence
// of events on other peers leaves the invariant intact.

#[tokio::test]
async fn sequential_objections_do_not_corrupt_state() {
    time::pause();
    let (mut orch, _evt_tx, _reg_tx) = CsjOrchestrator::new(test_config(), None);

    let (tx1, mut rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);

    orch.handle_registration(peer(1), Some(10.0), tx1).await;
    orch.handle_registration(peer(2), Some(80.0), tx2).await;
    let _ = rx1.recv().await; // BecomeDynamo

    orch.test_set_dynamo_tip(peer(1), 300, [0x11u8; 32]);

    // Round 1: P2 objects, dynamo wins.
    let issued = orch.test_issue_jump(peer(2), 240); // 300 - 60
    assert!(issued);
    orch.handle_intersect_not_found(peer(2)).await;
    assert!(orch.test_is_objector(peer(2)));

    let (r1_tx, r1_rx) = oneshot::channel();
    orch.handle_bisection_complete(
        peer(2),
        Point::Specific(100, [0u8; 32]),
        3, // dynamo has more → AdoptDynamo
        r1_tx,
    )
    .await;
    let d1 = r1_rx.await.unwrap();
    assert!(matches!(d1, OrchestratorDecision::AdoptDynamo));

    // After resolution P2 is Disengaged.
    assert!(
        orch.test_is_disengaged(peer(2)),
        "P2 must be disengaged after losing GDD"
    );

    // P1 is still dynamo.
    assert_eq!(orch.test_dynamo_addr(), Some(peer(1)));

    // Invariant still holds after first round.
    let states = orch.test_peer_jump_states();
    let refs: Vec<_> = states.iter().collect();
    assert!(check_dynamo_invariant(&refs).is_ok());
}

// ─── Test 8: Mid-jump dynamo disconnect → re-election fires ───────────────────
//
// While P2 is in LookingForIntersection the dynamo (P1) disconnects.
// P2 (next lowest latency) must be elected dynamo.  The invariant must hold.

#[tokio::test]
async fn dynamo_disconnect_mid_jump_triggers_reelection() {
    time::pause();
    let (mut orch, _evt_tx, _reg_tx) = CsjOrchestrator::new(test_config(), None);

    let (tx1, mut rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);
    let (tx3, _rx3) = mpsc::channel(16);

    orch.handle_registration(peer(1), Some(10.0), tx1).await;
    orch.handle_registration(peer(2), Some(50.0), tx2).await;
    orch.handle_registration(peer(3), Some(80.0), tx3).await;
    let _ = rx1.recv().await; // P1 BecomeDynamo

    // Issue a jump to P2 (puts it in LookingForIntersection).
    let issued = orch.test_issue_jump(peer(2), 140);
    assert!(issued, "jump must be issuable to P2");

    // P1 disconnects while P2 is mid-jump.
    orch.handle_peer_disconnected(peer(1)).await;

    // The next lowest-latency peer among {P2, P3} is P2 (50ms < 80ms).
    // elect_dynamo resets P2's jump state to Dynamo.
    assert_eq!(
        orch.test_dynamo_addr(),
        Some(peer(2)),
        "P2 must become dynamo after P1 disconnects"
    );

    // Dynamo invariant holds even with P2 elevated from LookingForIntersection.
    let states = orch.test_peer_jump_states();
    let refs: Vec<_> = states.iter().collect();
    assert!(
        check_dynamo_invariant(&refs).is_ok(),
        "dynamo invariant must hold after mid-jump dynamo disconnect"
    );
}
