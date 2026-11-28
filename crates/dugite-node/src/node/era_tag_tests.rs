//! A peer-supplied block's era tag must not steer the ledger (#1233).
//!
//! These tests drive the REAL node path: a `Node` built by `Node::new` on a
//! temporary database, a block pushed through the `fetched_blocks_rx` channel,
//! and `apply_fetched_block` (ChainSelQueue -> ChainDB -> header validation ->
//! ledger apply -> rollback on failure). Nothing is mocked below that.
//!
//! The block bytes come from the real forge (`forge_block`) with a pool that
//! sits in the `set` snapshot and holds real VRF, KES and opcert keys, so the
//! ONLY thing that can reject a re-tagged copy is the era check: every case
//! first proves the honest, untagged copy of the same bytes passes
//! `validate_peer_header_full`.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use dugite_crypto::keys::PaymentSigningKey;
use dugite_crypto::{kes, vrf};
use dugite_ledger::state::{PoolRegistration, StakeSnapshot};
use dugite_primitives::block::{Block, Point, ProtocolVersion};
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_224, blake2b_256, Hash32};
use dugite_primitives::time::{BlockNo, EpochNo, SlotNo};
use dugite_primitives::value::Lovelace;
use tokio::sync::mpsc;

use super::connection_lifecycle::{FetchedBlock, PeerFailureKind};
use super::{Node, NodeArgs, StoredBlockPeers};
use crate::config::NodeConfig;
use crate::forge::{forge_block, BlockProducerConfig, BlockProducerCredentials};
use crate::topology::Topology;

const EPOCH_LENGTH: u64 = 100;
const SLOTS_PER_KES_PERIOD: u64 = 129_600;

/// A devnet-shaped Shelley genesis. `systemStart` is far in the past so every
/// slot the tests use is behind the wall clock (no FutureBlock).
fn shelley_genesis_json(pv_major: u64) -> String {
    format!(
        r#"{{
  "systemStart": "2020-01-01T00:00:00Z",
  "networkMagic": 42,
  "networkId": "Testnet",
  "activeSlotsCoeff": 0.5,
  "securityParam": 40,
  "epochLength": {EPOCH_LENGTH},
  "slotLength": 1,
  "maxLovelaceSupply": 60000000000000000,
  "maxKESEvolutions": 62,
  "slotsPerKESPeriod": {SLOTS_PER_KES_PERIOD},
  "updateQuorum": 2,
  "protocolParams": {{
    "minFeeA": 44, "minFeeB": 155381, "maxBlockBodySize": 90112,
    "maxTxSize": 16384, "maxBlockHeaderSize": 1100, "keyDeposit": 2000000,
    "poolDeposit": 500000000, "minPoolCost": 340000000, "eMax": 18,
    "nOpt": 500, "a0": 0.3, "rho": 0.003, "tau": 0.2,
    "protocolVersion": {{ "major": {pv_major}, "minor": 0 }}
  }}
}}"#
    )
}

/// Real keys for one pool: Ed25519 cold key, VRF, Sum6Kes, and an opcert signed
/// by the cold key.
fn pool_credentials() -> (BlockProducerCredentials, Hash32) {
    let cold_sk = PaymentSigningKey::from_bytes(&[0x11; 32]).expect("cold key");
    let cold_vkey = cold_sk.verification_key().to_bytes().to_vec();
    let pool_id = blake2b_224(&cold_vkey);

    let vrf_kp = vrf::generate_vrf_keypair_from_secret(&[0x22; 32]);
    let (kes_sk, kes_pk) = kes::kes_keygen(&[0x33; 32]).expect("kes keygen");

    let (opcert_seq, opcert_kes_period) = (0u64, 0u64);
    let signable =
        dugite_crypto::ocert::ocert_signable_bytes(&kes_pk, opcert_seq, opcert_kes_period);
    let sigma = cold_sk.sign(&signable);

    let vrf_keyhash = blake2b_256(&vrf_kp.public_key);
    (
        BlockProducerCredentials {
            vrf_skey: *vrf_kp.secret_key(),
            vrf_vkey: vrf_kp.public_key,
            cold_vkey,
            kes_skey: kes_sk,
            kes_vkey: kes_pk.to_vec(),
            opcert_sequence: opcert_seq,
            opcert_kes_period,
            opcert_sigma: sigma,
            pool_id,
        },
        vrf_keyhash,
    )
}

