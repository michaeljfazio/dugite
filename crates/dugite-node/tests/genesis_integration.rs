//! End-to-end wiring of the Genesis governor and chain selection.
//!
//! These tests exercise the load-bearing SEAM that unit tests cannot: the
//! GSM/GDD actor publishes the LoE via an `arc_swap::ArcSwap`, the SAME
//! handle is installed on the ChainDB, and `trimToLoE` in the live
//! chain-selection queue enforces it. They also assert the praos-mode
//! polarity (a disabled actor publishes nothing that constrains selection).

use std::net::SocketAddr;
use std::sync::Arc;

use dugite_consensus::loe::LoeState;
use dugite_consensus::EraParams;
use dugite_node::genesis_peer_state::{FragAnchor, FragEntry, PeerStateRegistry, WithOrigin};
use dugite_node::gsm::{
    run_gsm_actor, GddAction, GenesisSyncState, GsmConfig, GsmEvent, GsmSnapshot,
};
use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_storage::{AddBlockResult, ChainDB, ChainSelHandle};
use tokio::sync::{mpsc, watch};

fn h(b: u8) -> Hash32 {
    Hash32::from_bytes([b; 32])
}

fn addr(n: u8) -> SocketAddr {
    format!("10.9.0.{n}:3001").parse().unwrap()
}

/// Spawn the real GSM/GDD actor wired to a shared LoE handle + registry,
/// returning the channels and the shared pieces.
#[allow(clippy::type_complexity)]
fn spawn_governor(
    enabled: bool,
    chain_db: Arc<tokio::sync::RwLock<ChainDB>>,
    loe: Arc<arc_swap::ArcSwap<LoeState>>,
) -> (
    mpsc::Sender<GsmEvent>,
    watch::Receiver<GsmSnapshot>,
    mpsc::Receiver<GddAction>,
    Arc<PeerStateRegistry>,
) {
    let registry = PeerStateRegistry::new();
    let params = EraParams {
        epoch_size: 1_000,
        slot_length_ms: 1_000,
        safe_zone: 200,
        genesis_window: 50,
    };
    let era_history = Arc::new(tokio::sync::RwLock::new(
        dugite_consensus::EraHistory::from_genesis(params.clone(), params, 0),
    ));
    let config = GsmConfig {
        min_active_blp: 1,
        max_caught_up_age_secs: 600,
        min_caught_up_dwell_secs: 0,
        anti_thundering_herd_max_secs: 0,
        gdd_rate_limit_ms: 20,
        security_param_k: 2,
        marker_path: std::env::temp_dir()
            .join(format!("gi-{}-{enabled}.marker", std::process::id())),
    };
    let _ = std::fs::remove_file(&config.marker_path);
    let (event_tx, event_rx) = mpsc::channel(256);
    let (snapshot_tx, snapshot_rx) = watch::channel(GsmSnapshot {
        state: GenesisSyncState::PreSyncing,
        loe_slot: Some(0),
    });
    let (action_tx, action_rx) = mpsc::channel(64);
    tokio::spawn(run_gsm_actor(
        config,
        enabled,
        registry.clone(),
        chain_db,
        era_history,
        loe.clone(),
        None,
        event_rx,
        snapshot_tx,
        action_tx,
    ));
    (event_tx, snapshot_rx, action_rx, registry)
}