struct Harness {
    node: Node,
    creds: BlockProducerCredentials,
    tx: mpsc::Sender<FetchedBlock>,
    peer_rx: mpsc::Receiver<(std::net::SocketAddr, PeerFailureKind)>,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A node whose ledger is at protocol version `pv_major`, with the test
    /// pool holding 100% of the `set` snapshot.
    async fn new(pv_major: u64) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .try_init();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("shelley-genesis.json"),
            shelley_genesis_json(pv_major),
        )
        .expect("write genesis");

        let config = NodeConfig {
            shelley_genesis_file: Some("shelley-genesis.json".into()),
            network_magic: Some(42),
            ..NodeConfig::default()
        };
        let node = Node::new(NodeArgs {
            config,
            topology: Topology::default(),
            topology_path: dir.path().join("topology.json"),
            config_path: dir.path().join("config.json"),
            database_path: dir.path().join("db"),
            socket_path: dir.path().join("node.sock"),
            host_addr: "127.0.0.1".into(),
            port: 0,
            config_dir: dir.path().to_path_buf(),
            shelley_kes_key: None,
            shelley_vrf_key: None,
            shelley_operational_certificate: None,
            _shelley_cold_key: None,
            rpc_config: None,
            metrics_port: 0,
            require_metrics: false,
            compat_metrics: false,
            liveness_threshold_secs: 0,
            mempool_max_tx: 100,
            mempool_max_bytes: 1 << 20,
            snapshot_max_retained: 2,
            snapshot_bulk_min_blocks: 50_000,
            snapshot_bulk_min_secs: 360,
            storage_config: dugite_storage::StorageConfig::default(),
            consensus_mode: "praos".into(),
            validate_all_blocks: false,
            skip_eagerly_validated_header_crypto: false,
            log_handle: None,
        })
        .expect("Node::new on a temp database");

        let (creds, vrf_keyhash) = pool_credentials();

        // The pool holds all stake in the `set` snapshot (leader election).
        {
            let mut ls = node.ledger_state.write().await;
            assert_eq!(ls.epochs.protocol_params.protocol_version_major, pv_major);
            let reg = PoolRegistration {
                pool_id: creds.pool_id,
                vrf_keyhash,
                pledge: Lovelace(0),
                cost: Lovelace(0),
                margin_numerator: 0,
                margin_denominator: 1,
                reward_account: vec![0xe0; 29],
                owners: vec![],
                relays: vec![],
                metadata_url: None,
                metadata_hash: None,
            };
            let mut snap = StakeSnapshot::empty(EpochNo(0));
            snap.pool_stake.insert(creds.pool_id, Lovelace(1_000_000));
            snap.pool_params = Arc::new(HashMap::from([(creds.pool_id, reg)]));
            ls.epochs.snapshots.set = Some(snap);
        }

        let (tx, rx) = mpsc::channel(16);
        let mut node = node;
        node.fetched_blocks_rx = Some(rx);
        let (peer_tx, peer_rx) = mpsc::channel(16);
        node.peer_failure_tx = Some(peer_tx);
        Harness {
            node,
            creds,
            tx,
            peer_rx,
            _dir: dir,
        }
    }

    /// Make the ledger know that protocol version `major` is adopted at the next
    /// epoch boundary: a quorum of genesis delegates voted for it (pre-Conway
    /// PPUP, the way every hard fork up to Conway was enacted).
    async fn schedule_hard_fork(&self, major: u64) {
        use dugite_primitives::transaction::ProtocolParamUpdate;
        let mut ls = self.node.ledger_state.write().await;
        let ppu = ProtocolParamUpdate {
            protocol_version_major: Some(major),
            protocol_version_minor: Some(0),
            ..Default::default()
        };
        let epoch = ls.epoch;
        ls.epochs.pending_pp_updates.insert(
            epoch,
            vec![
                (Hash32::from_bytes([1; 32]), ppu.clone()),
                (Hash32::from_bytes([2; 32]), ppu),
            ],
        );
        assert_eq!(
            ls.era_due_at_next_boundary(),
            Era::from_protocol_major(major)
        );
    }

    async fn ledger_era(&self) -> Era {
        self.node.ledger_state.read().await.era
    }

    async fn ledger_tip_point(&self) -> Point {
        self.node.ledger_state.read().await.tip.point.clone()
    }

    async fn chain_db_tip_point(&self) -> Point {
        self.node.chain_db.read().await.get_tip().point
    }

    fn header_failures(&self) -> u64 {
        self.node
            .metrics
            .header_validation_failures_total
            .load(Ordering::Relaxed)
    }

    async fn has_block(&self, hash: &Hash32) -> bool {
        self.node.chain_db.read().await.has_block(hash)
    }

    async fn is_marked_invalid(&self, hash: &Hash32) -> bool {
        let handle = self.node.chain_sel_handle.as_ref().expect("chain sel");
        let mut cache = handle.invalid_cache.write().await;
        cache.get(hash).is_some()
    }

    /// Forge a block with the REAL forge at `slot`, as `era`, and return its
    /// wire bytes. Returns `None` if the pool is not the slot leader.
    async fn forge_at(
        &mut self,
        era: Era,
        pv_major: u64,
        slot: u64,
        block_no: u64,
        prev_hash: Hash32,
    ) -> Vec<u8> {
        let nonce = self
            .node
            .ledger_state
            .read()
            .await
            .epoch_nonce_for_slot(slot);
        let cfg = BlockProducerConfig {
            protocol_version: ProtocolVersion {
                major: pv_major,
                minor: 0,
            },
            era,
            slots_per_kes_period: SLOTS_PER_KES_PERIOD,
            max_kes_evolutions: 62,
            ..BlockProducerConfig::default()
        };
        let (_, cbor) = forge_block(
            &self.creds,
            &cfg,
            SlotNo(slot),
            BlockNo(block_no),
            prev_hash,
            &nonce,
            vec![],
        )
        .expect("forge");
        cbor
    }

    fn decode(&self, cbor: &[u8]) -> Result<Block, dugite_serialization::SerializationError> {
        dugite_serialization::decode_block_with_byron_epoch_length(
            cbor,
            self.node.byron_epoch_length,
        )
    }

    /// First slot at or after `from` where the pool is elected AND the honest
    /// block passes the node's full header validation. Returns the honest
    /// bytes (era `era`).
    async fn honest_leader_block(
        &mut self,
        era: Era,
        pv_major: u64,
        from: u64,
        to: u64,
        block_no: u64,
        prev_hash: Hash32,
    ) -> (u64, Vec<u8>) {
        let mut last_err = String::new();
        for slot in from..to {
            let cbor = self
                .forge_at(era, pv_major, slot, block_no, prev_hash)
                .await;
            let block = self.decode(&cbor).expect("honest block decodes");
            assert_eq!(block.era, era);
            match self.node.validate_peer_header_full(&block).await {
                Ok(()) => return (slot, cbor),
                Err(e) => last_err = e.to_string(),
            }
        }
        panic!("pool was never elected in slots {from}..{to}; last error: {last_err}");
    }

    /// Push `cbor` through the real channel and apply it exactly as the run
    /// loop does.
    async fn deliver(&mut self, cbor: &[u8]) -> Block {
        self.deliver_from("127.0.0.1:3001".parse().unwrap(), cbor)
            .await
    }

    /// As [`Self::deliver`], served by `peer`.
    async fn deliver_from(&mut self, peer: std::net::SocketAddr, cbor: &[u8]) -> Block {
        let block = self.decode(cbor).expect("block decodes");
        let fetched = FetchedBlock {
            peer,
            block: block.clone(),
            tip_slot: block.slot().0,
            tip_hash: *block.hash().as_bytes(),
            tip_block_number: block.block_number().0,
        };
        self.tx.send(fetched).await.expect("channel open");
        let received = self
            .node
            .fetched_blocks_rx
            .as_mut()
            .expect("rx")
            .recv()
            .await
            .expect("block on channel");
        self.node.apply_fetched_block(received).await;
        block
    }

    /// Apply the first (genesis) block, which fixes the ledger era, and return
    /// `(slot, hash)` of the new tip.
    async fn bootstrap(&mut self, era: Era, pv_major: u64) -> (u64, Hash32) {
        let (slot, cbor) = self
            .honest_leader_block(era, pv_major, 1, 200, 1, Hash32::ZERO)
            .await;
        let block = self.deliver(&cbor).await;
        assert_eq!(self.ledger_era().await, era, "genesis block fixes the era");
        assert_eq!(
            self.ledger_tip_point().await,
            Point::Specific(SlotNo(slot), *block.hash()),
            "the bootstrap block must apply"
        );
        (slot, *block.hash())
    }
}

/// Re-wrap forged Conway bytes `[7, body]` under another era tag.
fn retag(cbor: &[u8], new_tag: u8) -> Vec<u8> {
    assert_eq!(&cbor[..2], &[0x82, 0x07], "forged Conway block: [7, body]");
    let mut out = cbor.to_vec();
    out[1] = new_tag;
    out
}

/// Hand-built Byron epoch boundary block `[0, [header, body, extra]]` that
/// extends `prev_hash`. An EBB has no signature, no VRF and no KES: nothing but
/// its wrapper says it is not an ordinary block.
fn byron_ebb(prev_hash: &Hash32, epoch: u64, block_no: u64) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(2).unwrap().u8(0).unwrap(); // [0, ebb]
    e.array(3).unwrap();
    // header = [magic, prev_hash, body_proof, [epoch, [difficulty]], extra]
    e.array(5).unwrap();
    e.u32(42).unwrap();
    e.bytes(prev_hash.as_bytes()).unwrap();
    e.bytes(&[0u8; 32]).unwrap();
    e.array(2).unwrap().u64(epoch).unwrap();
    e.array(1).unwrap().u64(block_no).unwrap();
    e.array(1).unwrap().map(0).unwrap();
    e.array(0).unwrap(); // body: stakeholder ids
    e.array(1).unwrap().map(0).unwrap(); // extra
    e.into_writer()
}