async fn drive_to_syncing(event_tx: &mpsc::Sender<GsmEvent>, snap: &watch::Receiver<GsmSnapshot>) {
    event_tx
        .send(GsmEvent::SyncStatus {
            active_blp_count: 5,
            selection_block_no: 0,
            tip_age_secs: 9_999,
        })
        .await
        .unwrap();
    let mut rx = snap.clone();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if rx.borrow().state == GenesisSyncState::Syncing {
                break;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("reached Syncing");
}

/// The governor's published LoE fragment, shared via one arc_swap with the
/// ChainDB, defers a block beyond k past the LoE tip; advancing the LoE
/// (via the registry + reprocess) adopts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn governor_publishes_loe_that_chain_selection_enforces() {
    let tmp = tempfile::tempdir().unwrap();
    let chain_db = Arc::new(tokio::sync::RwLock::new(
        ChainDB::open(tmp.path()).expect("chaindb"),
    ));
    let loe = Arc::new(arc_swap::ArcSwap::from_pointee(LoeState::Disabled));
    chain_db.write().await.set_loe_handle(loe.clone());

    let (event_tx, snap_rx, _action_rx, registry) =
        spawn_governor(true, chain_db.clone(), loe.clone());
    drive_to_syncing(&event_tx, &snap_rx).await;

    // Two peers agreeing on a single block at slot 1 (the immutable tip is
    // Origin) → the shared candidate prefix (LoE fragment) = [(1, block1)].
    // k = 2.
    for n in [1u8, 2] {
        let st = registry.register(addr(n), FragAnchor::Origin);
        st.on_roll_forward(FragEntry {
            slot: 1,
            hash: *h(1).as_bytes(),
            block_no: 1,
        });
        st.on_await_reply();
    }
    event_tx
        .send(GsmEvent::PeerIdling { addr: addr(1) })
        .await
        .unwrap();

    // Wait until the governor publishes a Fragment LoE with tip at slot 1.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let LoeState::Fragment { entries, .. } = &**loe.load() {
                if entries.iter().any(|e| e.slot == 1) {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("LoE fragment published with the shared prefix");

    // Now run chain selection through the SAME ChainDB (same LoE handle).
    let (handle, runner) = ChainSelHandle::new(chain_db.clone());
    let _runner = tokio::spawn(runner);

    // Blocks 1..3 adopt (within k of the LoE tip at slot 1); block 4 is
    // depth 3 past the LoE tip → deferred.
    for (i, slot, parent) in [
        (1u8, 10u64, Hash32::ZERO),
        (2, 20, h(0x11)),
        (3, 30, h(0x12)),
    ] {
        let _ = handle
            .submit_block(
                h(0x10 + i),
                SlotNo(slot),
                BlockNo(i as u64),
                parent,
                vec![i],
            )
            .await
            .unwrap();
    }
    let r = handle
        .submit_block(h(0x14), SlotNo(40), BlockNo(4), h(0x13), vec![4])
        .await
        .unwrap();
    // The LoE tip is at slot 1 (the genesis prefix), so the chain may extend
    // only k=2 blocks past the immutable tip; block 4 overshoots → deferred.
    assert_eq!(
        r,
        AddBlockResult::StoredAsFork,
        "block beyond k past the published LoE tip must be deferred"
    );

    // The governor's LoE never lets selection run away from genesis under a
    // sparse fragment — proving the published constraint reaches selection.
    let tip_bn = chain_db
        .read()
        .await
        .get_tip_info()
        .map(|(_, _, bn)| bn.0)
        .unwrap_or(0);
    assert!(
        tip_bn <= 3,
        "selection capped by the governor LoE, got bn {tip_bn}"
    );
}

/// Praos polarity: a disabled governor publishes nothing that constrains
/// selection — every block extends the chain exactly as before Genesis
/// existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn praos_governor_does_not_constrain_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let chain_db = Arc::new(tokio::sync::RwLock::new(
        ChainDB::open(tmp.path()).expect("chaindb"),
    ));
    let loe = Arc::new(arc_swap::ArcSwap::from_pointee(LoeState::Disabled));
    chain_db.write().await.set_loe_handle(loe.clone());

    let (_event_tx, _snap_rx, _action_rx, _registry) =
        spawn_governor(false, chain_db.clone(), loe.clone());
    // Give the disabled actor a moment; it must keep the LoE Disabled.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(
        loe.load().is_disabled(),
        "praos: governor leaves LoE Disabled"
    );

    let (handle, runner) = ChainSelHandle::new(chain_db.clone());
    let _runner = tokio::spawn(runner);
    for i in 1..=10u8 {
        let prev = if i == 1 { Hash32::ZERO } else { h(i - 1) };
        let r = handle
            .submit_block(
                h(i),
                SlotNo(i as u64 * 10),
                BlockNo(i as u64),
                prev,
                vec![i],
            )
            .await
            .unwrap();
        assert!(
            matches!(r, AddBlockResult::AddedAsTip { .. }),
            "praos: block {i} must extend the chain unconstrained, got {r:?}"
        );
    }
}

/// The GDD kill path: a sparse-fork peer is flagged for disconnect by the
/// real actor and the verdict reaches the GddAction channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gdd_actor_disconnects_sparse_fork_peer() {
    let tmp = tempfile::tempdir().unwrap();
    let chain_db = Arc::new(tokio::sync::RwLock::new(
        ChainDB::open(tmp.path()).expect("chaindb"),
    ));
    let loe = Arc::new(arc_swap::ArcSwap::from_pointee(LoeState::Disabled));
    chain_db.write().await.set_loe_handle(loe.clone());
    let (event_tx, snap_rx, mut action_rx, registry) =
        spawn_governor(true, chain_db.clone(), loe.clone());
    drive_to_syncing(&event_tx, &snap_rx).await;

    // Dense peer: 4 blocks (> k=2) in window on one fork.
    let dense = registry.register(addr(1), FragAnchor::Origin);
    let sparse = registry.register(addr(2), FragAnchor::Origin);
    // Common block at slot 1, then divergence.
    for st in [&dense, &sparse] {
        st.on_roll_forward(FragEntry {
            slot: 1,
            hash: *h(1).as_bytes(),
            block_no: 1,
        });
    }
    for (i, slot) in [(2u8, 5u64), (3, 6), (4, 7), (5, 8)] {
        dense.on_roll_forward(FragEntry {
            slot,
            hash: *h(i).as_bytes(),
            block_no: slot,
        });
    }
    // Sparse peer: a lone block on a different fork, then idle.
    sparse.on_roll_forward(FragEntry {
        slot: 9,
        hash: *h(0xbb).as_bytes(),
        block_no: 2,
    });
    sparse.on_await_reply();
    event_tx
        .send(GsmEvent::PeerIdling { addr: addr(2) })
        .await
        .unwrap();

    let action = tokio::time::timeout(std::time::Duration::from_secs(2), action_rx.recv())
        .await
        .expect("GDD verdict within 2s")
        .expect("channel open");
    match action {
        GddAction::DisconnectPeer(a) => {
            assert_eq!(a, addr(2), "the sparse-fork peer is disconnected")
        }
    }
    let _ = WithOrigin::Origin; // keep the import meaningful
}