/// Everything a rejected, wrongly tagged block must leave behind, and what it
/// must NOT: the copy shares the honest block's hash, so that hash may be
/// neither blacklisted nor left in the ChainDB.
async fn assert_rejected(
    h: &mut Harness,
    bad: &Block,
    era_before: Era,
    parent: (u64, Hash32),
    failures_before: u64,
) {
    let parent_point = Point::Specific(SlotNo(parent.0), parent.1);
    assert_eq!(h.ledger_era().await, era_before, "ledger era must not move");
    assert_eq!(
        h.ledger_tip_point().await,
        parent_point,
        "ledger tip is the parent"
    );
    assert_eq!(
        h.chain_db_tip_point().await,
        parent_point,
        "ChainDB tip is the parent"
    );
    assert!(
        !h.is_marked_invalid(bad.hash()).await,
        "the hash must NOT be blacklisted: the era tag is not part of it"
    );
    assert!(
        !h.has_block(bad.hash()).await,
        "the re-tagged bytes must not stay in the ChainDB under that hash"
    );
    assert_eq!(
        h.header_failures(),
        failures_before + 1,
        "header-validation failure counted exactly once"
    );
    let (_peer, kind) = h.peer_rx.try_recv().expect("the serving peer is reported");
    assert_eq!(kind, PeerFailureKind::ProtocolFault);
    assert!(h.peer_rx.try_recv().is_err(), "reported exactly once");
}

/// The chain must go on after a rejected block: an honest block at the same
/// height arrives and the ledger ends on it.
async fn assert_honest_block_applies(
    h: &mut Harness,
    era: Era,
    pv: u64,
    parent: (u64, Hash32),
    after_slot: u64,
) {
    let (slot, cbor) = h
        .honest_leader_block(era, pv, after_slot + 1, after_slot + 200, 2, parent.1)
        .await;
    let honest = h.deliver(&cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *honest.hash()),
        "an honest block after a rejected one must apply"
    );
    assert_eq!(h.ledger_era().await, era);
    assert_eq!(h.chain_db_tip_point().await, h.ledger_tip_point().await);
}

/// Core of every re-tagged case.
async fn retagged_case(honest_era: Era, pv: u64, new_tag: u8) {
    let mut h = Harness::new(pv).await;
    let parent = h.bootstrap(honest_era, pv).await;

    // A leader block at the next height, signed by the real pool.
    let (slot, honest_cbor) = h
        .honest_leader_block(honest_era, pv, parent.0 + 1, parent.0 + 200, 2, parent.1)
        .await;
    // Control: the untagged bytes pass full header validation, so the era tag
    // is the ONLY thing wrong with the re-tagged copy.
    let honest_block = h.decode(&honest_cbor).unwrap();
    h.node
        .validate_peer_header_full(&honest_block)
        .await
        .expect("control: honest bytes validate");

    let bad_cbor = retag(&honest_cbor, new_tag);
    let bad_block = h.decode(&bad_cbor).expect("re-tagged bytes decode by tag");
    assert_ne!(
        bad_block.era, honest_era,
        "the tag must change the decoded era"
    );
    let err = h
        .node
        .validate_peer_header_full(&bad_block)
        .await
        .expect_err("a wrongly tagged block must fail header validation");
    assert!(err.to_string().contains("era tag check"), "{err}");
    // Same header bytes, so the same hash as the honest block.
    assert_eq!(bad_block.header.slot, SlotNo(slot));

    let failures = h.header_failures();
    let bad = h.deliver(&bad_cbor).await;
    assert_rejected(&mut h, &bad, honest_era, parent, failures).await;

    // The re-tagged copy arrived FIRST. The honest copy has the SAME hash and
    // must still apply.
    assert_eq!(bad.hash(), honest_block.hash(), "same header, same hash");
    let honest = h.deliver(&honest_cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *honest.hash()),
        "the honest copy with the same hash must apply after the re-tagged one"
    );
    assert_eq!(h.ledger_era().await, honest_era);
    assert_eq!(h.chain_db_tip_point().await, h.ledger_tip_point().await);
}

/// A Babbage tag on a Conway-era block at PV10 (the reported case).
#[tokio::test(flavor = "multi_thread")]
async fn babbage_tag_in_conway_is_rejected() {
    retagged_case(Era::Conway, 10, 6).await;
}

/// A Dijkstra tag at PV10: an early upgrade mid-epoch.
#[tokio::test(flavor = "multi_thread")]
async fn dijkstra_tag_at_pv10_is_rejected() {
    retagged_case(Era::Conway, 10, 8).await;
}

/// A peer-supplied Byron EBB extending the tip, on a Conway ledger. This is the
/// case that needs no stake: an EBB carries no VRF, KES or signature.
#[tokio::test(flavor = "multi_thread")]
async fn byron_ebb_extending_the_tip_is_rejected_on_a_conway_ledger() {
    let mut h = Harness::new(10).await;
    let parent = h.bootstrap(Era::Conway, 10).await;

    let ebb_cbor = byron_ebb(&parent.1, 1, 2);
    let ebb = h.decode(&ebb_cbor).expect("EBB decodes");
    assert_eq!(ebb.era, Era::Byron);
    assert_eq!(*ebb.prev_hash(), parent.1);

    let failures = h.header_failures();
    let bad = h.deliver(&ebb_cbor).await;
    assert_rejected(&mut h, &bad, Era::Conway, parent, failures).await;
    assert_honest_block_applies(&mut h, Era::Conway, 10, parent, parent.0).await;
}

/// FINDING (reported in the PR): on main, a peer-supplied `[0, ebb]` block IS
/// admitted by chain selection and the ChainDB as the selected tip. Nothing
/// before the ledger looks at the tag, so only the era check stands between
/// that block and the ledger.
#[tokio::test(flavor = "multi_thread")]
async fn byron_ebb_is_admitted_by_chain_selection_as_the_tip() {
    let mut h = Harness::new(10).await;
    let parent = h.bootstrap(Era::Conway, 10).await;

    let ebb_cbor = byron_ebb(&parent.1, 1, 2);
    let ebb = h.decode(&ebb_cbor).unwrap();
    let handle = h.node.chain_sel_handle.clone().expect("chain sel");
    let verdict = handle
        .submit_block_with_header(
            *ebb.hash(),
            ebb.slot(),
            ebb.block_number(),
            *ebb.prev_hash(),
            ebb_cbor,
            ebb.header.clone(),
        )
        .await;
    assert!(
        matches!(
            verdict,
            Some(dugite_storage::AddBlockResult::AddedAsTip { .. })
        ),
        "chain selection admits the peer EBB as the tip: {verdict:?}"
    );
    assert_eq!(
        h.chain_db_tip_point().await,
        Point::Specific(ebb.slot(), *ebb.hash()),
        "the ChainDB selected tip is now the EBB"
    );
}

/// A legitimate era boundary must still pass: a Babbage ledger that KNOWS a
/// hard fork to PV9 is due (a quorum of genesis delegates voted for it), then the
/// first block of the next epoch tagged Conway (PV9 header, the +1 the HFC
/// allows).
#[tokio::test(flavor = "multi_thread")]
async fn legitimate_era_boundary_block_is_accepted() {
    let mut h = Harness::new(8).await;
    let parent = h.bootstrap(Era::Babbage, 8).await;
    assert!(parent.0 < EPOCH_LENGTH, "bootstrap block is in epoch 0");
    h.schedule_hard_fork(9).await;

    // First Conway block of epoch 1.
    let (slot, cbor) = h
        .honest_leader_block(
            Era::Conway,
            9,
            EPOCH_LENGTH,
            EPOCH_LENGTH + 100,
            2,
            parent.1,
        )
        .await;
    let before = h.header_failures();
    let block = h.deliver(&cbor).await;
    assert_eq!(block.era, Era::Conway);
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *block.hash()),
        "the era-boundary block must apply"
    );
    assert_eq!(h.ledger_era().await, Era::Conway);
    assert_eq!(h.header_failures(), before, "no header failure");
    assert!(!h.is_marked_invalid(block.hash()).await);
    assert_eq!(
        h.node
            .metrics
            .era_parity_mismatch_total
            .load(Ordering::Relaxed),
        0,
        "the boundary block agrees with the protocol version that was adopted"
    );
}

/// The same Babbage ledger with NO hard fork due: a Conway-tagged first block of
/// the next epoch is not a boundary fork. (The old rule accepted it: any
/// upgrade on the first block of an epoch.)
#[tokio::test(flavor = "multi_thread")]
async fn conway_tag_on_the_first_block_of_an_epoch_with_no_fork_due_is_rejected() {
    let mut h = Harness::new(8).await;
    let parent = h.bootstrap(Era::Babbage, 8).await;
    // Same VRF input as the honest Babbage block at that slot.
    let (slot, _) = h
        .honest_leader_block(
            Era::Babbage,
            8,
            EPOCH_LENGTH,
            EPOCH_LENGTH + 100,
            2,
            parent.1,
        )
        .await;
    let cbor = h.forge_at(Era::Conway, 9, slot, 2, parent.1).await;
    let failures = h.header_failures();
    let bad = h.deliver(&cbor).await;
    assert_rejected(&mut h, &bad, Era::Babbage, parent, failures).await;
    assert_honest_block_applies(&mut h, Era::Babbage, 8, parent, slot).await;
}

/// THE PROBE. On a PV10 Conway ledger the honest first block of epoch 1 is
/// re-tagged 7 -> 8 (Dijkstra). No hard fork is due, so it must be rejected; the
/// honest copy (same hash) must then apply, and so must the next block. (The old
/// rule accepted it, the ledger became Dijkstra at PV10, and the next honest
/// block was rejected as an "era downgrade": a stake-free permanent wedge.)
#[tokio::test(flavor = "multi_thread")]
async fn retagged_first_block_of_an_epoch_with_no_fork_due_is_rejected_and_the_chain_goes_on() {
    let mut h = Harness::new(10).await;
    let parent = h.bootstrap(Era::Conway, 10).await;
    let (slot, honest_cbor) = h
        .honest_leader_block(
            Era::Conway,
            10,
            EPOCH_LENGTH,
            EPOCH_LENGTH + 100,
            2,
            parent.1,
        )
        .await;
    let honest_block = h.decode(&honest_cbor).unwrap();
    assert_eq!(
        h.node.ledger_state.read().await.era_due_at_next_boundary(),
        Era::Conway,
        "no hard fork is due"
    );
    let bad_cbor = retag(&honest_cbor, 8);
    let failures = h.header_failures();
    let bad = h.deliver(&bad_cbor).await;
    assert_eq!(bad.era, Era::Dijkstra);
    assert_rejected(&mut h, &bad, Era::Conway, parent, failures).await;

    let honest = h.deliver(&honest_cbor).await;
    assert_eq!(honest.hash(), honest_block.hash());
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *honest.hash()),
        "the honest first block of the epoch applies"
    );
    assert_eq!(h.ledger_era().await, Era::Conway);

    // And the chain keeps going.
    let (slot3, cbor3) = h
        .honest_leader_block(Era::Conway, 10, slot + 1, slot + 100, 3, *honest.hash())
        .await;
    let next = h.deliver(&cbor3).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot3), *next.hash()),
        "the next honest block applies"
    );
    assert_eq!(h.ledger_era().await, Era::Conway);
}

/// The other half of the same wedge: a hard fork IS due and the first block of
/// the epoch is tagged with the OLD era. Accepting it would leave the ledger in
/// the old era under the new protocol version and reject every honest block
/// after it.
#[tokio::test(flavor = "multi_thread")]
async fn old_era_tag_on_the_first_block_after_a_due_fork_is_rejected() {
    let mut h = Harness::new(8).await;
    let parent = h.bootstrap(Era::Babbage, 8).await;
    h.schedule_hard_fork(9).await;
    // Elected slot, found with the honest Conway block of the due fork.
    let (slot, _) = h
        .honest_leader_block(
            Era::Conway,
            9,
            EPOCH_LENGTH,
            EPOCH_LENGTH + 100,
            2,
            parent.1,
        )
        .await;
    let cbor = h.forge_at(Era::Babbage, 8, slot, 2, parent.1).await;
    let failures = h.header_failures();
    let bad = h.deliver(&cbor).await;
    assert_rejected(&mut h, &bad, Era::Babbage, parent, failures).await;

    // The honest Conway block of the due fork then applies.
    let (cslot, ccbor) = h
        .honest_leader_block(
            Era::Conway,
            9,
            EPOCH_LENGTH,
            EPOCH_LENGTH + 100,
            2,
            parent.1,
        )
        .await;
    let ok = h.deliver(&ccbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(cslot), *ok.hash())
    );
    assert_eq!(h.ledger_era().await, Era::Conway);
}

/// A `TestConwayHardForkAtEpoch` trigger makes the fork due at that epoch with
/// no protocol-version change (cardano-node `TriggerHardForkAtEpoch`).
#[tokio::test(flavor = "multi_thread")]
async fn a_trigger_hard_fork_at_epoch_makes_the_fork_due_without_a_pv_change() {
    let mut h = Harness::new(8).await;
    let parent = h.bootstrap(Era::Babbage, 8).await;
    h.node
        .ledger_state
        .write()
        .await
        .set_hard_fork_triggers([(Era::Conway, 1)].into_iter().collect());
    let (slot, cbor) = h
        .honest_leader_block(
            Era::Conway,
            8,
            EPOCH_LENGTH,
            EPOCH_LENGTH + 100,
            2,
            parent.1,
        )
        .await;
    let block = h.deliver(&cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *block.hash())
    );
    assert_eq!(h.ledger_era().await, Era::Conway);
}

/// The ledger's own check (`apply_block`) with the node's checks bypassed: the
/// wrong-era block is stored, fails ledger apply, and is discarded WITHOUT
/// blacklisting its hash, the peer is reported, and the bytes are gone from the
/// volatile WAL too: reopening the volatile directory does not bring them back
/// under the honest hash.
#[tokio::test(flavor = "multi_thread")]
async fn ledger_site_rejection_is_discarded_durably_and_not_blacklisted() {
    let mut h = Harness::new(10).await;
    let parent = h.bootstrap(Era::Conway, 10).await;
    let (slot, honest_cbor) = h
        .honest_leader_block(Era::Conway, 10, parent.0 + 1, parent.0 + 100, 2, parent.1)
        .await;
    let bad_cbor = retag(&honest_cbor, 6);
    h.node.skip_era_checks_for_test = true;
    let failures = h.header_failures();
    let bad = h.deliver(&bad_cbor).await;
    h.node.skip_era_checks_for_test = false;

    // It really went through the store and the ledger (not the node checks).
    assert_rejected(&mut h, &bad, Era::Conway, parent, failures).await;

    // Durable: a fresh open of the volatile directory (WAL replay) must not
    // contain the re-tagged bytes.
    let volatile = h._dir.path().join("db").join("volatile");
    let reopened = dugite_storage::VolatileDB::open(&volatile).expect("reopen volatile");
    assert!(
        !reopened.has_block(bad.hash()),
        "the re-tagged bytes must not come back from the WAL"
    );
    assert!(
        reopened.has_block(&parent.1),
        "the control: surviving blocks are still in the WAL"
    );
    drop(reopened);

    let honest = h.deliver(&honest_cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *honest.hash())
    );
}

/// The same Conway tag on the SAME ledger but inside epoch 0 is an early
/// upgrade, not a boundary: rejected.
#[tokio::test(flavor = "multi_thread")]
async fn conway_tag_mid_epoch_on_a_babbage_ledger_is_rejected() {
    let mut h = Harness::new(8).await;
    let parent = h.bootstrap(Era::Babbage, 8).await;

    // Elected slot, found with the honest Babbage block; the Conway block at
    // the same slot has the same VRF input, so the pool is elected there too.
    let (slot, _) = h
        .honest_leader_block(Era::Babbage, 8, parent.0 + 1, EPOCH_LENGTH - 1, 2, parent.1)
        .await;
    let cbor = h.forge_at(Era::Conway, 9, slot, 2, parent.1).await;
    let bad_block = h.decode(&cbor).unwrap();
    assert_eq!(bad_block.era, Era::Conway);
    let err = h
        .node
        .validate_peer_header_full(&bad_block)
        .await
        .expect_err("a mid-epoch era upgrade must fail header validation");
    assert!(err.to_string().contains("era tag check"), "{err}");

    let failures = h.header_failures();
    let bad = h.deliver(&cbor).await;
    assert_rejected(&mut h, &bad, Era::Babbage, parent, failures).await;
    assert_honest_block_applies(&mut h, Era::Babbage, 8, parent, slot).await;
}

/// The re-tagged copy is stored first because it does not extend the ledger tip
/// yet (the pre-storage check cannot decide), and is rejected later, when a
/// fork through it is replayed. The stored copy must then be removed and its
/// hash left usable, so the honest copy still applies.
#[tokio::test(flavor = "multi_thread")]
async fn retagged_copy_stored_out_of_order_is_removed_when_its_fork_is_replayed() {
    let mut h = Harness::new(10).await;
    let a = h.bootstrap(Era::Conway, 10).await;

    // Chain A -> X (applied): the ledger tip is X.
    let (x_slot, x_cbor) = h
        .honest_leader_block(Era::Conway, 10, a.0 + 1, a.0 + 100, 2, a.1)
        .await;
    let x = h.deliver(&x_cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(x_slot), *x.hash())
    );

    // A competing branch A -> B -> E, with B delivered RE-TAGGED.
    let (b_slot, b_cbor) = h
        .honest_leader_block(Era::Conway, 10, x_slot + 1, x_slot + 100, 2, a.1)
        .await;
    let b = h.decode(&b_cbor).unwrap();
    let (_, e_cbor) = h
        .honest_leader_block(Era::Conway, 10, b_slot + 1, b_slot + 100, 3, *b.hash())
        .await;
    let failures = h.header_failures();
    let b_bad = h.deliver(&retag(&b_cbor, 6)).await;
    assert_eq!(b_bad.hash(), b.hash());
    // Whether the fork A -> B' is replayed on arrival of B' (it wins the equal
    // height tiebreak) or on arrival of E, the replay is where it is rejected.
    h.deliver(&e_cbor).await;

    assert_eq!(h.ledger_era().await, Era::Conway);
    assert!(
        !h.is_marked_invalid(b.hash()).await,
        "the hash must NOT be blacklisted"
    );
    assert!(
        !h.has_block(b.hash()).await,
        "the re-tagged copy must be removed from the ChainDB"
    );
    assert_eq!(h.header_failures(), failures + 1);
    assert_eq!(h.chain_db_tip_point().await, h.ledger_tip_point().await);

    // The honest copy of B (same hash) is accepted, and the chain moves on
    // through it.
    h.deliver(&b_cbor).await;
    assert!(h.has_block(b.hash()).await);
    assert!(!h.is_marked_invalid(b.hash()).await);
    assert_eq!(h.chain_db_tip_point().await, h.ledger_tip_point().await);
}

/// An empty epoch puts the next block two epochs ahead of the ledger tip (here:
/// within the forecast horizon, epoch 2 of a 100-slot epoch). A re-tag DOWNGRADE
/// across the gap (Conway to Babbage) is rejected and the honest copy applies.
#[tokio::test(flavor = "multi_thread")]
async fn retag_downgrade_two_epochs_ahead_is_rejected() {
    let mut h = Harness::new(10).await;
    let parent = h.bootstrap(Era::Conway, 10).await;
    let (slot, honest_cbor) = h
        .honest_leader_block(Era::Conway, 10, 2 * EPOCH_LENGTH, 240, 2, parent.1)
        .await;
    assert!(slot >= 2 * EPOCH_LENGTH, "two epochs past the ledger tip");
    let failures = h.header_failures();
    let bad = h.deliver(&retag(&honest_cbor, 6)).await;
    assert_rejected(&mut h, &bad, Era::Conway, parent, failures).await;
    let honest = h.deliver(&honest_cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(SlotNo(slot), *honest.hash())
    );
    assert_eq!(h.ledger_era().await, Era::Conway);
}

fn addr(port: u16) -> std::net::SocketAddr {
    format!("127.0.0.1:{port}").parse().unwrap()
}

/// The recorded server is the one that served THOSE bytes: the same hash with
/// other bytes is another entry, and an existing entry is never overwritten.
#[test]
fn stored_block_peers_key_on_the_bytes_and_never_overwrite() {
    let mut m = StoredBlockPeers::new(8);
    let h = Hash32::from_bytes([7; 32]);
    m.record(&h, b"honest bytes", addr(1));
    m.record(&h, b"re-tagged bytes", addr(2));
    // A later delivery of the same bytes does not take the entry over.
    m.record(&h, b"honest bytes", addr(3));
    assert_eq!(m.take(&h, b"re-tagged bytes"), Some(addr(2)));
    assert_eq!(m.take(&h, b"honest bytes"), Some(addr(1)));
    assert_eq!(m.take(&h, b"honest bytes"), None, "taken once");
    assert_eq!(m.take(&h, b"never stored"), None);
}

/// Eviction is least-recently-used: a flood evicts the oldest entries, never
/// everything, and an entry that is looked at again survives.
#[test]
fn stored_block_peers_evict_least_recently_used_not_everything() {
    let mut m = StoredBlockPeers::new(4);
    let h = |i: u8| Hash32::from_bytes([i; 32]);
    for i in 0..4u8 {
        m.record(&h(i), b"b", addr(1000 + i as u16));
    }
    // Re-recording block 0 marks it recently used (no overwrite).
    m.record(&h(0), b"b", addr(9));
    // A flood of 3 new entries evicts the three least recently used (1, 2, 3).
    for i in 10..13u8 {
        m.record(&h(i), b"b", addr(2000 + i as u16));
    }
    assert_eq!(m.len(), 4, "bounded, not cleared");
    assert_eq!(
        m.take(&h(0), b"b"),
        Some(addr(1000)),
        "touched entry survives"
    );
    assert_eq!(m.take(&h(1), b"b"), None);
    assert_eq!(m.take(&h(12), b"b"), Some(addr(2012)), "newest survive");
}

/// Node path: the honest copy is served by P1 and applies; the re-tagged copy of
/// the same hash comes from P3. P3 is the peer that is faulted, and the map holds
/// only the honest bytes' server (the re-tagged copy was never stored).
#[tokio::test(flavor = "multi_thread")]
async fn the_peer_that_served_the_rejected_bytes_is_the_one_faulted() {
    let mut h = Harness::new(10).await;
    let a = h.bootstrap(Era::Conway, 10).await;
    let (_, honest_cbor) = h
        .honest_leader_block(Era::Conway, 10, a.0 + 1, a.0 + 100, 2, a.1)
        .await;
    let honest = h.decode(&honest_cbor).unwrap();
    h.deliver_from(addr(3001), &honest_cbor).await;
    assert_eq!(
        h.ledger_tip_point().await,
        Point::Specific(honest.slot(), *honest.hash())
    );

    // The same hash, re-tagged, from another peer.
    h.deliver_from(addr(3003), &retag(&honest_cbor, 6)).await;
    let (peer, kind) = h.peer_rx.try_recv().expect("a peer is reported");
    assert_eq!(peer, addr(3003), "the server of the rejected bytes");
    assert_eq!(kind, PeerFailureKind::ProtocolFault);
    assert!(h.peer_rx.try_recv().is_err());

    let mut peers = h.node.stored_block_peers.lock();
    assert_eq!(peers.len(), 2, "bootstrap block + the honest copy only");
    assert_eq!(
        peers.take(honest.hash(), &honest_cbor),
        Some(addr(3001)),
        "the honest copy keeps its server"
    );
    assert_eq!(
        peers.take(honest.hash(), &retag(&honest_cbor, 6)),
        None,
        "the re-tagged bytes were never stored"
    );
}
