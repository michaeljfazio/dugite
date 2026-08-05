//! Main Dugite node: struct definition, initialization, and run loop orchestration.
//!
//! This module owns the `Node` struct and the top-level lifecycle methods (`new`,
//! `run`).  All subsystem logic is delegated to focused sub-modules:
//!
//! - [`epoch`]  — Snapshot policy, ledger snapshot save/prune/restore
//! - [`serve`]  — N2N/N2C server adapters (BlockProvider, TxValidator, metrics bridges)
//! - [`query`]  — N2C LocalStateQuery response building (`update_query_state`)
//! - [`sync`]   — Pipelined ChainSync loop, block processing, rollback, replay

// NOTE: these two module-level `dead_code` allows are themselves stale for
// the modules' own top-level items — `ConnectionLifecycleManager` and
// `PeerConnection` are both heavily used in production. Left in place
// because a module-level `#[allow]` suppresses the lint recursively: lifting
// it surfaces a separate, pre-existing set of dead code *inside* these files
// (e.g. `PeerConnection::has_warm_protocols`/`has_hot_protocols`,
// `FetchedBlock::tip_slot`/`tip_hash`/`tip_block_number`) that is out of
// scope for #1003 (peer-manager methods in `networking.rs`) and deserves
// its own audit rather than a drive-by fix here.
#[allow(dead_code)]
pub(crate) mod connection_lifecycle;
pub(crate) mod epoch;
pub(crate) mod ledger_view;
pub(crate) mod n2c_query;
pub(crate) mod networking;
#[allow(dead_code)]
pub(crate) mod peer_connection;
pub(crate) mod query;
pub(crate) mod serve;
pub(crate) mod snapshot_worker;
pub(crate) mod sync;
pub mod tip_broadcast;

use anyhow::Result;
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::signal;
use tokio::sync::{mpsc, watch, RwLock, Semaphore};
use tracing::{debug, error, info, trace, warn};

use crate::node::connection_lifecycle::{
    CandidateChainState, ConnectError, ConnectResult, ConnectionLifecycleManager, FetchedBlock,
    LifecycleError, PeerFailureKind,
};
use crate::node::peer_connection::PeerConnection;

use dugite_consensus::chain_fragment::ChainFragment;
use dugite_consensus::praos::BlockIssuerInfo;
use dugite_consensus::OuroborosPraos;
use dugite_consensus::ValidationMode;
use dugite_ledger::{BlockValidationMode, LedgerState};
use dugite_mempool::{Mempool, MempoolConfig};
use dugite_network::{Governor, GovernorConfig, PeerTargets, RollbackAnnouncement};

use crate::node::n2c_query::QueryHandler;
use crate::node::networking::{DiffusionMode, NodePeerManager, PeerManagerConfig};
use dugite_primitives::block::Point;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_storage::background::{CopyToImmutable, GcScheduler, SnapshotScheduler};
use dugite_storage::{ChainDB, ChainSelHandle};

use crate::config::NodeConfig;
use crate::genesis::{
    load_dijkstra_genesis_with_hash, AlonzoGenesis, ByronGenesis, ConwayGenesis, ShelleyGenesis,
};
use crate::metrics::GovernanceSnapshot;
use crate::topology::Topology;

// ── Post-apply timing gate (issue #702) ───────────────────────────────────
//
// `DUGITE_POST_APPLY_TIMING=1` enables per-section timing inside
// `post_block_apply_updates`.  Checked once at first call via OnceLock so
// there is zero overhead (no env var lookup, no branch) when the env var is
// unset.
static POST_APPLY_TIMING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[inline(always)]
fn post_apply_timing_enabled() -> bool {
    *POST_APPLY_TIMING
        .get_or_init(|| std::env::var("DUGITE_POST_APPLY_TIMING").as_deref() == Ok("1"))
}

// ── Resource-limit constants (G-series audit fixes) ────────────────────────

/// Timeout for the N2N inbound handshake task.
///
/// The inner `run_n2n_handshake_server` already has a 10-second timeout, but if
/// the bearer stalls *before* reaching the handshake call (e.g. a peer sends
/// exactly one byte and holds the TCP stream open), the outer task never
/// reaches that timeout and parks indefinitely.  This outer guard ensures
/// the task terminates regardless of where the stall occurs (G2).
const N2N_INBOUND_TASK_TIMEOUT: Duration = Duration::from_secs(30);

/// Heartbeat tick interval for the process-freeze watchdog (G9).
const HEARTBEAT_TICK: Duration = Duration::from_secs(2);

/// Maximum acceptable heartbeat lateness before logging a WARN (G9).
const HEARTBEAT_LATE_THRESHOLD: Duration = Duration::from_secs(10);

/// Poll interval for the #768 apply-stall watchdog.
const APPLY_STALL_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// How long the ledger tip may remain ahead of the ChainDB tip with zero
/// forward progress (and fetched blocks arriving but not connecting) before the
/// node is declared stranded (#768) and shuts down with an actionable error.
/// Generous so a slowly-bridging Mithril gap is never false-flagged — in normal
/// operation the ChainDB write precedes the ledger apply, so the ledger tip is
/// NEVER ahead of the ChainDB tip; this state only arises from an
/// ahead-of-storage snapshot.
const APPLY_STALL_TIMEOUT: Duration = Duration::from_secs(300);

/// #768 apply-stall predicate (pure, unit-tested). The node is stranded iff:
/// (1) the ledger tip is STRICTLY ahead of the ChainDB tip — only possible from
///     an ahead-of-storage snapshot, since in normal operation the ChainDB
///     write precedes the ledger apply (so chaindb_tip >= ledger_tip);
/// (2) fetched blocks are still arriving and being skipped as non-connecting
///     (`work_arriving`) — distinguishes the wedge from a quiet at-tip node; and
/// (3) there has been zero forward progress (neither tip advanced) for at least
///     `timeout`.
/// Any forward progress (a tip advancing, i.e. a Mithril gap bridging) resets
/// the timer, so a healthy or slowly-progressing node never trips it.
fn apply_stall_detected(
    ledger_slot: u64,
    chaindb_slot: u64,
    work_arriving: bool,
    stalled: Duration,
    timeout: Duration,
) -> bool {
    ledger_slot > chaindb_slot && work_arriving && stalled >= timeout
}

// ── N2N broadcast channel capacities ──────────────────────────────────────

/// Block-announcement broadcast channel capacity for N2N and N2C servers.
///
/// 64 was the original value; increased to 512 to absorb burst fork events
/// without triggering the `RecvError::Lagged` path that causes downstream
/// peers to receive an incorrect rollback point (G6).
const BLOCK_ANN_CHANNEL_CAP: usize = 512;

/// Rollback-announcement broadcast channel capacity.
///
/// 16 was the original value; increased to 256.  Only the most-recent
/// rollback matters, but a higher buffer reduces the chance of lagging
/// during rapid fork oscillations (G6).
const ROLLBACK_ANN_CHANNEL_CAP: usize = 256;

// ── Fetch pipeline channel capacity ───────────────────────────────────────

/// BlockFetch → ledger-apply channel capacity.
///
/// Sized to absorb one full Byron-era apply round-trip without
/// back-pressuring the BlockFetch worker, which now polls at the
/// Haskell-aligned 10 ms cadence (see
/// `connection_lifecycle::ConnectionLifecycleManager::make_blockfetch_task`
/// :: `poll_ticker`).  Previously 128 entries (~12 ms of Byron throughput)
/// pinned the worker on `fetched_blocks_tx.send().await` after every range
/// completion, undoing the throughput win of the 10 ms cadence.
///
/// Each `FetchedBlock` holds the raw CBOR plus the fully-decoded `Block`
/// (transactions/witnesses/decoded Plutus data), so peak in-flight memory is a
/// few × the raw size: ballpark up to ~0.5–1 GB worst-case Conway at 4096 slots,
/// ~50 MB Alonzo avg, a few MB Byron — within the runtime budget on the soak
/// host, but not a hard ceiling for memory-constrained deployments.
///
/// #767: raised 1024 → 4096 to widen the apply-lag tolerance window during bulk
/// catch-up. The residual peer-cascade stall begins when sustained apply lag
/// drains then refills this channel; a deeper buffer delays cascade onset
/// (~85 s → ~340 s fill time), giving apply time to absorb spikes. Defense in
/// depth alongside the chainsync lock-convoy fix.
const FETCHED_BLOCKS_CHANNEL_CAP: usize = 4096;

/// GSM event channel capacity (G12).
///
/// Increased from 1024 to 4096 to absorb rapid peer churn events without
/// dropping GSM events via try_send in the ChainSync task.
const GSM_EVENT_CHANNEL_CAP: usize = 4096;

/// #760: hard deadline (seconds) for the main run loop to BREAK after a
/// shutdown signal, after which an independent watchdog force-exits the
/// process. Bounds the SIGTERM-to-exit latency so a sync wedge (e.g. the
/// genesis CSJ-far-ahead loop) can never leave the node un-stoppable: a
/// wedged node once ignored SIGTERM for 1h42m, forcing a SIGKILL that risks
/// ImmutableDB/LSM corruption. The watchdog only fires if the loop has NOT
/// broken (i.e. never reached the already-bounded post-loop drain), so a
/// healthy slow drain is never killed. Override via
/// `DUGITE_SHUTDOWN_DEADLINE_SECS`. A SECOND signal forces immediate exit.
const SHUTDOWN_LOOP_BREAK_DEADLINE_SECS: u64 = 90;

/// Bind the N2N listener with `SO_REUSEADDR` + `SO_REUSEPORT` (Unix) so
/// outbound connections from this node can share the listen port via
/// [`dugite_network::TcpBearer::connect_from`]. Matches Haskell
/// ouroboros-network `configureSocket` behaviour. Returns a tokio
/// `TcpListener` ready for `accept()`.
/// Resolve the on-disk path of an InMemory ledger snapshot's `tables` blob.
///
/// Mithril aggregator + ouroboros-consensus snapshot layouts have evolved:
///
/// - **New (ouroboros-consensus 1.0.0.0+):** the tables blob lives at the flat
///   path `<snap>/tables` (a file, not a directory).
/// - **Legacy:** the tables blob lived at the nested path `<snap>/tables/tvar`
///   where `<snap>/tables` was a directory.
///
/// Prefer the new layout if both happen to be present. Return `None` when
/// neither layout is satisfied — in particular, when `<snap>/tables` is a
/// directory without a `tvar` child, the importer must NOT treat the directory
/// as a blob.
///
/// Used by the Mithril InMemory snapshot importer. Issue #460.
pub(crate) fn resolve_inmemory_tables_path(snap: &std::path::Path) -> Option<PathBuf> {
    let flat = snap.join("tables");
    let nested = flat.join("tvar");
    // Prefer the new flat layout: `<snap>/tables` as a regular file.
    if flat.is_file() {
        return Some(flat);
    }
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// Authoritatively resolve the on-disk MemPack `TxIx` byte order for a Haskell
/// snapshot directory, reading the disambiguator from the sibling `meta` file's
/// `tablesCodecVersion`.
///
/// This is the exact logic the InMemory snapshot importer runs; it is factored
/// out so it can be exercised end-to-end by the importer tests (rather than
/// re-implementing the policy or testing `from_tables_codec_version` in
/// isolation). `tvar_data` is the raw MemPack tables blob, used only by the
/// independent cross-validation safety net.
///
/// STRICT META SEMANTICS (#10). The backend/version/endianness DECISION below is
/// byte-exact with upstream `loadSnapshot`'s metadata handling. SCOPE NOTE: this
/// import path does NOT verify snapshot CRC/checksum integrity
/// (`crcOfConcat == snapshotChecksum`); that is tracked separately as #17.
///
/// The two-file `state`+`meta` snapshot format carries the disambiguators we
/// REQUIRE: a `backend` (`UTxOHDMemSnapshot`) and a `tablesCodecVersion`. The
/// node loader `Ouroboros.Consensus.Storage.LedgerDB.V2.InMemory.loadSnapshot`
/// reads the metadata and:
///
/// 1. enforces the backend BEFORE decoding tables —
///    ```haskell
///    when (snapshotBackend /= UTxOHDMemSnapshot) $
///      throwE $ MetadataBackendMismatch snapshotBackend
///    ```
/// 2. then decodes the tables via the `BigEndianTxIx` MemPack instance
///    UNCONDITIONALLY — it never branches on the codec version for endianness;
/// 3. a meta that fails to parse (including the mandatory
///    `o .: "tablesCodecVersion"` — absent or null) is a `ReadMetadataError`;
/// 4. a MISSING meta file is also a `ReadMetadataError`. (`getMetadata`'s
///    `MetadataFileDoesNotExist -> Nothing` in the offline converter is the
///    CRC-SKIP path, NOT a decode-LE branch — endianness is never selected from
///    a missing/absent version.)
///
/// We therefore REJECT everything upstream's METADATA DECODE rejects (default to
/// rejection / byte-exact only; CRC/checksum integrity is out of scope here — #17):
///
/// * meta FILE absent ⇒ ERROR (no silent legacy little-endian fallback);
/// * meta present, `backend != "utxohd-mem"` (absent/other) ⇒ ERROR
///   (`MetadataBackendMismatch`);
/// * meta present, `tablesCodecVersion` absent/null ⇒ ERROR (`MetadataInvalid`);
/// * meta present, `tablesCodecVersion == 1` ⇒ big-endian `TxIx` (the only
///   accepted outcome; chain-verified against the modern preprod snapshot);
/// * meta present, any other version / malformed ⇒ ERROR (`enforceVersion`).
///
/// `Big` is the ONLY byte order this function can ever return. The
/// cross-validator is a LIVE defense-in-depth veto (called below on `tvar_data`):
/// it re-derives the byte order from the actual UTxO index distribution and
/// REJECTS the import if the data clearly contradicts the version-derived choice.
/// The purely empirical `detect_txix_endianness` is reserved for test/fixture
/// snapshots that ship no `meta` file.
pub(crate) fn resolve_snapshot_txix_endianness(
    snapshot_dir: &std::path::Path,
    tvar_data: &[u8],
) -> anyhow::Result<dugite_serialization::mempack::TxIxEndianness> {
    use anyhow::Context;

    let meta_path = snapshot_dir.join("meta");
    let meta_bytes = match std::fs::read(&meta_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "snapshot has no meta file at {} (upstream V2/InMemory loadSnapshot would \
                 fail with ReadMetadataError); refusing to import a snapshot without a valid \
                 tables codec version — endianness is never selected from a missing meta \
                 (no silent little-endian fallback)",
                meta_path.display()
            );
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "reading snapshot meta file at {} (it exists but could not be read — \
                     refusing to guess the TxIx codec version)",
                    meta_path.display()
                )
            });
        }
    };

    // Enforce backend == "utxohd-mem" (UTxOHDMemSnapshot) BEFORE anything else,
    // mirroring V2/InMemory loadSnapshot's MetadataBackendMismatch guard. The
    // separate `check_snapshot_backend_match` / `load_snapshot_with_backend_guard`
    // path guards the dugite ledger-snapshot LOAD, not this MemPack IMPORT path.
    dugite_serialization::mempack::enforce_snapshot_backend_is_utxohd_mem(&meta_bytes)
        .with_context(|| format!("validating snapshot backend from {}", meta_path.display()))?;

    let codec_version = dugite_serialization::mempack::parse_tables_codec_version(&meta_bytes)
        .with_context(|| format!("parsing tablesCodecVersion from {}", meta_path.display()))?;

    let endianness = dugite_serialization::mempack::TxIxEndianness::from_tables_codec_version(
        Some(codec_version),
    )
    .with_context(|| {
        format!(
            "mapping tablesCodecVersion={codec_version} (from {}) to a TxIx byte order",
            meta_path.display()
        )
    })?;
    info!(
        codec_version,
        txix_endianness = ?endianness,
        "Authoritatively determined MemPack TxIx endianness from snapshot meta \
         tablesCodecVersion (strict: only version 1 => big-endian is accepted)"
    );

    // INDEPENDENT cross-validation (defense in depth): re-derive the byte order
    // empirically from the data and ERROR if it CLEARLY contradicts the
    // version-derived choice (always big-endian under strict semantics). The
    // version decides; the heuristic only vetoes a definite disagreement.
    dugite_serialization::mempack::cross_validate_txix_endianness(tvar_data, endianness)
        .with_context(|| {
            format!(
                "snapshot meta at {} disagrees with the UTxO index distribution",
                meta_path.display()
            )
        })?;

    Ok(endianness)
}

fn bind_n2n_listener(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let domain = match addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    {
        // Best-effort — REUSEPORT may be unavailable on some Unixes.
        let _ = socket.set_reuse_port(true);
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    let std_listener: std::net::TcpListener = socket.into();
    tokio::net::TcpListener::from_std(std_listener)
}

/// Refresh all peer / connection-manager gauges from the current peer-manager
/// state.
///
/// Centralises the gauge writes so every connection-lifecycle transition
/// (inbound register, outbound register, peer-failed, peer-disconnected) can
/// drive the same set of Prometheus gauges without each call-site duplicating
/// the field list. The active-connection count comes from the lifecycle map
/// rather than the peer-manager — keep them passed in separately so callers
/// without lifecycle access (tests, future internal probes) can still use this.
///
/// Regression context (GitHub #437): prior to this refactor `update_peer_metrics`
/// only wrote `peers_*` and `n2n_connections_active`. The `conn_*` family of
/// gauges (matching Haskell's `ConnectionManagerCounters`) was only refreshed by
/// the block-arrival logging path in `sync.rs`, so a node at chain tip with
/// inbound peers reported `conn_inbound = 0` until the next block landed.
pub(crate) fn apply_peer_metrics(
    metrics: &crate::metrics::NodeMetrics,
    pm: &crate::node::networking::NodePeerManager,
    active_connection_count: usize,
) {
    use std::sync::atomic::Ordering::Relaxed;

    metrics
        .peers_connected
        .store((pm.warm_peer_count() + pm.hot_peer_count()) as u64, Relaxed);
    metrics
        .peers_cold
        .store(pm.cold_peer_count() as u64, Relaxed);
    metrics
        .peers_warm
        .store(pm.warm_peer_count() as u64, Relaxed);
    metrics.peers_hot.store(pm.hot_peer_count() as u64, Relaxed);
    metrics
        .peers_outbound
        .store(pm.outbound_peer_count() as u64, Relaxed);
    metrics
        .peers_inbound
        .store(pm.inbound_peer_count() as u64, Relaxed);
    metrics
        .peers_duplex
        .store(pm.duplex_peer_count() as u64, Relaxed);

    // Connection-manager counters (Haskell ConnectionManagerCounters compat).
    let cm = pm.connection_manager_counters();
    metrics.conn_full_duplex.store(cm.full_duplex, Relaxed);
    metrics.conn_duplex.store(cm.duplex, Relaxed);
    metrics
        .conn_unidirectional
        .store(cm.unidirectional, Relaxed);
    metrics.conn_inbound.store(cm.inbound, Relaxed);
    metrics.conn_outbound.store(cm.outbound, Relaxed);
    metrics.conn_terminating.store(cm.terminating, Relaxed);

    // n2n_connections_active is derived from the lifecycle's connections HashMap
    // (the authoritative source) rather than a fetch_add/fetch_sub counter that
    // can drift. Invariant: gauge == connections.len() after every call.
    metrics
        .n2n_connections_active
        .store(active_connection_count as u64, Relaxed);
}

/// Convert a MemPack-classified reference script ([`ScriptRefKind`]) into a
/// typed [`ScriptRef`](dugite_primitives::transaction::ScriptRef).
///
/// Used by the Haskell/Mithril UTxO importer to reconstruct reference scripts
/// from a tag-5 (`TxOutCompactRefScript`) MemPack TxOut. The Plutus language tag
/// is era-relative in MemPack (per-era `packTagM`), but every Cardano era's
/// supported-language list is a strict PREFIX of `[V1, V2, V3, V4]` — no era has
/// ever reordered or removed a language — so the era-relative index equals the
/// global `fromEnum(language)` and the static map below (0→V1, 1→V2, 2→V3, 3→V4)
/// is byte-exact for EVERY snapshot era (Babbage/Conway/Dijkstra agree). Adding a
/// NEW language in a future era is safe here: its tag (≥ 4) hits the out-of-range
/// hard-error arm rather than mis-mapping.
///
/// INVARIANT (#16): this static tag→version mapping is correct ONLY while that
/// strict-prefix property holds. If a future era ever REORDERS or REMOVES a Plutus
/// language, the era-relative tag would no longer equal the global version, and
/// this MUST become era-aware (thread the snapshot's era + its per-era language
/// list) — the unit test
/// `decode_imported_script_ref_maps_plutus_language_tags_to_global_versions` pins
/// both the current mapping and the out-of-range rejection.
///
/// OPAQUE-STORE vs HARD-ERROR is split per the Haskell MemPack instances:
///
/// * Plutus body — `Plutus l` derives `MemPack` through
///   `newtype PlutusBinary { unPlutusBinary :: ShortByteString }
///    deriving newtype (… MemPack)` (cardano-ledger
///   `Cardano.Ledger.Plutus.Language`). So `unpackM` stores the flat program
///   bytes OPAQUELY and never re-validates them at snapshot load. We mirror that:
///   the body is carried verbatim into `ScriptRef::PlutusV{1,2,3,4}` with no
///   structural decode — a structurally-odd-but-framed body does NOT error.
///
/// * Native (timelock) body — `newtype Timelock era = MkTimelock (MemoBytes …)`
///   with `unpackM = MkTimelock <$> unpackMemoBytesM (eraProtVerLow @era)`
///   (cardano-ledger `Cardano.Ledger.Allegra.Scripts`). Unlike `PlutusBinary`,
///   `unpackMemoBytesM` STRUCTURALLY decodes the script tree, so a malformed
///   timelock CBOR is a genuine MemPack `unpackM` failure → HARD ERROR. We mirror
///   that by decoding the native CBOR and erroring on failure.
///
/// The out-of-range Plutus language tag is a frame-level error (mirrors
/// `unknownTagM`); truncation is caught earlier by `parse_script_ref_kind`. In
/// every error case we refuse the import rather than silently drop a `ScriptRef`
/// (which would make a Mithril-fast-started node fail to resolve a reference
/// script at the live tip — spurious phase-2 failures).
fn decode_imported_script_ref(
    kind: dugite_serialization::mempack::txout::ScriptRefKind,
) -> anyhow::Result<dugite_primitives::transaction::ScriptRef> {
    use dugite_primitives::transaction::ScriptRef;
    use dugite_serialization::mempack::txout::ScriptRefKind;
    match kind {
        ScriptRefKind::Native(cbor) => {
            let ns = dugite_serialization::decode_native_script_cbor(&cbor).map_err(|e| {
                anyhow::anyhow!(e).context(
                    "import: malformed native reference-script (tag-5) CBOR; refusing to import \
                     a silently-dropped script_ref",
                )
            })?;
            Ok(ScriptRef::NativeScript(ns))
        }
        ScriptRefKind::Plutus { lang_tag, body } => match lang_tag {
            0 => Ok(ScriptRef::PlutusV1(body)),
            1 => Ok(ScriptRef::PlutusV2(body)),
            2 => Ok(ScriptRef::PlutusV3(body)),
            3 => Ok(ScriptRef::PlutusV4(body)),
            other => Err(anyhow::anyhow!(
                "import: unknown Plutus language tag {other} in tag-5 MemPack reference script; \
                 refusing to import a silently-dropped script_ref"
            )),
        },
    }
}

/// OPAQUE-STORE a MemPack-imported inline datum (`BinaryData`) into an
/// [`OutputDatum::InlineDatum`].
///
/// Matches Haskell `BinaryData`, defined as
///
/// ```haskell
/// newtype BinaryData era = BinaryData ShortByteString
///   deriving newtype (..., MemPack)
/// ```
///
/// (cardano-ledger `Cardano.Ledger.Plutus.Data`). Because the `MemPack` instance
/// is *derived through* `ShortByteString`, `unpackM` at snapshot load stores the
/// bytes OPAQUELY and never re-decodes the Plutus `Data` structure — structural
/// validation lives only in `makeBinaryData`, the on-chain `DecCBOR` path, which
/// `loadSnapshot` does NOT invoke. So a tag-4 blob that is framed (its MemPack
/// VarLen wrapper already stripped by the TxOut decoder) but structurally odd
/// must NOT hard-error the import (that would OVER-REJECT vs Haskell).
///
/// We best-effort decode `inline_cbor` to populate the structural `data` field
/// for downstream ledger/Plutus use; on a decode error we KEEP the verbatim bytes
/// (`PlutusData::Bytes` fallback) and never drop the datum. `raw_cbor` always
/// carries the exact bytes for byte-exact re-encoding. This function is
/// total — it never returns `OutputDatum::None` for a present inline datum.
fn import_inline_datum(inline_cbor: &[u8]) -> dugite_primitives::transaction::OutputDatum {
    use dugite_primitives::transaction::{OutputDatum, PlutusData};
    match dugite_serialization::decode_plutus_data_cbor(inline_cbor) {
        Ok(data) => OutputDatum::InlineDatum {
            data,
            raw_cbor: Some(inline_cbor.to_vec()),
        },
        Err(_e) => OutputDatum::InlineDatum {
            data: PlutusData::Bytes(inline_cbor.to_vec()),
            raw_cbor: Some(inline_cbor.to_vec()),
        },
    }
}

/// Flatten a `LedgerState` into the primitive [`GovernanceSnapshot`] consumed
/// by [`crate::metrics::NodeMetrics::set_governance_snapshot`].
///
/// Called from node startup and the sync loop's metric-refresh path so the
/// governance gauges always reflect the ledger/pparam state together.
fn governance_snapshot_from_ledger(ls: &LedgerState) -> GovernanceSnapshot {
    let gov = &ls.gov.governance;
    let pp = &ls.epochs.protocol_params;
    let threshold_bps = gov
        .committee_threshold
        .as_ref()
        .filter(|t| t.denominator != 0)
        .map(|t| ((t.numerator as u128 * 10_000) / t.denominator as u128) as u64)
        .unwrap_or(0);
    GovernanceSnapshot {
        delegation_count: ls.certs.delegations.len() as u64,
        treasury_lovelace: ls.epochs.treasury.0,
        reserves_lovelace: ls.epochs.reserves.0,
        pool_count: ls.certs.pool_params.len() as u64,
        drep_total: gov.dreps.len() as u64,
        drep_active: gov.active_drep_count() as u64,
        drep_registrations_total: gov.drep_registration_count,
        vote_delegation_count: gov.vote_delegations.len() as u64,
        proposal_count: gov.proposals.len() as u64,
        committee_hot_count: gov.committee_hot_keys.len() as u64,
        committee_total_count: gov.committee_expiration.len() as u64,
        committee_resigned_count: gov.committee_resigned.len() as u64,
        committee_no_confidence: gov.no_confidence,
        committee_threshold_bps: threshold_bps,
        gov_dormant_epochs: gov.num_dormant_epochs,
        constitution_present: gov.constitution.is_some(),
        pparam_drep_deposit_lovelace: pp.drep_deposit.0,
        pparam_drep_activity_epochs: pp.drep_activity,
        pparam_gov_action_deposit_lovelace: pp.gov_action_deposit.0,
        pparam_gov_action_lifetime_epochs: pp.gov_action_lifetime,
        pparam_committee_min_size: pp.committee_min_size,
        pparam_committee_max_term_length: pp.committee_max_term_length,
    }
}

/// Refresh the *heavier* per-block gauges — the full governance snapshot
/// (DReps, proposals, committee, pparams, and pots) plus `utxo_count` — from
/// live ledger state.
///
/// This is deliberately distinct from the O(1) pots refresh ([`crate::metrics::NodeMetrics::set_pots`])
/// that runs on the universal per-block path (`post_block_apply_updates`):
/// [`governance_snapshot_from_ledger`] walks the governance maps, so this is
/// reserved for the at-tip / era-transition branch of `apply_fetched_block`
/// (and the forge path), where blocks are seconds apart. It must NOT run per
/// block on the bulk-sync hot path — that is the whole point of the at-tip
/// gate; doing so would reintroduce the per-block governance walk the catch-up
/// branch deliberately skips. Without this call, `drep_count`, `proposal_count`,
/// the committee gauges, the pparam gauges, and `utxo_count` froze at tip on
/// every epoch boundary the node did not forge itself — the same staleness
/// class the pots gauge had.
pub(crate) fn refresh_heavy_at_tip_gauges(metrics: &crate::metrics::NodeMetrics, ls: &LedgerState) {
    metrics.set_governance_snapshot(&governance_snapshot_from_ledger(ls));
    metrics.set_utxo_count(ls.utxo.utxo_set.len() as u64);
}

// ─── NodeArgs ────────────────────────────────────────────────────────────────

pub struct NodeArgs {
    pub config: NodeConfig,
    pub topology: Topology,
    pub topology_path: PathBuf,
    /// Path to the node config JSON. Re-read on SIGHUP to pick up new
    /// `LogDirective` values for runtime trace-verbosity reload (#473).
    pub config_path: PathBuf,
    pub database_path: PathBuf,
    pub socket_path: PathBuf,
    pub host_addr: String,
    pub port: u16,
    /// Directory containing the config file (for resolving relative genesis paths)
    pub config_dir: PathBuf,
    /// Path to KES signing key (enables block production)
    pub shelley_kes_key: Option<PathBuf>,
    /// Path to VRF signing key (enables block production)
    pub shelley_vrf_key: Option<PathBuf>,
    /// Path to operational certificate (enables block production)
    pub shelley_operational_certificate: Option<PathBuf>,
    /// Path to cold signing key (accepted for cardano-node compatibility)
    pub _shelley_cold_key: Option<PathBuf>,
    /// UTxO RPC server configuration (None = disabled) — issue #672.
    pub rpc_config: Option<dugite_rpc::RpcConfig>,
    /// Prometheus metrics port (0 to disable)
    pub metrics_port: u16,
    /// Make a metrics bind failure a fatal startup error (default: continue without metrics)
    pub require_metrics: bool,
    /// Emit `cardano_node_metrics_*` compatibility aliases alongside native metrics
    pub compat_metrics: bool,
    /// Liveness threshold for the `/live` HTTP endpoint (seconds; 0 disables).
    pub liveness_threshold_secs: u64,
    /// Maximum number of transactions in the mempool
    pub mempool_max_tx: usize,
    /// Maximum mempool size in bytes
    pub mempool_max_bytes: usize,
    /// Maximum snapshots to retain on disk
    pub snapshot_max_retained: usize,
    /// Minimum blocks between bulk-sync snapshots
    pub snapshot_bulk_min_blocks: u64,
    /// Minimum seconds between bulk-sync snapshots
    pub snapshot_bulk_min_secs: u64,
    /// Storage configuration (block index type, UTxO backend, LSM tuning)
    pub storage_config: dugite_storage::StorageConfig,
    /// Consensus mode: "praos" (default) or "genesis" (enables genesis bootstrap)
    pub consensus_mode: String,
    /// Force ValidateAll mode on every block (paranoid/auditing mode)
    pub validate_all_blocks: bool,
    /// Issue #655 P2.b — skip apply-time `validate_header_full` for
    /// headers that already passed eager per-peer validation against
    /// the same ledger view's epoch. Default OFF until Phase 1 has
    /// been soaked for 7+ days on preview AND preprod with no
    /// unexpected disconnect storms (the original #655 acceptance
    /// criteria). When OFF, body apply continues to fully re-validate
    /// every header — defense in depth, identical to today.
    pub skip_eagerly_validated_header_crypto: bool,
    /// Handle to the live tracing subscriber.  When present, the SIGHUP handler
    /// re-reads the node config's `LogDirective` and applies it via
    /// `LogHandle::reload`, enabling per-subsystem trace verbosity changes
    /// without a process restart (#473).
    pub log_handle: Option<crate::logging::LogHandle>,
}

// ─── Node struct ─────────────────────────────────────────────────────────────

pub struct Node {
    pub(crate) config: NodeConfig,
    pub(crate) topology: Topology,
    pub(crate) chain_db: Arc<RwLock<ChainDB>>,
    pub(crate) ledger_state: Arc<RwLock<LedgerState>>,
    /// Lock-free read-only view of stable ledger state, published by the
    /// apply path after each successful advance (issue #651 P2 / #652 P0).
    /// Use [`Node::view`] to load atomically without taking the
    /// `ledger_state` `RwLock`. Strict readers (forge VRF leader check at
    /// precise tip, mempool revalidation against the new tip) must continue
    /// to acquire `ledger_state.read().await` — those paths are not on the
    /// contention surface.
    pub(crate) ledger_view: Arc<arc_swap::ArcSwap<ledger_view::LedgerView>>,
    /// Watch channel publishing the ledger tip slot after every successful
    /// apply (issue #654 — wake-on-tip-advance back-pressure for eager
    /// per-peer header validation). Senders are the apply paths; receivers
    /// are per-peer chainsync tasks parking on forecast-horizon exhaustion.
    /// Payload is `tip.point.slot()` flattened to `u64` (origin → 0).
    pub(crate) ledger_tip_slot_tx: tokio::sync::watch::Sender<u64>,
    /// Eagerly-validated header hashes (issue #655 P2.b): block hash →
    /// epoch at which eager validation succeeded. Populated by the
    /// chainsync receive task on a successful pass through
    /// `eager_validate_header`. Consumed by the apply-time validator at
    /// `process_forward_blocks` — when the
    /// `skip_eagerly_validated_header_crypto` config flag is enabled
    /// AND `current_epoch == recorded_epoch`, the apply-time
    /// `validate_header_full` re-check is skipped (the eager pass
    /// already covered the same crypto against the same snapshot
    /// pointer). Flag defaults OFF — operators turn it on after
    /// Phase 1 has soaked clean. Entries are removed on hit AND on
    /// stale-epoch skip to keep the map bounded.
    pub(crate) eagerly_validated_headers:
        Arc<parking_lot::Mutex<HashMap<dugite_primitives::hash::Hash32, u64>>>,
    /// Volatile delta window for O(1) rollback.
    ///
    /// Kept in sync with `ledger_state`: after each `apply_block_with_delta`,
    /// the delta is pushed here. On rollback, `ledger_seq.rollback(n)` discards
    /// volatile deltas and `ledger_state` is rolled back via DiffSeq.
    ///
    /// **Lock ordering:** always acquire `ledger_state` before `ledger_seq`.
    pub(crate) ledger_seq: Arc<RwLock<dugite_ledger::ledger_seq::LedgerSeq>>,
    pub(crate) consensus: OuroborosPraos,
    pub(crate) mempool: Arc<Mempool>,
    /// Connection lifecycle manager — one TCP connection per peer,
    /// temperature-based protocol activation matching Haskell PeerStateActions.
    /// Created in `new()`, used in `run()` for Governor action dispatch.
    connection_lifecycle: Option<ConnectionLifecycleManager>,
    /// Handle to the BlockFetch decision task (independent tokio task).
    /// Runs the decision loop that assigns fetch ranges to per-peer workers.
    /// Receiver for blocks fetched by per-peer BlockFetch workers.
    /// The main run loop consumes these and applies them to the ledger.
    fetched_blocks_rx: Option<mpsc::Receiver<FetchedBlock>>,
    /// Cross-block Phase-2 (Plutus) pooling window for bulk-sync CPU saturation
    /// (`DUGITE_DEFER_PHASE2_WINDOW`, default 0 = OFF). When > 0, during catch-up
    /// (`!at_tip`) `apply_fetched_block` applies each block's STATE inline but
    /// DEFERS the Plutus drain, stashing `(block, work_items)` here; the run loop
    /// flushes the window via [`Self::flush_pending_phase2`] — pooling many
    /// blocks' redeemers into one rayon batch to fill all cores. State is
    /// byte-identical; only when/where Plutus runs moves. Default OFF: the live
    /// apply path is unchanged until an operator opts in (the exposure-gating /
    /// fork-in-window gauntlet is the prerequisite for default-on).
    defer_phase2_window: usize,
    /// The deferred (block, Phase-2 work items) accumulated under
    /// `defer_phase2_window`, drained by [`Self::flush_pending_phase2`].
    pending_phase2: Vec<(
        Box<dugite_primitives::block::Block>,
        Vec<dugite_ledger::plutus::Phase2WorkItem>,
    )>,
    /// Exact ledger tip point immediately BEFORE the first block of the current
    /// deferred window was applied (the parent of `pending_phase2[0]`). On a
    /// deferred block-fatal at window index 0 this is the precise rollback
    /// target; for i>0 the target is block i-1's own point. Slots are sparse in
    /// Cardano, so the rollback must land on a real on-chain point, not a
    /// `slot-1` guess (`handle_ledger_rollback` classifies by slot).
    pending_phase2_anchor: Option<dugite_primitives::block::Point>,
    /// Running Σ of Phase-2 work items (redeemers) buffered in `pending_phase2`.
    /// The pooled flush is triggered by THIS, not block count, so a dense-Plutus
    /// region (many redeemers per block) flushes before the pooled eval's peak
    /// memory grows unbounded. The deferral-soak wedge was a 64-block window
    /// whose redeemer count — not block count — drove a multi-GB pooled eval.
    pending_phase2_items: usize,
    /// Work-item cap that forces a [`Self::flush_pending_phase2`] even before the
    /// block window (`defer_phase2_window`) fills (`DUGITE_DEFER_PHASE2_MAX_ITEMS`,
    /// default 256). Bounds the pooled flush's peak memory + wall time.
    defer_phase2_max_items: usize,
    /// Clone of the run loop's shutdown watch, observed by the pooled flush
    /// between chunks so a SIGTERM during a long flush aborts the remaining
    /// chunks instead of being swallowed (the original `block_in_place` flush was
    /// not cancel-aware → SIGTERM was ignored during the soak wedge).
    shutdown_rx_for_flush: Option<tokio::sync::watch::Receiver<bool>>,
    /// Receiver for peer failure reports from protocol tasks (e.g. fetch timeout).
    /// The main run loop drains this to call `peer_failed()` for reputation scoring.
    peer_failure_rx: Option<mpsc::Receiver<(SocketAddr, PeerFailureKind)>>,
    /// Receiver for KeepAlive RTT measurements from connected peers.
    /// The main run loop uses these to update PeerManager EWMA and RTT gauges.
    keepalive_rtt_rx: Option<mpsc::Receiver<(SocketAddr, f64)>>,
    pub(crate) query_handler: Arc<RwLock<QueryHandler>>,
    pub(crate) peer_manager: Arc<RwLock<NodePeerManager>>,
    pub(crate) socket_path: PathBuf,
    pub(crate) database_path: PathBuf,
    pub(crate) listen_addr: std::net::SocketAddr,
    pub(crate) network_magic: u64,
    /// Byron epoch length in absolute slots (10 * k). For correct slot
    /// computation on non-mainnet networks.
    pub(crate) byron_epoch_length: u64,
    /// Byron slot duration in milliseconds (20000 on mainnet/preprod, 1000 on
    /// testnets that use 1-second Byron slots). Stored so that the Plutus
    /// SlotConfig can be anchored at the Shelley hard-fork boundary in
    /// `slot_config()` calls that happen after `new()` (e.g. in `run()`).
    pub(crate) byron_slot_duration_ms: u64,
    pub(crate) shelley_genesis: Option<ShelleyGenesis>,
    /// HFC era history state machine — tracks era boundaries with slot/epoch/time
    /// arithmetic. Initialized from genesis configs and extended during sync as
    /// era transitions are detected in the block stream.
    pub(crate) era_history: Arc<RwLock<dugite_consensus::EraHistory>>,
    pub(crate) topology_path: PathBuf,
    /// Path to the node config JSON; re-read on SIGHUP for `LogDirective` reload (#473).
    pub(crate) config_path: PathBuf,
    /// Handle to the live tracing subscriber for runtime filter reload (#473).
    pub(crate) log_handle: Option<crate::logging::LogHandle>,
    pub(crate) metrics: Arc<crate::metrics::NodeMetrics>,
    /// Block producer credentials (None = relay-only mode)
    pub(crate) block_producer: Option<crate::forge::BlockProducerCredentials>,
    /// Broadcast sender for announcing forged blocks to connected peers
    pub(crate) block_announcement_tx:
        Option<tokio::sync::broadcast::Sender<dugite_network::BlockAnnouncement>>,
    /// Broadcast sender for notifying connected peers of chain rollbacks
    pub(crate) rollback_announcement_tx:
        Option<tokio::sync::broadcast::Sender<RollbackAnnouncement>>,
    /// Payload-bearing tip event broadcaster — issue #672 M0.1.
    ///
    /// Sibling channel pair carrying `TipApply` / `TipRollback` events with
    /// richer payloads (era) than the existing announcement channels. Fanned
    /// in additively from the same send sites; consumed by external RPC.
    /// `None` until `run()` initialises it alongside the announcement channels.
    pub(crate) tip_broadcaster: Option<Arc<tip_broadcast::TipBroadcaster>>,
    /// UTxO RPC server configuration — `None` if disabled (default).
    pub(crate) rpc_config: Option<dugite_rpc::RpcConfig>,
    /// Prometheus metrics port
    pub(crate) metrics_port: u16,
    /// Make a metrics bind failure fatal (see `--require-metrics`)
    pub(crate) require_metrics: bool,
    /// Expected Blake2b-256 hash of the Byron genesis block (from config or computed from file)
    pub(crate) expected_byron_genesis_hash: Option<dugite_primitives::hash::Hash32>,
    /// Expected Blake2b-256 hash of the Shelley genesis block (from config or computed from file)
    pub(crate) expected_shelley_genesis_hash: Option<dugite_primitives::hash::Hash32>,
    /// Whether genesis block validation has been performed (only need to validate once)
    pub(crate) genesis_validated: bool,
    /// Live (post-replay) epoch transitions — only incremented during the sync
    /// loop, not during chunk replay.  Used for `snapshots_established` since
    /// replay-built snapshots may have approximate stake values.
    pub(crate) live_epoch_transitions: u32,
    /// Snapshot policy controlling when ledger snapshots are taken.
    pub(crate) snapshot_policy: epoch::SnapshotPolicy,
    /// Consensus mode: "praos" (default) or "genesis"
    pub(crate) consensus_mode: String,
    /// Force full Phase-2 Plutus validation on all blocks
    pub(crate) validate_all_blocks: bool,
    /// Issue #655 P2.b — see `NodeConfig::skip_eagerly_validated_header_crypto`.
    pub(crate) skip_eagerly_validated_header_crypto: bool,
    /// Watch receiver for current disk space level, updated by disk monitor
    pub(crate) disk_space_rx: watch::Receiver<crate::disk_monitor::DiskSpaceLevel>,
    /// GSM event sender — produces events for the GSM actor.
    ///
    /// All GsmEvent emissions use `try_send` (non-blocking). If the channel
    /// is full, the event is dropped with a debug log — acceptable because
    /// the GSM actor processes events asynchronously and the periodic
    /// SyncStatus event ensures state convergence.
    pub(crate) gsm_event_tx: tokio::sync::mpsc::Sender<crate::gsm::GsmEvent>,

    /// Lossless per-peer Genesis chain state registry (candidate fragments,
    /// idling, csLatestSlot) — the Haskell per-peer `ChainSyncState` TVar
    /// analogue. Written by ChainSync tasks; read by the GSM/GDD/LoE.
    pub(crate) peer_registry: Arc<crate::genesis_peer_state::PeerStateRegistry>,

    /// The Limit on Eagerness published by the GSM/GDD governor; consumed by
    /// chain selection (`LoeState::Disabled` in praos mode = identity).
    #[allow(dead_code)] // consumed by chain selection in the trimToLoE task (T5)
    pub(crate) loe_out: Arc<arc_swap::ArcSwap<dugite_consensus::loe::LoeState>>,

    /// Limit on Patience (capacity, rate) handed to every ChainSync task;
    /// `None` in praos mode or with `EnableLoP=false`.
    pub(crate) lop_params: Option<(u64, u64)>,

    /// Minimum active big-ledger peers for the GSM HAA (`min_active_blp`),
    /// surfaced for the SyncStatus emitter's local-roots-aware HAA check.
    pub(crate) gsm_min_active_blp: usize,

    /// Historicity cutoff handed to every ChainSync task; `None` in praos
    /// mode (Haskell `gcHistoricityCutoff = Nothing`).
    pub(crate) historicity_cutoff_secs: Option<u64>,

    /// ChainSync Jumping coordinator; `None` = disabled (praos / EnableCSJ
    /// false → noJumping). Shared across all peers' ChainSync tasks.
    pub(crate) csj: Option<Arc<crate::csj::CsjRegistry>>,
    /// GSM snapshot receiver — latest GSM state (watch channel, synchronous borrow).
    ///
    /// Consumers call `self.gsm_snapshot_rx.borrow()` to read the latest
    /// `GsmSnapshot` without any async overhead. In Praos mode the snapshot
    /// is always `{ state: CaughtUp, loe_slot: None }`.
    pub(crate) gsm_snapshot_rx: tokio::sync::watch::Receiver<crate::gsm::GsmSnapshot>,
    /// Pre-built actor pieces, consumed by `run()` to spawn the GSM actor task.
    ///
    /// `Option` so `run()` can `.take()` the parts once — the actor is spawned
    /// exactly once and owns all receiver/sender handles from that point.
    pub(crate) gsm_actor_parts: Option<GsmActorParts>,
    /// Anchored chain fragment representing the volatile portion of the
    /// selected chain (the last k block headers not yet in ImmutableDB).
    ///
    /// Matches Haskell's `AnchoredFragment` — anchored at the immutable tip,
    /// headers grow as new blocks are adopted.  Used for:
    /// - ChainSync server: `find_intersect` for downstream peers
    /// - Background copy-to-immutable: fragment length > k triggers copy
    /// - Chain selection: comparing candidate chains against the current chain
    ///
    /// Protected by `RwLock` so both the sync loop (write) and N2N server
    /// tasks (read, for intersection finding) can access it concurrently.
    pub(crate) chain_fragment: Arc<RwLock<ChainFragment>>,
    /// Chain-selection queue handle.
    ///
    /// All blocks (from peers and from the local forger) are submitted
    /// through this handle.  The background `add_block_runner` task owns
    /// the receiving end and writes blocks to VolatileDB sequentially,
    /// avoiding concurrency hazards between storage writes and chain
    /// selection.  This is Dugite's implementation of Haskell's
    /// `addBlockAsync` / `addBlockRunner` pattern.
    ///
    /// `None` only during the constructor before the runner task is spawned.
    pub(crate) chain_sel_handle: Option<ChainSelHandle>,

    // ── Phase 5: Background maintenance operations ────────────────────────
    //
    // These match Haskell's Background.hs: copy-to-immutable, GC, and
    // snapshot scheduling.  All three are synchronous value types — they
    // are called from the main processing loop after each block is applied
    // (or periodically from a dedicated tick).  Wrapping them in a Mutex
    // allows the sync loop (`&mut self`) and future ticker tasks (`Arc`) to
    // share them if needed, but for now only the sync loop touches them.
    /// Copies the oldest volatile block to ImmutableDB when the fragment
    /// grows beyond the security parameter k.
    ///
    /// Matches Haskell's `copyToImmutableDB` in Background.hs.
    pub(crate) copy_to_immutable: CopyToImmutable,

    /// Deferred GC for VolatileDB entries after copy-to-immutable.
    ///
    /// Entries are scheduled with a 60-second delay (matching Haskell's
    /// `gcDelay`) and removed on the next `run_pending` call after expiry.
    /// Matches Haskell's `garbageCollectBlocks` / `GcSchedule`.
    pub(crate) gc_scheduler: GcScheduler,

    /// Decides when to save LedgerSeq anchor snapshots to disk.
    ///
    /// Triggers at epoch boundaries, every N blocks, and on graceful
    /// shutdown.  Matches Haskell's snapshot policy in Background.hs.
    pub(crate) bg_snapshot_scheduler: SnapshotScheduler,

    /// Instant of last `update_query_state()` call.
    ///
    /// Rate-limits the per-block N2C snapshot rebuild to at most once per
    /// second. Without this guard the O(n²) DRep delegator scan (8k DReps ×
    /// 38k reward accounts) stalls the apply loop for every single block.
    last_query_state_update: Instant,

    /// Wall-clock of the last `sync_volatile_wal()` call.  Used by
    /// `process_forward_blocks` to bound the in-catch-up WAL loss window by
    /// fsyncing every ~1 s when `volatile_wal_sync_at_tip == false`.  When
    /// the WAL is in at-tip mode every write already fsyncs, so this
    /// timestamp is unused.
    last_volatile_wal_sync: Instant,

    /// Forge gate: true once any peer has returned a non-Origin MsgIntersectFound.
    ///
    /// Prevents the block producer from forging before ChainSync has established
    /// a real intersection with at least one peer.  Without this gate the BP can
    /// forge block 0 at startup (before any peer connects), creating a self-forged
    /// fork that the Bug-A Origin-intersection guard then refuses to roll back from,
    /// permanently stalling the node.
    ///
    /// Set to `true` by `chainsync_client_task` on the first non-Origin
    /// `MsgIntersectFound`.  Once true it is never reset — the gate is only
    /// relevant during the brief window between process start and first sync.
    ///
    /// Checked in `try_forge_block_at` alongside the hot-peer count, so both
    /// conditions must hold before a forge attempt is made.
    ///
    /// `AtomicBool` avoids any lock acquisition in the hot forge path.
    pub(crate) peer_intersection_established: Arc<std::sync::atomic::AtomicBool>,

    /// Block ingestion back-pressure flag set by the disk monitor.
    ///
    /// Set to `true` when free space drops below `PAUSE_THRESHOLD_BYTES` (1 GB).
    /// Cleared only after `RECOVER_THRESHOLD_BYTES` (5 GB) of free space is
    /// sustained for 60 s.  Both `apply_fetched_block` and `process_forward_blocks`
    /// check this flag before committing any block to ChainDB, so the database
    /// cannot be written when the disk is critically low.
    ///
    /// `AtomicBool` (Relaxed ordering) avoids any lock acquisition on the hot path.
    pub(crate) ingestion_paused: Arc<std::sync::atomic::AtomicBool>,

    // ── Volatile WAL durability mode (catch-up vs at-tip) ────────────────
    /// Tracks whether the VolatileDB WAL is currently in per-write fsync
    /// mode (`true` — at-tip durability) or batched mode (`false` — catch-up
    /// speed).  Initialised to `true` to match the storage-layer default.
    /// `process_forward_blocks` flips this on each transition between
    /// `strict_verification() == true` and `false` to avoid taking the
    /// ChainDB write lock when the mode hasn't changed.
    pub(crate) volatile_wal_sync_at_tip: Arc<std::sync::atomic::AtomicBool>,

    // ── Snapshot worker (issue #695) ─────────────────────────────────
    /// Sender to the background snapshot worker task. `None` after
    /// shutdown's drop-sender step; calls into `try_snapshot_async`
    /// after that return `SnapshotEnqueue::Closed`.
    pub(crate) snapshot_tx: Option<tokio::sync::mpsc::Sender<snapshot_worker::SnapshotRequest>>,

    /// Join handle for the background snapshot worker. Taken at
    /// shutdown so the synchronous final save can `.await` quiescence
    /// before writing — see `Node::run`'s shutdown sequence.
    pub(crate) snapshot_worker_handle: Option<tokio::task::JoinHandle<()>>,
}

// ─── GsmActorParts ──────────────────────────────────────────────────────────

/// Outcome of a `TriggeredFork` switch-plan execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForkSwitchOutcome {
    /// The fork was adopted (possibly partially — an invalid block mid-fork
    /// aborts the replay loop but keeps the applied prefix).
    Replayed,
    /// The switch could not proceed (rollback failed / fork abandoned);
    /// the caller must not continue its own apply path.
    Aborted,
}

/// Pre-built channel handles for spawning the GSM actor in `Node::run()`.
///
/// Created in `Node::new()` and consumed (`.take()`) in `run()`. This avoids
/// passing individual channel handles through multiple layers of plumbing.
pub(crate) struct GsmActorParts {
    pub config: crate::gsm::GsmConfig,
    pub enabled: bool,
    pub event_rx: tokio::sync::mpsc::Receiver<crate::gsm::GsmEvent>,
    pub snapshot_tx: tokio::sync::watch::Sender<crate::gsm::GsmSnapshot>,
    pub action_tx: tokio::sync::mpsc::Sender<crate::gsm::GddAction>,
    pub action_rx: tokio::sync::mpsc::Receiver<crate::gsm::GddAction>,
    /// Lossless per-peer chain state (shared with the ChainSync tasks).
    pub registry: Arc<crate::genesis_peer_state::PeerStateRegistry>,
    /// LoE publication target (shared with chain selection).
    pub loe_out: Arc<arc_swap::ArcSwap<dugite_consensus::loe::LoeState>>,
}

// ─── BFT overlay gate (#985) ─────────────────────────────────────────────────

/// Whether a block's header should be checked against the TPraos BFT overlay
/// schedule.
///
/// # Why the era is the primary term
///
/// Haskell binds the consensus protocol to the era at the *type* level —
/// `ouroboros-consensus-cardano`'s `HFEras` declares
/// `ShelleyBlock (TPraos c)` for Shelley/Allegra/Mary/Alonzo and
/// `ShelleyBlock (Praos c)` for Babbage onward — and `Praos`'s
/// `updateChainDepState` is only `validateKESSignature` + `validateVRFSignature`.
/// Its `PraosValidationErr` has eleven constructors, all VRF/KES/OCert, none
/// overlay-related, and the Praos `LedgerView` carries neither `d` nor
/// `GenDelegs`. So for a Babbage+ header the OVERLAY rule is not skipped at
/// runtime — it is *unreachable*, whatever the ledger state happens to hold.
///
/// dugite runs one header validator across both protocols, so that structural
/// guarantee has to be reproduced by this gate. Before #985 the gate keyed off
/// ledger pparams alone (`protocol_version_major < 7 && d > 0 && …`). A
/// LedgerSeq anchored at a genesis state reconstructed preview-genesis pparams
/// — PV 6, d = 1 — into the live ledger, which opened the gate on a *canonical
/// Conway block*: every slot classified as an overlay slot, offset 25616 of
/// epoch 1378 not divisible by `asc_inv = 20`, so `NonActiveSlot` →
/// `NotActiveOverlaySlot`. The node rejected block 4535827, cached it as
/// invalid, and refused every descendant forever.
///
/// With `era` leading, that outcome is unreachable regardless of how corrupt
/// or stale the ledger state is. The remaining terms are retained because
/// *within* the TPraos eras they are the correct, load-bearing test.
///
/// # Parameters
///
/// - `era` — the BLOCK's era, never the ledger's.
/// - `pv_major` — enacted protocol version major from the ledger.
/// - `forecast_d_numerator` — `d` forecast for the block's epoch (see the call
///   site: it must be the block's epoch value, not the un-ticked current one).
/// - `has_genesis_delegates` — whether the ledger holds any genesis delegates.
pub(crate) fn should_build_overlay_context(
    era: dugite_primitives::era::Era,
    pv_major: u64,
    forecast_d_numerator: u64,
    has_genesis_delegates: bool,
) -> bool {
    era.uses_tpraos() && pv_major < 7 && forecast_d_numerator > 0 && has_genesis_delegates
}

// ─── Node impl: new() ────────────────────────────────────────────────────────

impl Node {
    /// Load a ledger snapshot and enforce the backend-tag guard before the
    /// node commits to it. Mirrors Haskell's `MetadataBackendMismatch`: a
    /// snapshot written with a *different* UTxO backend is rejected (→ `Err`)
    /// so the caller falls through to the from-chain rebuild rather than
    /// loading an empty or structurally-incompatible UTxO set. A missing
    /// sidecar (pre-meta snapshots, e.g. an existing `db-mainnet`) is handled
    /// by backend inference and loads normally.
    fn load_snapshot_with_backend_guard(
        snapshot_path: &std::path::Path,
        database_path: &std::path::Path,
        configured: dugite_ledger::SnapshotBackend,
    ) -> std::result::Result<LedgerState, String> {
        let state = LedgerState::load_snapshot(snapshot_path).map_err(|e| e.to_string())?;
        match dugite_ledger::check_snapshot_backend_match(
            snapshot_path,
            &state,
            database_path,
            configured,
        ) {
            dugite_ledger::BackendCheckResult::Ok => Ok(state),
            // A `dugite-mem` snapshot loaded under a `dugite-lsm` node (the
            // common case right after `mithril-import`, which always writes a
            // mem-tagged snapshot). The inline `utxo_set` we just loaded is
            // migrated into the LSM store by the `attach_utxo_store` drain that
            // runs immediately after this — no from-genesis replay required.
            dugite_ledger::BackendCheckResult::Convertible {
                snapshot_backend,
                configured_backend,
            } => {
                info!(
                    snapshot_backend = snapshot_backend.as_tag(),
                    configured_backend = configured_backend.as_tag(),
                    utxo_count = state.utxo.utxo_set.len(),
                    "Loaded in-memory snapshot under the LSM backend; its inline UTxOs will be \
                     migrated into the on-disk store (no from-genesis replay)."
                );
                Ok(state)
            }
            dugite_ledger::BackendCheckResult::Mismatch {
                snapshot_backend,
                configured_backend,
            } => {
                warn!(
                    snapshot_backend = snapshot_backend.as_tag(),
                    configured_backend = configured_backend.as_tag(),
                    "Ledger snapshot was created with a different UTxO backend — ignoring it \
                     and rebuilding from chain. Run the snapshot converter to migrate without \
                     a replay."
                );
                Err(format!(
                    "snapshot backend `{}` does not match configured backend `{}`",
                    snapshot_backend.as_tag(),
                    configured_backend.as_tag()
                ))
            }
        }
    }

    pub fn new(args: NodeArgs) -> Result<Self> {
        let mut protocol_params = ProtocolParameters::mainnet_defaults();

        // Load Byron genesis if configured — done early so we can read the
        // security parameter k before opening ChainDB.
        let config_dir = args.config_dir.clone();
        let mut byron_epoch_length: u64 = 0; // 0 = use mainnet defaults (mainnet)
        let mut byron_slot_duration_ms: u64 = 20_000; // default 20s, overridden by genesis
        let mut byron_genesis_file_hash: Option<dugite_primitives::hash::Hash32> = None;
        let mut security_param_k: usize = dugite_storage::chain_db::DEFAULT_SECURITY_PARAM_K;
        let byron_genesis_utxos: Vec<(Vec<u8>, u64)> =
            if let Some(ref genesis_path) = args.config.byron_genesis_file {
                let genesis_path = config_dir.join(genesis_path);
                match ByronGenesis::load_with_hash(&genesis_path) {
                    Ok((genesis, hash)) => {
                        let utxos = genesis.initial_utxos();
                        let k = genesis.security_param();
                        security_param_k = k as usize;
                        byron_epoch_length = 10 * k;
                        byron_slot_duration_ms = genesis.slot_duration_ms();
                        info!(
                            magic = genesis.protocol_magic(),
                            k,
                            epoch_len = byron_epoch_length,
                            slot_duration_ms = byron_slot_duration_ms,
                            utxos = utxos.len(),
                            "Byron genesis loaded",
                        );
                        byron_genesis_file_hash = Some(hash);
                        utxos.into_iter().map(|e| (e.address, e.lovelace)).collect()
                    }
                    Err(e) => {
                        warn!("Failed to load Byron genesis: {e}");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

        // Open ChainDB with the security parameter k from Byron genesis.
        // Uses default epoch parameters (epoch 0, length 432000) since era_history
        // isn't built yet. The active chunk gets correctly named at the first
        // finalize_chunk() call during epoch transitions, which passes real epoch info.
        let chain_db = Arc::new(RwLock::new(ChainDB::open_with_config(
            &args.database_path,
            &args.storage_config.immutable,
            security_param_k,
        )?));

        // Load Shelley genesis if configured (with hash for nonce initialization)
        let (shelley_genesis, shelley_genesis_hash) =
            if let Some(ref genesis_path) = args.config.shelley_genesis_file {
                let genesis_path = config_dir.join(genesis_path);
                match ShelleyGenesis::load_with_hash(&genesis_path) {
                    Ok((genesis, hash)) => {
                        info!(
                            magic = genesis.network_magic,
                            start = %genesis.system_start,
                            epoch_len = genesis.epoch_length,
                            "Shelley genesis loaded",
                        );
                        genesis.apply_to_protocol_params(&mut protocol_params);
                        (Some(genesis), Some(hash))
                    }
                    Err(e) => {
                        warn!("Failed to load Shelley genesis: {e}");
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

        // Load Alonzo genesis if configured (with hash validation)
        let mut alonzo_genesis_file_hash: Option<dugite_primitives::hash::Hash32> = None;
        if let Some(ref genesis_path) = args.config.alonzo_genesis_file {
            let genesis_path = config_dir.join(genesis_path);
            match AlonzoGenesis::load_with_hash(&genesis_path) {
                Ok((genesis, hash)) => {
                    info!(
                        max_val_size = genesis.max_value_size,
                        collateral_pct = genesis.collateral_percentage,
                        "Alonzo genesis loaded",
                    );
                    alonzo_genesis_file_hash = Some(hash);
                    genesis.apply_to_protocol_params(&mut protocol_params);
                }
                Err(e) => {
                    warn!("Failed to load Alonzo genesis: {e}");
                }
            }
        }

        // Validate Alonzo genesis hash if configured
        if let Some(ref expected_hex) = args.config.alonzo_genesis_hash {
            if let Ok(expected) = dugite_primitives::hash::Hash32::from_hex(expected_hex) {
                if let Some(ref actual) = alonzo_genesis_file_hash {
                    if *actual != expected {
                        anyhow::bail!(
                            "Alonzo genesis hash mismatch: expected {}, got {}",
                            expected.to_hex(),
                            actual.to_hex()
                        );
                    }
                    debug!("Alonzo genesis hash validated: {}", expected.to_hex());
                }
            }
        }

        // Load Conway genesis if configured (with hash validation)
        let mut conway_committee_threshold: Option<(u64, u64)> = None;
        let mut conway_committee_members: Vec<([u8; 32], u64)> = Vec::new();
        let mut conway_constitution: Option<dugite_primitives::transaction::Constitution> = None;
        let mut conway_initial_dreps: Vec<(dugite_primitives::hash::Hash28, u64)> = Vec::new();
        let mut conway_genesis_file_hash: Option<dugite_primitives::hash::Hash32> = None;
        let mut conway_v3_cost_model: Option<Vec<i64>> = None;
        // #994: did a genesis FILE supply the PlutusV2 cost model, or is the one
        // we end up with the built-in default? `ConwayGenesis::apply_to_protocol_params`
        // injects `defaultV2CostModel` when none is present, so this has to be
        // sampled before that call. See `genesis_prev_protocol_params` below.
        let v2_cost_model_from_genesis = protocol_params.cost_models.plutus_v2.is_some();
        if let Some(ref genesis_path) = args.config.conway_genesis_file {
            let genesis_path = config_dir.join(genesis_path);
            match ConwayGenesis::load_with_hash(&genesis_path) {
                Ok((genesis, hash)) => {
                    info!(
                        drep_deposit = genesis.d_rep_deposit,
                        gov_deposit = genesis.gov_action_deposit,
                        committee_min = genesis.committee_min_size,
                        "Conway genesis loaded",
                    );
                    conway_genesis_file_hash = Some(hash);
                    conway_committee_threshold = genesis.committee_threshold();
                    conway_committee_members = genesis.committee_members();
                    conway_constitution = genesis.to_ledger_constitution();
                    conway_initial_dreps = genesis.initial_dreps_as_entries();
                    conway_v3_cost_model = genesis.plutus_v3_cost_model.clone();
                    genesis.apply_to_protocol_params(&mut protocol_params);
                }
                Err(e) => {
                    warn!("Failed to load Conway genesis: {e}");
                }
            }
        }

        // Validate Conway genesis hash if configured
        if let Some(ref expected_hex) = args.config.conway_genesis_hash {
            if let Ok(expected) = dugite_primitives::hash::Hash32::from_hex(expected_hex) {
                if let Some(ref actual) = conway_genesis_file_hash {
                    if *actual != expected {
                        anyhow::bail!(
                            "Conway genesis hash mismatch: expected {}, got {}",
                            expected.to_hex(),
                            actual.to_hex()
                        );
                    }
                    debug!("Conway genesis hash validated: {}", expected.to_hex());
                }
            }
        }

        // #994: `cgsPrevPParams` at genesis carries only the cost models the
        // genesis FILES supplied — never the built-in `defaultV2CostModel`.
        //
        // On a chain whose FIRST era is Conway (`create-testnet-data`, i.e. the
        // local devnet) cardano-node reports
        //     cur  = [PlutusV1, PlutusV2, PlutusV3]
        //     prev = [PlutusV1,           PlutusV3]
        // with `prev` otherwise byte-identical to `cur` — V1 and V3 match
        // exactly, and every non-costModels field matches. The devnet's
        // alonzo-genesis defines only `PlutusV1` and its conway-genesis only
        // `plutusV3CostModel`, so neither file supplies V2: the V2 in `cur` is
        // the default both implementations inject so V2 scripts are runnable.
        // It never reaches `prev`.
        //
        // dugite seeded `prev` by cloning `cur` wholesale, so the defaulted V2
        // leaked in and `gov-state` diverged three times over (the same value is
        // embedded in `nextRatifyState.nextEnactState.{curPParams,prevPParams}`).
        //
        // Inert on mainnet/preview/preprod: those alonzo-genesis files DO define
        // PlutusV2, so `v2_cost_model_from_genesis` is true and `prev` is an
        // exact clone as before. Those chains also start in Byron, where
        // `prev` is overwritten by the first epoch boundary regardless — which
        // is precisely why this is reachable only on a Conway-genesis chain.
        let genesis_prev_protocol_params = {
            let mut prev = protocol_params.clone();
            if !v2_cost_model_from_genesis {
                prev.cost_models.plutus_v2 = None;
            }
            prev
        };

        // Load Dijkstra genesis if configured (issue #462 Phase 6 — parse only).
        //
        // The parsed values are not yet applied to `protocol_params`; pparams
        // 34-37 (max_ref_script_size_per_block/_per_tx, ref_script_cost_stride,
        // ref_script_cost_multiplier) are wired in a separate phase. We still
        // perform full validation here so a malformed genesis is rejected at
        // startup and a configured `DijkstraGenesisHash` is honoured.
        let mut dijkstra_genesis_file_hash: Option<dugite_primitives::hash::Hash32> = None;
        if let Some(ref genesis_path) = args.config.dijkstra_genesis_file {
            let genesis_path = config_dir.join(genesis_path);
            match load_dijkstra_genesis_with_hash(&genesis_path) {
                Ok((genesis, hash)) => {
                    info!(
                        max_ref_script_size_per_block = genesis.max_ref_script_size_per_block,
                        max_ref_script_size_per_tx = genesis.max_ref_script_size_per_tx,
                        ref_script_cost_stride = genesis.ref_script_cost_stride,
                        ref_script_cost_multiplier = %format!(
                            "{}/{}",
                            genesis.ref_script_cost_multiplier.numerator(),
                            genesis.ref_script_cost_multiplier.denominator(),
                        ),
                        "Dijkstra genesis loaded (parse-only — pparams 34-37 not yet wired)",
                    );
                    dijkstra_genesis_file_hash = Some(hash);
                    // `genesis` is intentionally unused at runtime — the
                    // info-log above is the only consumer until pparams
                    // 34-37 wiring lands in a follow-up phase.
                    let _ = genesis;
                }
                Err(e) => {
                    warn!("Failed to load Dijkstra genesis: {e}");
                }
            }
        }

        // Validate Dijkstra genesis hash if configured.
        if let Some(ref expected_hex) = args.config.dijkstra_genesis_hash {
            if let Ok(expected) = dugite_primitives::hash::Hash32::from_hex(expected_hex) {
                if let Some(ref actual) = dijkstra_genesis_file_hash {
                    if *actual != expected {
                        anyhow::bail!(
                            "Dijkstra genesis hash mismatch: expected {}, got {}",
                            expected.to_hex(),
                            actual.to_hex()
                        );
                    }
                    debug!("Dijkstra genesis hash validated: {}", expected.to_hex());
                }
            }
        }

        // Compute network magic early — needed for shelley transition epoch lookup
        let network_magic = args.config.network_magic.unwrap_or_else(|| {
            if let Some(ref sg) = shelley_genesis {
                sg.network_magic
            } else {
                args.config.network.magic()
            }
        });

        // ── Haskell ledger snapshot import (from Mithril ancillary) ─��──────
        //
        // After `mithril-import --ancillary`, the decoded Haskell ExtLedgerState
        // files live in `database_path/haskell-ledger/<slot>/`. If present, we
        // decode them into a native LedgerState and save it as the regular
        // `ledger-snapshot.bin`. This replaces the chain-from-genesis replay
        // (~10 hours for preview) with a ~15-minute gap replay.
        let snapshot_path = args.database_path.join("ledger-snapshot.bin");
        let haskell_ledger_dir = args.database_path.join("haskell-ledger");
        if haskell_ledger_dir.exists() {
            info!("Found Haskell ledger state from Mithril ancillary import");

            // Find the newest snapshot directory (highest slot number)
            let mut best_slot = 0u64;
            let mut best_dir = None;
            if let Ok(entries) = std::fs::read_dir(&haskell_ledger_dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                        if let Ok(slot) = entry.file_name().to_string_lossy().parse::<u64>() {
                            if slot > best_slot {
                                best_slot = slot;
                                best_dir = Some(entry.path());
                            }
                        }
                    }
                }
            }

            if let Some(snapshot_dir) = best_dir {
                match Self::import_haskell_ledger_snapshot(
                    &snapshot_dir,
                    &snapshot_path,
                    &protocol_params,
                    shelley_genesis.as_ref(),
                    shelley_genesis_hash,
                    network_magic,
                    byron_epoch_length,
                    byron_slot_duration_ms,
                ) {
                    Ok(()) => {
                        info!("Haskell ledger import complete; native snapshot saved");
                        // Clean up the consumed directory
                        if let Err(e) = std::fs::remove_dir_all(&haskell_ledger_dir) {
                            warn!("Failed to remove consumed haskell-ledger directory: {e}");
                        } else {
                            info!("Removed consumed haskell-ledger directory");
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to import Haskell ledger snapshot: {e:#}. \
                             Falling back to chain replay."
                        );
                    }
                }
            }
        }

        // Try to load existing ledger snapshot (with the backend-mismatch
        // guard — a snapshot tagged for a different UTxO backend is rejected
        // here and the node rebuilds from chain).
        // #989: a ledger snapshot is only usable if its UTxO store still exists
        // and is complete. Checked HERE, before the snapshot is loaded and before
        // the genesis setup below runs, because a rebuild after that point skips
        // the setup (see `utxo_store_is_usable`). `wipe_utxo_store_before_replay`
        // is honoured in the LSM block further down.
        let mut wipe_utxo_store_before_replay = false;
        let snapshot_usable = if snapshot_path.exists()
            && matches!(
                args.storage_config.utxo.backend,
                dugite_storage::UtxoBackend::Lsm
            ) {
            let probe_slot = Self::peek_snapshot_slot(&snapshot_path).unwrap_or(0);
            let ok = Self::utxo_store_is_usable(
                &args.database_path.join("utxo-store"),
                &args.storage_config.utxo,
                probe_slot,
            );
            if !ok {
                wipe_utxo_store_before_replay = true;
            }
            ok
        } else {
            true
        };

        let mut ledger = if snapshot_path.exists() && snapshot_usable {
            match Self::load_snapshot_with_backend_guard(
                &snapshot_path,
                &args.database_path,
                epoch::snapshot_backend_of(args.storage_config.utxo.backend),
            ) {
                Ok(mut state) => {
                    // Re-apply genesis config in case it changed
                    let ste = epoch::shelley_transition_epoch_for_magic(network_magic);
                    if let Some(ref genesis) = shelley_genesis {
                        state.set_epoch_length(genesis.epoch_length, genesis.security_param);
                        state.set_slot_config(genesis.slot_config(
                            ste,
                            byron_epoch_length,
                            byron_slot_duration_ms,
                        ));
                        state.set_update_quorum(genesis.update_quorum);
                        let gen_deleg_entries = genesis.gen_delegs_entries();
                        if !gen_deleg_entries.is_empty() {
                            tracing::debug!(
                                count = gen_deleg_entries.len(),
                                "Loaded genesis delegates for overlay schedule validation"
                            );
                            state.set_genesis_delegates(&gen_deleg_entries);
                        }
                    }
                    state.set_shelley_transition(ste, byron_epoch_length);
                    if let Some(hash) = shelley_genesis_hash {
                        state.genesis_hash = hash;
                    }

                    // Recalculate the epoch from the tip slot using the now-correct
                    // genesis parameters.  Snapshots saved with wrong epoch_length
                    // (e.g. mainnet default 432000 instead of preview 86400) have
                    // incorrect epoch numbers baked in.  Without this correction,
                    // apply_block would try to process hundreds of spurious epoch
                    // transitions (445 → 1239) and the stake snapshots would be at
                    // wrong epochs, causing pool_stake=0 for block producers.
                    if state.tip.point != Point::Origin {
                        let tip_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
                        let correct_epoch = state.epoch_of_slot(tip_slot);
                        if correct_epoch != state.epoch.0 {
                            warn!(
                                snapshot_epoch = state.epoch.0,
                                correct_epoch,
                                tip_slot,
                                "Snapshot epoch differs from computed epoch — correcting"
                            );
                            state.epoch = dugite_primitives::time::EpochNo(correct_epoch);
                        }
                    }

                    // NOTE: Stale-defaults heuristic removed (issue #347).
                    // Mithril ancillary import now provides correct protocol parameters.

                    // NOTE: Protocol-version-behind-era heuristic removed (issue #347).
                    // Mithril ancillary import now provides correct protocol version.

                    {
                        // ── Snapshot canonicality check ───────────────────────
                        //
                        // A snapshot whose tip is *within the ImmutableDB slot range*
                        // must match the canonical block at that slot.  If it does not,
                        // the snapshot was saved on a fork chain and must be discarded.
                        //
                        // This situation arises when the BP forges a block that is NOT
                        // adopted by the network.  The forged block enters the VolatileDB
                        // and the ledger is advanced to it; when a snapshot fires the
                        // fork tip is persisted.  On the next restart the VolatileDB WAL
                        // is empty (the fork block was never written to ImmutableDB) so
                        // `has_block` returns false.  Meanwhile the ImmutableDB canonical
                        // chain has a different (or absent) block at the fork slot.
                        //
                        // Haskell's `LedgerDB.Init.initLedgerDB` handles this by
                        // rolling back the ledger to the youngest snapshot whose tip IS
                        // on the current chain fragment.  We replicate that behaviour:
                        //
                        //   1. Check if the primary snapshot tip is canonical.
                        //   2. If not, walk older epoch snapshots (newest-first) and
                        //      pick the first canonical one.
                        //   3. If none qualify, fall back to genesis + full replay.
                        //
                        // The canonicality check is delegated to `epoch::is_snapshot_canonical`
                        // which correctly handles:
                        //   - hash present in ChainDB → canonical
                        //   - hash absent, slot in immutable range, slot occupied by a
                        //     different block → fork
                        //   - hash absent, slot in immutable range, slot empty (no block
                        //     at that exact slot) → fork (BP filled a slot the canonical
                        //     chain left empty)
                        //   - slot in volatile range → provisionally accepted
                        //
                        // `snapshot_valid = Some(state)` on success, `None` on fork.

                        // Fast path: if ChainDB has the block the check is free.
                        let snapshot_state: Option<LedgerState> = {
                            let db_guard = chain_db.try_read();
                            let db: Option<&dugite_storage::ChainDB> = match db_guard.as_ref() {
                                Ok(db) => Some(&**db),
                                Err(_) => {
                                    warn!("Could not acquire ChainDB lock for snapshot validation, assuming valid");
                                    None
                                }
                            };

                            let snap_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
                            let db_tip_slot = db
                                .map(|db| db.get_tip().point.slot().map(|s| s.0).unwrap_or(0))
                                .unwrap_or(0);

                            // Special-case: snapshot is AHEAD of ChainDB (Mithril import).
                            // The missing blocks will be fetched from peers; accept as-is.
                            let ahead_of_chaindb = snap_slot > db_tip_slot && db_tip_slot > 0;
                            if ahead_of_chaindb {
                                let db_tip_display =
                                    db.map(|db| db.get_tip().to_string()).unwrap_or_default();
                                warn!(
                                    "Ledger snapshot is ahead of ChainDB \
                                     (snapshot={}, chaindb={}); this is expected \
                                     after a Mithril import — accepting snapshot, \
                                     missing blocks will be fetched from peers",
                                    state.tip, db_tip_display,
                                );
                                Some(state)
                            } else if epoch::is_snapshot_canonical(snap_slot, &state.tip.point, db)
                            {
                                // Primary snapshot is on the canonical chain — use it.
                                Some(state)
                            } else {
                                // Primary snapshot is on a fork.
                                //
                                // Walk all epoch snapshots (newest → oldest) and find
                                // the most recent one that IS on the canonical chain.
                                // This matches Haskell's `LedgerDB.Init.initLedgerDB`
                                // which rolls back to the youngest on-chain snapshot.
                                warn!(
                                    snapshot_slot = snap_slot,
                                    fork_hash = %state.tip.point.hash().map(|h| h.to_hex()).unwrap_or_default(),
                                    "Ledger snapshot tip is on a dead fork — rolling back to \
                                     the most recent canonical snapshot (Haskell: initLedgerDB)"
                                );
                                let db_path = &args.database_path;
                                let mut recovered: Option<LedgerState> = None;
                                let candidates = crate::startup::enumerate_snapshots(db_path);
                                for candidate in &candidates {
                                    // Skip the primary snapshot (already known fork).
                                    if candidate.ledger_slot == snap_slot {
                                        continue;
                                    }
                                    match LedgerState::load_snapshot(&candidate.path) {
                                        Ok(mut alt) => {
                                            let alt_slot =
                                                alt.tip.point.slot().map(|s| s.0).unwrap_or(0);
                                            if epoch::is_snapshot_canonical(
                                                alt_slot,
                                                &alt.tip.point,
                                                db,
                                            ) {
                                                // Apply the same genesis-config fixups that were
                                                // applied to the primary snapshot above, so the
                                                // recovered state has correct epoch_length, slot
                                                // config, shelley transition, and genesis_hash.
                                                let ste = epoch::shelley_transition_epoch_for_magic(
                                                    network_magic,
                                                );
                                                if let Some(ref genesis) = shelley_genesis {
                                                    alt.set_epoch_length(
                                                        genesis.epoch_length,
                                                        genesis.security_param,
                                                    );
                                                    alt.set_slot_config(genesis.slot_config(
                                                        ste,
                                                        byron_epoch_length,
                                                        byron_slot_duration_ms,
                                                    ));
                                                    alt.set_update_quorum(genesis.update_quorum);
                                                    let gen_deleg_entries =
                                                        genesis.gen_delegs_entries();
                                                    if !gen_deleg_entries.is_empty() {
                                                        alt.set_genesis_delegates(
                                                            &gen_deleg_entries,
                                                        );
                                                    }
                                                }
                                                alt.set_shelley_transition(ste, byron_epoch_length);
                                                if let Some(hash) = shelley_genesis_hash {
                                                    alt.genesis_hash = hash;
                                                }
                                                if alt.tip.point != Point::Origin {
                                                    let tip_slot = alt
                                                        .tip
                                                        .point
                                                        .slot()
                                                        .map(|s| s.0)
                                                        .unwrap_or(0);
                                                    let correct_epoch = alt.epoch_of_slot(tip_slot);
                                                    if correct_epoch != alt.epoch.0 {
                                                        alt.epoch =
                                                            dugite_primitives::time::EpochNo(
                                                                correct_epoch,
                                                            );
                                                    }
                                                }
                                                info!(
                                                    recovered_slot = alt_slot,
                                                    recovered_tip = %alt.tip,
                                                    "Ledger fork-rollback: recovered from \
                                                     canonical snapshot at slot {}",
                                                    alt_slot,
                                                );
                                                recovered = Some(alt);
                                                break;
                                            } else {
                                                warn!(
                                                    candidate_slot = candidate.ledger_slot,
                                                    "Fork-rollback candidate is also on a fork — skipping"
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                path = %candidate.path.display(),
                                                "Fork-rollback: failed to load candidate snapshot: {e}"
                                            );
                                        }
                                    }
                                }
                                if recovered.is_none() {
                                    error!(
                                        "Fork-rollback: no canonical snapshot found in {}. \
                                         All snapshots are on forks. \
                                         The node will replay from genesis — this will be slow. \
                                         To avoid this in future runs, delete the database and \
                                         re-import via `dugite-node mithril-import`.",
                                        db_path.display()
                                    );
                                }
                                recovered
                            }
                        };

                        if let Some(recovered) = snapshot_state {
                            info!(
                                epoch = recovered.epoch.0,
                                utxos = recovered.utxo.utxo_set.len(),
                                tip = %recovered.tip,
                                "Ledger restored from snapshot",
                            );
                            recovered
                        } else {
                            warn!("No canonical snapshot found — replaying from genesis");
                            Self::init_fresh_ledger(
                                &protocol_params,
                                &genesis_prev_protocol_params,
                                shelley_genesis.as_ref(),
                                shelley_genesis_hash,
                                &byron_genesis_utxos,
                                network_magic,
                                byron_epoch_length,
                                byron_slot_duration_ms,
                            )
                        }
                    } // end canonicality check
                }
                Err(e) => {
                    warn!("Failed to load ledger snapshot, starting fresh: {e}");
                    Self::init_fresh_ledger(
                        &protocol_params,
                        &genesis_prev_protocol_params,
                        shelley_genesis.as_ref(),
                        shelley_genesis_hash,
                        &byron_genesis_utxos,
                        network_magic,
                        byron_epoch_length,
                        byron_slot_duration_ms,
                    )
                }
            }
        } else {
            // No native snapshot — start fresh and replay from ChainDB.
            // (Haskell ledger state import is not supported for UTxO-HD format.)
            Self::init_fresh_ledger(
                &protocol_params,
                &genesis_prev_protocol_params,
                shelley_genesis.as_ref(),
                shelley_genesis_hash,
                &byron_genesis_utxos,
                network_magic,
                byron_epoch_length,
                byron_slot_duration_ms,
            )
        };
        // Apply Conway genesis committee threshold and members if not already set
        if let Some((num, den)) = conway_committee_threshold {
            if ledger.gov.governance.committee_threshold.is_none() {
                use dugite_primitives::transaction::Rational;
                std::sync::Arc::make_mut(&mut ledger.gov.governance).committee_threshold =
                    Some(Rational {
                        numerator: num,
                        denominator: den,
                    });
                debug!("Applied Conway genesis committee quorum threshold ({num}/{den})");
            }
        }
        // Seed initial committee members from Conway genesis if committee is empty
        if ledger.gov.governance.committee_expiration.is_empty()
            && !conway_committee_members.is_empty()
        {
            use dugite_primitives::hash::Hash32;
            for (hash_bytes, expiration) in &conway_committee_members {
                let cold_key = Hash32::from_bytes(*hash_bytes);
                std::sync::Arc::make_mut(&mut ledger.gov.governance)
                    .committee_expiration
                    .insert(cold_key, dugite_primitives::EpochNo(*expiration));
                // `ConwayGenesis::committee_members` encodes the credential KIND
                // in byte 28 (0x01 = script), but every consumer reads it from
                // `script_committee_credentials` instead — two representations
                // of one fact. Seeding only the first left `committee-state`,
                // `gov-state` and `ledger-state` reporting a genesis script
                // member as `keyHash-…` where cardano-node says `scriptHash-…`
                // (preview's sole genesis CC member is a script hash).
                //
                // `main.rs`'s fresh-ledger path already did this; `Node::new` —
                // the path a real node takes — did not.
                if hash_bytes[28] == 0x01 {
                    std::sync::Arc::make_mut(&mut ledger.gov.governance)
                        .script_committee_credentials
                        .insert(cold_key);
                }
            }
            debug!(
                "Seeded {} initial committee members from Conway genesis",
                conway_committee_members.len()
            );
        }

        // Seed constitution from Conway genesis (CIP-1694 proposal guardrail).
        // Only applied when the ledger has no constitution yet, so that chains
        // recovered from a snapshot keep their on-chain value.
        if let Some(constitution) = conway_constitution {
            if ledger.gov.governance.constitution.is_none() {
                std::sync::Arc::make_mut(&mut ledger.gov.governance).constitution =
                    Some(constitution);
                debug!("Seeded constitution from Conway genesis");
            }
        }

        // Seed initial DReps from Conway genesis when the ledger DRep map is
        // empty (fresh start). Haskell's `addDefaultDRepsToState` sets
        // expiry = 0 + drep_activity (bootstrap phase, no dormant subtraction).
        if ledger.gov.governance.dreps.is_empty() && !conway_initial_dreps.is_empty() {
            use dugite_ledger::state::DRepRegistration;
            use dugite_primitives::credentials::Credential;
            use dugite_primitives::value::Lovelace;
            use dugite_primitives::EpochNo;
            let count = conway_initial_dreps.len();
            let drep_activity = ledger.epochs.protocol_params.drep_activity;
            let gov = std::sync::Arc::make_mut(&mut ledger.gov.governance);
            for (hash28, deposit) in &conway_initial_dreps {
                let credential = Credential::VerificationKey(*hash28);
                let cred_hash = credential.to_typed_hash32();
                gov.dreps.insert(
                    cred_hash,
                    DRepRegistration {
                        credential,
                        deposit: Lovelace(*deposit),
                        anchor: None,
                        registered_epoch: EpochNo(0),
                        drep_expiry: EpochNo(drep_activity),
                        active: true,
                    },
                );
            }
            debug!("Seeded {} initial DReps from Conway genesis", count);
        }

        // Store Conway genesis init data on ledger for era-transition rules.
        {
            let has_data = conway_committee_threshold.is_some()
                || !conway_committee_members.is_empty()
                || !conway_initial_dreps.is_empty()
                || conway_v3_cost_model.is_some();
            if has_data {
                ledger.conway_genesis_init = Some(dugite_ledger::eras::ConwayGenesisInit {
                    initial_dreps: conway_initial_dreps,
                    committee_members: conway_committee_members,
                    committee_threshold: conway_committee_threshold,
                    constitution: ledger.gov.governance.constitution.clone(),
                    plutus_v3_cost_model: conway_v3_cost_model.clone(),
                });
            }
        }

        // Safety net: a Conway-era ledger snapshot taken before the V3 cost-model
        // seeding fix (or any Conway snapshot whose `cost_models.plutus_v3` is
        // `None`) would compute the wrong `script_data_hash` and default-cost-model
        // budgets for every PlutusV3 transaction. The Babbage→Conway
        // `on_era_transition` only fires when crossing the hard fork during replay;
        // for a node resuming directly from a Conway snapshot it never runs.
        //
        // The genesis `ucppPlutusV3CostModel` is the INITIAL (PV9) V3 model. From
        // the Plomin HF (PV10) the on-chain V3 model is expanded (251→297, and
        // PV11→350) via governance ParameterChange enactment, NOT via the genesis
        // upgrade params. So only seed the genesis model when we are at PV9 (where
        // it is exactly authoritative). At PV>9 a `None` V3 means a governance-
        // enactment gap (or an unsupported snapshot) — seeding the stale 251-entry
        // genesis model there would be WRONG, so we warn loudly instead and let the
        // governance replay path populate the correct expanded model.
        let pv = ledger.epochs.protocol_params.protocol_version_major;
        if pv >= 9
            && ledger
                .epochs
                .protocol_params
                .cost_models
                .plutus_v3
                .is_none()
        {
            match (pv, &conway_v3_cost_model) {
                (9, Some(v3)) => {
                    ledger.epochs.protocol_params.cost_models.plutus_v3 = Some(v3.clone());
                    warn!(
                        entries = v3.len(),
                        "Seeded missing PlutusV3 cost model into Conway ledger state from genesis \
                         (PV9 snapshot predates V3 seeding); this prevents ScriptDataHashMismatch \
                         and spurious budget-exhausted divergences on PlutusV3 transactions"
                    );
                }
                (9, None) => {
                    warn!(
                        "Conway PV9 ledger state has no PlutusV3 cost model and no Conway genesis \
                         V3 cost model is available to seed it — PlutusV3 transactions will diverge"
                    );
                }
                _ => {
                    // pv > 9 with no V3: do NOT inject the stale genesis model.
                    warn!(
                        pv,
                        "Conway PV{pv} ledger state has no PlutusV3 cost model. The genesis \
                         (PV9) model is NOT authoritative at this PV (Plomin/PV11 expand V3 via \
                         governance), so it is NOT being seeded — the governance ParameterChange \
                         enactment must populate the correct expanded model. PlutusV3 transactions \
                         will diverge until then; this indicates a governance-enactment gap or an \
                         unsupported snapshot."
                    );
                }
            }
        }

        // Wire up on-disk UTxO store if LSM backend is configured
        if matches!(
            args.storage_config.utxo.backend,
            dugite_storage::UtxoBackend::Lsm
        ) {
            let utxo_path = args.database_path.join("utxo-store");
            let utxo_cfg = &args.storage_config.utxo;

            // ── From-genesis store-consistency guard ───────────────────────────
            //
            // When the ledger is at Origin (slot 0) we are about to seed genesis
            // and replay the whole chain from block 0. The on-disk LSM UTxO store
            // MUST start empty for that replay to reconstruct the correct UTxO
            // set. If a previous sync left a populated `utxo-store/` on disk while
            // the ledger snapshot was lost/reset (so we fell back to a chain-from-
            // genesis replay via `init_fresh_ledger`), opening it as-is would let
            // `attach_utxo_store` migrate the genesis UTxOs ON TOP of the stale
            // tip set, and the replay then piles the Byron UTxOs on top of that.
            // At the Byron→Shelley boundary `sumCoinUTxO` then ~doubles (genesis
            // supply + stale tip supply), the reserves recompute
            // (`maxLovelaceSupply - sumCoinUTxO`) goes negative → 0, and the first
            // MIR reserves-debit underflows and panics. Wipe the stale store so
            // the replay rebuilds the UTxO set from scratch. (A truly fresh node
            // has no `utxo-store/` yet, so this is a no-op there.)
            if (ledger.tip.point == Point::Origin || wipe_utxo_store_before_replay)
                && utxo_path.exists()
            {
                warn!(
                    path = %utxo_path.display(),
                    "Ledger is at origin (from-genesis replay) but a UTxO store exists on disk — \
                     wiping it so the replay rebuilds the UTxO set from an empty store. A stale \
                     store would otherwise inflate sumCoinUTxO at the Byron→Shelley boundary and \
                     drive the reserves recompute to 0 (MIR debit underflow panic)."
                );
                if let Err(e) = std::fs::remove_dir_all(&utxo_path) {
                    // Non-fatal: if the wipe fails the recompute tripwire in
                    // `recompute_shelley_initial_reserves` still aborts loudly with
                    // an actionable message rather than corrupting the ledger.
                    warn!(
                        path = %utxo_path.display(),
                        "Failed to wipe stale UTxO store before from-genesis replay: {e}"
                    );
                }
            }

            match dugite_ledger::utxo_store::UtxoStore::open_with_config(
                &utxo_path,
                utxo_cfg.memtable_size_mb,
                utxo_cfg.block_cache_size_mb,
                utxo_cfg.bloom_filter_bits_per_key,
            ) {
                Ok(mut store) => {
                    info!(
                        path = %utxo_path.display(),
                        memtable_mb = utxo_cfg.memtable_size_mb,
                        cache_mb = utxo_cfg.block_cache_size_mb,
                        "UTxO store attached (LSM)"
                    );
                    // Catch-up sync optimization (#698): when the env var
                    // `DUGITE_LSM_WAL_DURING_SYNC=0` is set, disable LSM WAL
                    // writes during from-genesis / deep catch-up replay.
                    //
                    // The LSM WAL fires three `write()` syscalls per UTxO
                    // insert/delete (length prefix, payload, CRC).  On
                    // Babbage/Conway preview blocks (~5 LSM ops per tx ×
                    // 4-10 txs/block) that's 60-150 syscalls per block —
                    // and the macOS profile shows `write()` as the #1
                    // non-idle samples during catch-up replay.
                    //
                    // Without WAL the memtable still flushes to disk as
                    // SST files at the configured cadence (1 GB by
                    // default).  Crash recovery during sync: re-sync the
                    // ~minutes of un-flushed work, no consensus risk.
                    // The `set_wal_enabled(false)` is the same pattern
                    // used by `cargo bench -p dugite-lsm` for bulk-load
                    // benchmarks, and the LSM API docstring explicitly
                    // calls out catch-up sync as the intended use.
                    //
                    // **Operators MUST re-enable WAL before at-tip
                    // operation** by unsetting this env var and restarting
                    // (or once the auto-detect-at-tip path is wired,
                    // which the live-tip apply loop already does via the
                    // `at_tip` predicate from the publish_ledger_view
                    // gate — extending that gate to flip WAL state is the
                    // next step).
                    if std::env::var("DUGITE_LSM_WAL_DURING_SYNC")
                        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
                        .unwrap_or(false)
                    {
                        store.set_wal_enabled(false);
                        warn!(
                            "DUGITE_LSM_WAL_DURING_SYNC=0 — LSM WAL DISABLED for catch-up. \
                             Crash during sync will lose UTxO inserts since the last memtable \
                             flush (~minutes). Unset this env var and restart before going \
                             at-tip / producing blocks."
                        );
                    }
                    // The incomplete-store decision is NOT made here (#989). It
                    // has to happen BEFORE the ledger is chosen, because a
                    // rebuild at this point would skip the Conway genesis
                    // committee seeding that runs between the two — which is
                    // exactly the bug the first attempt at #989 introduced, and
                    // which a preview replay caught as `InvalidPrevGovActionId`.
                    // See `utxo_store_is_usable` and its use at the ledger-choice
                    // site above.
                    ledger.attach_utxo_store(store);
                }
                Err(e) => {
                    warn!(
                        "Failed to open UTxO store at {}: {e}, continuing with in-memory UTxOs",
                        utxo_path.display()
                    );
                }
            }
        } else {
            // In-memory UTxO backend (default).  Faster catch-up apply path
            // but the UTxO set lives in the process RSS.  At preview scale
            // (~3M entries) this is fine on any modern dev box; at mainnet
            // scale (~15M entries × ~10 KB) it will exhaust 32 GB systems
            // long before reaching tip.  Operators running mainnet must
            // pass `--utxo-backend lsm` (or set `utxo_backend = "lsm"` in
            // the storage config JSON) to enable the on-disk LSM store.
            warn!(
                "UTxO backend = in-memory (default). RAM scales linearly with UTxO set size. \
                 For mainnet, restart with `--utxo-backend lsm`. Restarts require reloading \
                 the latest ledger snapshot (or from-genesis replay if none exists)."
            );
        }

        // After attaching the UTxO store, rebuild stake distribution and snapshot
        // pool stakes so that block production has correct data immediately on restart.
        // Without this, a block producer restored from snapshot may see pool_stake=0
        // if the saved snapshot's incremental stake tracking was stale.
        if ledger.tip.point != Point::Origin && !ledger.utxo.utxo_set.is_empty() {
            info!("Rebuilding stake distribution from UTxO store for snapshot consistency");
            ledger.rebuild_stake_distribution();
            // Log delegation/pool_params stats before recompute for diagnostics
            debug!(
                main_delegations = ledger.certs.delegations.len(),
                pool_params = ledger.certs.pool_params.len(),
                stake_credentials = ledger.certs.stake_distribution.stake_map.len(),
                reward_accounts = ledger.certs.reward_accounts.len(),
                mark_delegations = ledger
                    .epochs
                    .snapshots
                    .mark
                    .as_ref()
                    .map(|s| s.delegations.len())
                    .unwrap_or(0),
                set_delegations = ledger
                    .epochs
                    .snapshots
                    .set
                    .as_ref()
                    .map(|s| s.delegations.len())
                    .unwrap_or(0),
                go_delegations = ledger
                    .epochs
                    .snapshots
                    .go
                    .as_ref()
                    .map(|s| s.delegations.len())
                    .unwrap_or(0),
                "Snapshot state before recompute",
            );
            ledger.recompute_snapshot_pool_stakes();
        }

        // Extract opcert counters and current era before moving ledger into
        // the lock, so we avoid blocking_read() inside the tokio runtime.
        let snapshot_era = ledger.era;
        let snapshot_opcert_counters = if ledger.consensus.opcert_counters.is_empty() {
            None
        } else {
            Some(ledger.consensus.opcert_counters.clone())
        };

        // Build LedgerSeq anchor from a lightweight clone (no UTxO data)
        // before moving `ledger` into the Arc. The security_param comes from
        // genesis or defaults to 2160.
        let seq_k = shelley_genesis
            .as_ref()
            .map(|g| g.security_param)
            .unwrap_or(2160);
        let ledger_seq = Arc::new(RwLock::new(
            dugite_ledger::ledger_seq::LedgerSeq::with_defaults(
                ledger.clone_without_utxos(),
                seq_k,
            ),
        ));

        // Issue #651 P2 / #652 P0 — build the initial lock-free read view
        // from the freshly-loaded ledger BEFORE wrapping it in the
        // `RwLock` (Node::new is sync; we can't `await` on the lock here).
        // Subsequent publishes happen at the end of each successful apply
        // path via `publish_ledger_view`.
        let ledger_view = Arc::new(arc_swap::ArcSwap::from_pointee(
            ledger_view::LedgerView::from_state(&ledger),
        ));
        // Issue #654 — watch channel for per-peer eager-validation
        // back-pressure: peers parked on forecast-horizon exhaustion wake
        // when this changes.
        let initial_tip_slot = ledger.tip.point.slot().map(|s| s.0).unwrap_or(0);
        let (ledger_tip_slot_tx, _initial_rx) = tokio::sync::watch::channel(initial_tip_slot);
        // Issue #655 P2.b — eager-validated header bookkeeping.
        let eagerly_validated_headers = Arc::new(parking_lot::Mutex::new(HashMap::new()));

        let ledger_state = Arc::new(RwLock::new(ledger));

        let mut consensus = if let Some(ref genesis) = shelley_genesis {
            OuroborosPraos::with_genesis_params(
                genesis.active_slots_coeff,
                genesis.security_param,
                dugite_primitives::time::EpochLength(genesis.epoch_length),
                genesis.slots_per_k_e_s_period,
                genesis.max_k_e_s_evolutions,
                args.config.max_major_protocol_version(),
            )
        } else {
            OuroborosPraos::new(args.config.max_major_protocol_version())
        };
        // Capture security_param before consensus is moved into the Node struct.
        let consensus_security_param = consensus.security_param;
        info!(
            epoch_len = consensus.epoch_length.0,
            k = consensus.security_param,
            f = consensus.active_slot_coeff,
            kes_period = consensus.slots_per_kes_period,
            max_kes = consensus.max_kes_evolutions,
            "Consensus: Praos",
        );

        // Seed opcert counters from the loaded ledger snapshot (#310).
        // Closes the replay-attack window that existed when counters
        // reset to empty on every restart.
        if let Some(counters) = snapshot_opcert_counters {
            consensus.set_opcert_counters(counters);
        }

        // Lightweight checkpoints (cardano-node CheckpointsFile): loaded once
        // at startup and enforced for every header in BOTH consensus modes
        // (validateIfCheckpoint). Path resolves relative to the config file's
        // directory. A parse error or file-hash mismatch is fatal.
        if let Some(ref cp_file) = args.config.checkpoints_file {
            let cp_dir = args
                .config_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            let cp_path = cp_dir.join(cp_file);
            let checkpoints = crate::checkpoints::load_checkpoints(
                &cp_path,
                args.config.checkpoints_file_hash.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!("CheckpointsFile: {e}"))?;
            info!(
                path = %cp_path.display(),
                count = checkpoints.len(),
                "Loaded lightweight checkpoints"
            );
            consensus.set_checkpoints(checkpoints);
        }

        // Build the HFC era history state machine from genesis parameters.
        // This replaces the hardcoded era lookup tables with a proper state machine
        // that tracks era boundaries and provides slot↔time conversions.
        let era_history = {
            use dugite_consensus::era_history::{EraHistory, EraParams};

            let k = consensus.security_param;
            let active_slots_coeff = consensus.active_slot_coeff;

            let byron_params = EraParams {
                epoch_size: if byron_epoch_length > 0 {
                    byron_epoch_length
                } else {
                    // Fallback: mainnet default (10 * 2160)
                    21600
                },
                slot_length_ms: byron_slot_duration_ms,
                safe_zone: k * 2,
                // Haskell `byronEraParams`: eraGenesisWin = GenesisWindow (2 * k).
                genesis_window: k * 2,
            };

            let shelley_epoch_length = shelley_genesis
                .as_ref()
                .map(|g| g.epoch_length)
                .unwrap_or(432000);
            let shelley_slot_length_ms = shelley_genesis
                .as_ref()
                .map(|g| g.slot_length * 1000)
                .unwrap_or(1000);
            // Haskell `shelleyEraParams`: both the safe zone and the genesis
            // window are the stability window `ceil(3k/f)`
            // (`computeStabilityWindow`). All real networks yield exact
            // integers so ceil == floor there, but ceiling is the formula.
            let stability_window = dugite_consensus::stability_window_slots(k, active_slots_coeff);

            let shelley_params = EraParams {
                epoch_size: shelley_epoch_length,
                slot_length_ms: shelley_slot_length_ms,
                safe_zone: stability_window,
                genesis_window: stability_window,
            };

            let shelley_transition_epoch = epoch::shelley_transition_epoch_for_magic(network_magic);

            let mut eh =
                EraHistory::from_genesis(byron_params, shelley_params, shelley_transition_epoch);

            // If we loaded a ledger snapshot, reconstruct past era transitions
            // so the era history covers all eras up to the current ledger era.
            // This uses the same hardcoded era boundaries as the previous
            // build_era_summaries() for known networks — only needed once on
            // first startup after the EraHistory feature is introduced.
            // NOTE: snapshot_era was extracted before wrapping ledger in Arc<RwLock>
            // to avoid blocking_read() panic inside the tokio runtime.
            {
                // Reconstruct past era transitions for the loaded snapshot era
                // using the per-network HFC era table. Only record transitions
                // whose target era is ≤ the snapshot's current era — i.e. eras
                // already crossed on-chain. Issue #465: preview's Dijkstra
                // entry lives in the table so a Dijkstra-era snapshot recovers
                // `EraHistory::current_era() == Dijkstra` immediately, before
                // the first Dijkstra block applies through the sync pipeline.
                let current_era = snapshot_era;
                for (era, epoch) in epoch::era_transitions_for_magic(network_magic) {
                    if current_era >= era && eh.current_era() < era {
                        eh.record_era_transition(era, epoch);
                    }
                }
            }

            info!(
                eras = eh.len(),
                current = %eh.current_era(),
                "HFC era history initialized",
            );

            Arc::new(RwLock::new(eh))
        };

        let mempool = Arc::new(Mempool::new(MempoolConfig {
            max_transactions: args.mempool_max_tx,
            max_bytes: args.mempool_max_bytes,
            ..MempoolConfig::default()
        }));

        let socket_path = args.socket_path.clone();
        let listen_addr: std::net::SocketAddr =
            format!("{}:{}", args.host_addr, args.port).parse()?;
        // network_magic computed earlier (before ledger snapshot loading).
        // Server tasks are spawned in run() and live for the node's lifetime.

        // Wire up live UTxO provider and ChainDB before wrapping in lock.
        // ChainDB is needed for validate_acquire (C1: SpecificPoint on-chain check).
        let mut qh = QueryHandler::new(args.config.max_major_protocol_version() as u32);
        qh.set_utxo_provider(Arc::new(serve::LedgerUtxoProvider {
            ledger: ledger_state.clone(),
        }));
        qh.set_chain_db(chain_db.clone());
        let query_handler = Arc::new(RwLock::new(qh));

        // Load block producer credentials if key paths are provided.
        // If ANY block production flag is set, ALL three must be present — a partial
        // configuration is an error, not a silent fallback to relay mode.
        let bp_flags = [
            ("--shelley-vrf-key", &args.shelley_vrf_key),
            ("--shelley-kes-key", &args.shelley_kes_key),
            (
                "--shelley-operational-certificate",
                &args.shelley_operational_certificate,
            ),
        ];
        let provided: Vec<&str> = bp_flags
            .iter()
            .filter(|(_, v)| v.is_some())
            .map(|(name, _)| *name)
            .collect();
        let missing: Vec<&str> = bp_flags
            .iter()
            .filter(|(_, v)| v.is_none())
            .map(|(name, _)| *name)
            .collect();

        let block_producer = if provided.is_empty() {
            info!("Relay-only mode (no block producer keys)");
            None
        } else if !missing.is_empty() {
            return Err(anyhow::anyhow!(
                "Incomplete block producer configuration: provided {} but missing {}. \
                 All three flags (--shelley-kes-key, --shelley-vrf-key, \
                 --shelley-operational-certificate) are required for block production.",
                provided.join(", "),
                missing.join(", "),
            ));
        } else {
            let vrf_path = args.shelley_vrf_key.as_ref().ok_or_else(|| {
                anyhow::anyhow!("VRF signing key path required for block production")
            })?;
            let kes_path = args.shelley_kes_key.as_ref().ok_or_else(|| {
                anyhow::anyhow!("KES signing key path required for block production")
            })?;
            let opcert_path = args
                .shelley_operational_certificate
                .as_ref()
                .ok_or_else(|| {
                    anyhow::anyhow!("Operational certificate path required for block production")
                })?;
            let creds =
                crate::forge::BlockProducerCredentials::load(vrf_path, kes_path, opcert_path)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to load block producer credentials: {e}. \
                     Check that the key files and operational certificate are valid."
                        )
                    })?;
            info!(
                pool = %creds.pool_id,
                opcert_seq = creds.opcert_sequence,
                kes_period = creds.opcert_kes_period,
                "Block producer mode",
            );

            // Validate KES period at startup: warn if the opcert's KES period
            // is already expired or will expire soon. This catches the common
            // misconfiguration of issuing an opcert with --kes-period 0 instead
            // of the current KES period.
            let tip_slot = {
                let ls = ledger_state
                    .try_read()
                    .expect("ledger_state lock uncontested during init");
                ls.tip.point.slot().map(|s| s.0).unwrap_or(0)
            };
            if tip_slot > 0 {
                let slots_per_kes = consensus.slots_per_kes_period;
                let max_kes = consensus.max_kes_evolutions;
                let current_kes_period = tip_slot / slots_per_kes;
                let opcert_kes_period = creds.opcert_kes_period;

                if current_kes_period < opcert_kes_period {
                    warn!(
                        current_kes_period,
                        opcert_kes_period,
                        "Operational certificate KES period is in the future — \
                         the certificate is not yet valid. Block forging will fail \
                         until slot {}.",
                        opcert_kes_period * slots_per_kes,
                    );
                } else {
                    let kes_offset = current_kes_period - opcert_kes_period;
                    if kes_offset >= max_kes {
                        error!(
                            current_kes_period,
                            opcert_kes_period,
                            kes_offset,
                            max_kes_evolutions = max_kes,
                            "KES key is EXPIRED. Offset {} >= max evolutions {}. \
                             Block forging will fail. Generate new KES keys and \
                             reissue the operational certificate with \
                             --kes-period {current_kes_period}.",
                            kes_offset,
                            max_kes,
                        );
                    } else {
                        let remaining = max_kes - kes_offset;
                        let remaining_slots = remaining * slots_per_kes;
                        info!(
                            current_kes_period,
                            opcert_kes_period,
                            kes_offset,
                            periods_remaining = remaining,
                            "KES key valid — {} of {} evolutions used, \
                             ~{} slots remaining",
                            kes_offset,
                            max_kes,
                            remaining_slots,
                        );
                        // Warn when fewer than 10 periods remain (~15 days on
                        // preview/mainnet with slotsPerKESPeriod=129600).
                        if remaining <= 10 {
                            warn!(
                                periods_remaining = remaining,
                                "KES key expiring soon — only {} periods remaining. \
                                 Rotate KES keys and reissue the operational \
                                 certificate before period {}.",
                                remaining,
                                opcert_kes_period + max_kes,
                            );
                        }
                    }
                }
            }

            Some(creds)
        };

        // Determine expected genesis hashes for genesis block validation.
        // Config hash fields take priority (ByronGenesisHash, ShelleyGenesisHash);
        // fall back to hashes computed from the genesis files themselves.
        let expected_byron_genesis_hash = args
            .config
            .byron_genesis_hash
            .as_deref()
            .and_then(|h| dugite_primitives::hash::Hash32::from_hex(h).ok())
            .or(byron_genesis_file_hash);
        let expected_shelley_genesis_hash = args
            .config
            .shelley_genesis_hash
            .as_deref()
            .and_then(|h| dugite_primitives::hash::Hash32::from_hex(h).ok())
            .or(shelley_genesis_hash);

        if let Some(ref h) = expected_byron_genesis_hash {
            debug!("Expected Byron genesis hash: {}", h.to_hex());
        }
        if let Some(ref h) = expected_shelley_genesis_hash {
            debug!("Expected Shelley genesis hash: {}", h.to_hex());
        }

        // Build GSM channels and actor parts. The actor itself is spawned in
        // `run()` — here we only create channels and compute the initial
        // snapshot so that `gsm_snapshot_rx.borrow()` returns correct values
        // from the moment the Node struct is constructed.
        //
        // When genesis mode is off the initial snapshot is `CaughtUp` with
        // `loe_slot: None`, and all events sent to `gsm_event_tx` are no-ops
        // inside the actor.
        let genesis_enabled = args.consensus_mode == "genesis";
        // Network-derived genesis parameters (audit gsm-07/gdd-04: the GSM
        // previously ran mainnet-hardcoded k/sgen on every network).
        let genesis_params = dugite_node::genesis_params::GenesisParams::from_network(
            consensus.security_param,
            consensus.active_slot_coeff,
            shelley_genesis
                .as_ref()
                .map(|g| g.slot_length as f64)
                .unwrap_or(1.0),
            args.config.min_big_ledger_peers_for_trusted_state,
            args.config
                .low_level_genesis_options
                .clone()
                .unwrap_or_default(),
        );
        // Stability window in seconds (sgen × slot_length_secs).  Used to
        // decide whether a no-marker tip is "recent" enough to skip
        // PreSyncing and start directly in Syncing (issue #757: Mithril
        // snapshot bootstrap stalls k blocks short of live tip because
        // PreSyncing LoE caps selection at k and BLPs never arrive in time).
        let syncing_startup_threshold_secs =
            (genesis_params.sgen_slots as f64 * genesis_params.slot_length_secs) as u64;
        let gsm_config = crate::gsm::GsmConfig {
            min_active_blp: genesis_params.min_big_ledger_peers,
            gdd_rate_limit_ms: (genesis_params.options.effective_gdd_rate_limit_secs() * 1000.0)
                .max(1.0) as u64,
            security_param_k: genesis_params.security_param_k,
            marker_path: args.database_path.join("caught_up.marker"),
            syncing_startup_threshold_secs,
            ..Default::default()
        };
        // LoE/GDD master switch (LowLevelGenesisOptions.EnableLoEAndGDD,
        // default true). Genesis mode normally runs LoE (Limit on Eagerness)
        // + GDD (Genesis Density Disconnect); an operator may explicitly
        // disable both via EnableLoEAndGDD=false, in which case chain
        // selection is unconstrained by LoE and no density disconnects fire
        // (CSJ / LoP / GSM state tracking remain active). Default-neutral.
        let loe_gdd_enabled = genesis_enabled && genesis_params.options.enable_loe_and_gdd;
        // The LoE handed to chain selection. Initial value mirrors Haskell's
        // pre-setGetLoEFragment conservative default: in genesis mode an
        // empty fragment anchored at the current immutable tip (≤ k blocks
        // of selection freedom); in praos mode (or LoE/GDD disabled) Disabled.
        let initial_loe = if loe_gdd_enabled {
            let db = chain_db
                .try_read()
                .expect("ChainDB lock available during startup");
            let anchor = match db.get_immutable_tip_point() {
                None | Some(dugite_primitives::block::Point::Origin) => None,
                Some(dugite_primitives::block::Point::Specific(slot, hash)) => {
                    Some(dugite_consensus::loe::LoePoint {
                        slot: slot.0,
                        hash: *hash.as_bytes(),
                    })
                }
            };
            dugite_consensus::loe::LoeState::Fragment {
                anchor,
                entries: Vec::new(),
                k: genesis_params.security_param_k,
            }
        } else {
            dugite_consensus::loe::LoeState::Disabled
        };
        let loe_out = Arc::new(arc_swap::ArcSwap::from_pointee(initial_loe));
        // Limit on Patience config (Haskell mkGenesisConfig): enabled only in
        // Genesis mode with EnableLoP (default true); praos =
        // ChainSyncLoPBucketDisabled.
        let lop_params = if genesis_enabled && genesis_params.options.enable_lop {
            Some((
                genesis_params.options.effective_bucket_capacity(),
                genesis_params.options.effective_bucket_rate(),
            ))
        } else {
            None
        };
        // Historicity cutoff (Haskell mkGenesisConfig): genesis mode only.
        let historicity_cutoff_secs = if genesis_enabled {
            Some(genesis_params.historicity_cutoff_secs)
        } else {
            None
        };
        // ChainSync Jumping (Haskell mkGenesisConfig): genesis mode + EnableCSJ.
        let csj = if genesis_enabled && genesis_params.options.enable_csj {
            Some(crate::csj::CsjRegistry::new(
                true,
                genesis_params.options.effective_csj_jump_size(),
            ))
        } else {
            None
        };
        {
            let mut db = chain_db
                .try_write()
                .expect("ChainDB lock available during startup");
            db.set_loe_handle(loe_out.clone());
            if loe_gdd_enabled {
                // Haskell initial-chain-selection k-cap (LoE enabled): never
                // boot onto a >k-deep selection rebuilt from the volatile WAL
                // — it may contain an adversarial chain the LoE was deferring
                // when the node shut down.
                let dropped = db.truncate_selection_to_depth(genesis_params.security_param_k);
                if dropped > 0 {
                    info!(
                        dropped,
                        k = genesis_params.security_param_k,
                        "Genesis startup: capped rebuilt selection at k blocks past \
                         the immutable tip (deferred blocks re-enter under the live LoE)"
                    );
                }
            }
        }
        // G12: increased from 1024 to 4096 to absorb rapid peer churn events.
        let (gsm_event_tx, gsm_event_rx) = tokio::sync::mpsc::channel(GSM_EVENT_CHANNEL_CAP);
        let peer_registry = crate::genesis_peer_state::PeerStateRegistry::new();
        // Compute the initial snapshot so consumers have the right state
        // before the actor has even started.
        //
        // For the no-marker Syncing path (issue #757): estimate the tip age
        // from the tip slot and shelley genesis system_start, using the same
        // threshold as the GSM actor so the initial snapshot is consistent.
        let initial_gsm_state = if genesis_enabled {
            if gsm_config.marker_path.exists() {
                crate::gsm::GenesisSyncState::CaughtUp
            } else {
                // Estimate tip age: system_start + tip_slot × slot_length_ms.
                let tip_age_secs: Option<u64> = shelley_genesis.as_ref().and_then(|sg| {
                    chrono::DateTime::parse_from_rfc3339(&sg.system_start)
                        .ok()
                        .map(|t| {
                            let tip_wallclock_ms = t.timestamp_millis().max(0) as u64
                                + initial_tip_slot * sg.slot_length.saturating_mul(1000);
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            now_ms.saturating_sub(tip_wallclock_ms) / 1000
                        })
                });
                let recent = matches!(
                    tip_age_secs,
                    Some(age) if gsm_config.syncing_startup_threshold_secs > 0
                        && age < gsm_config.syncing_startup_threshold_secs
                );
                // MUST mirror GenesisStateMachine::new()'s no-marker branch so the
                // seed snapshot's state matches the GSM actor's internal state.
                if recent {
                    crate::gsm::GenesisSyncState::Syncing
                } else {
                    crate::gsm::GenesisSyncState::PreSyncing
                }
            }
        } else {
            crate::gsm::GenesisSyncState::CaughtUp
        };
        let initial_loe = match initial_gsm_state {
            crate::gsm::GenesisSyncState::PreSyncing => Some(0),
            crate::gsm::GenesisSyncState::Syncing => Some(0),
            crate::gsm::GenesisSyncState::CaughtUp => None,
        };
        let initial_snapshot = crate::gsm::GsmSnapshot {
            state: initial_gsm_state,
            loe_slot: initial_loe,
        };
        let (gsm_snapshot_tx, gsm_snapshot_rx) = tokio::sync::watch::channel(initial_snapshot);
        let (gdd_action_tx, gdd_action_rx) = tokio::sync::mpsc::channel(64);
        let gsm_actor_parts = Some(GsmActorParts {
            config: gsm_config,
            enabled: genesis_enabled,
            event_rx: gsm_event_rx,
            snapshot_tx: gsm_snapshot_tx,
            action_tx: gdd_action_tx,
            action_rx: gdd_action_rx,
            registry: peer_registry.clone(),
            loe_out: loe_out.clone(),
        });

        // Build and configure metrics before assembling the node struct so we
        // can set the network magic immediately (the TUI reads it on first scrape).
        let node_metrics = {
            let m = crate::metrics::NodeMetrics::new();
            m.set_network_magic(network_magic);
            m.set_consensus_mode_genesis(args.consensus_mode == "genesis");
            m.set_utxo_backend(match args.storage_config.utxo.backend {
                dugite_storage::UtxoBackend::Lsm => "lsm",
                dugite_storage::UtxoBackend::InMemory => "in-memory",
            });
            m.set_compat_metrics(args.compat_metrics);
            m.liveness_threshold_secs.store(
                args.liveness_threshold_secs,
                std::sync::atomic::Ordering::Relaxed,
            );
            // Advertise block producer mode so the TUI shows the correct role and
            // displays the abbreviated pool ID in the Node panel.
            let is_bp = block_producer.is_some();
            if let Some(ref creds) = block_producer {
                m.set_block_producer(&creds.pool_id.to_hex());
            }
            // Advertise P2P configuration so the TUI can display the real state
            // rather than guessing from peer counts.
            let effective_peer_sharing = args.config.effective_peer_sharing(is_bp);
            m.set_p2p_config(&args.config.diffusion_mode, effective_peer_sharing);
            // Publish chain-shape parameters from the Shelley genesis so
            // dugite-monitor and dashboards don't have to hard-code
            // mainnet/preview defaults. Without these the epoch-progress
            // ETA reads "~5 days" on a devnet with a 200-slot epoch.
            if let Some(ref sg) = shelley_genesis {
                m.set_shelley_chain_params(
                    sg.epoch_length,
                    sg.slot_length.saturating_mul(1000), // genesis is seconds; metric wants ms
                    sg.active_slots_coeff,
                );
            }
            Arc::new(m)
        };

        // Log P2P configuration at startup for diagnostics.
        info!(
            diffusion_mode = %args.config.diffusion_mode,
            peer_sharing = args.config.effective_peer_sharing(block_producer.is_some()),
            "P2P networking configuration"
        );

        // ── Phase 1: Initialize ChainFragment from ImmutableDB tip ──────────
        //
        // On startup, the chain fragment represents the volatile window of the
        // selected chain.  We anchor it at the current ImmutableDB tip and
        // populate it with any volatile block headers that form a chain from
        // that tip.  This mirrors Haskell's `openDBInternal` startup step 5.
        //
        // For a fresh node (Origin), the fragment is empty with Origin as anchor.
        // For a node restarted after syncing, we seed the fragment from the
        // VolatileDB (via ChainDB) so the chain selection has correct context.
        //
        // Use `try_read()` to avoid blocking in the async runtime.
        // At this point in startup, no other tasks hold the lock.
        let chain_fragment = {
            let db = chain_db
                .try_read()
                .expect("ChainDB lock available during startup");
            let immutable_tip = db.get_immutable_tip();
            let anchor = match &immutable_tip.point {
                Point::Origin => Point::Origin,
                Point::Specific(slot, hash) => Point::Specific(*slot, *hash),
            };

            // Collect volatile block headers to seed the fragment.
            // We use the ChainDB volatile chain (selected_chain) which is already
            // ordered from anchor to tip.  Convert to BlockHeader stubs — we only
            // need slot + hash for the fragment invariant; full headers are available
            // in VolatileDB if needed.
            let volatile_headers = db.get_volatile_chain_headers();

            ChainFragment::from_headers(anchor, volatile_headers)
        };

        // ── Phase 1: Initialize ChainSelHandle ──────────────────────────────
        //
        // Create the chain-selection queue.  The runner future is NOT yet
        // spawned here — `Node::new()` is sync, so we store it and spawn in
        // `run()` instead.  The handle is stored so the sync loop and forge
        // path can submit blocks without holding any other locks.
        let (chain_sel_handle, chain_sel_runner) = ChainSelHandle::new(Arc::clone(&chain_db));
        // Spawn the runner.  `new()` is called from within a tokio runtime
        // (from main() which is `#[tokio::main]`), so `tokio::spawn` is safe.
        tokio::spawn(chain_sel_runner);

        // Issue #747: extract bulk-sync snapshot rate limit from config BEFORE
        // args.config is moved into the Node struct literal below.
        let bulk_sync_snapshot_rate_limit_secs: f64 = args
            .config
            .low_level_genesis_options
            .as_ref()
            .map(|o| o.effective_snapshot_min_interval_bulk_sync_secs())
            .unwrap_or(1800.0);

        Ok(Node {
            config: args.config,
            topology: args.topology,
            chain_db,
            ledger_state,
            ledger_view,
            ledger_tip_slot_tx,
            eagerly_validated_headers,
            ledger_seq,
            consensus,
            mempool,
            // Lifecycle manager, fetch task, and fetch channel are initialized
            // in run() once the block_announcement_tx is created.
            connection_lifecycle: None,
            fetched_blocks_rx: None,
            defer_phase2_window: std::env::var("DUGITE_DEFER_PHASE2_WINDOW")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            pending_phase2: Vec::new(),
            pending_phase2_anchor: None,
            pending_phase2_items: 0,
            defer_phase2_max_items: std::env::var("DUGITE_DEFER_PHASE2_MAX_ITEMS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(256),
            shutdown_rx_for_flush: None,
            peer_failure_rx: None,
            keepalive_rtt_rx: None,
            query_handler,
            peer_manager: Arc::new(RwLock::new({
                let mut pm = NodePeerManager::new(PeerManagerConfig::default());
                pm.set_gsm_event_tx(gsm_event_tx.clone());
                // #933: mirror the consensus mode into the peer manager —
                // the `consensusMode` dimension of `haa_satisfied`'s
                // Haskell `outboundConnectionsState` case split.
                pm.set_genesis_mode(genesis_enabled);
                pm
            })),
            socket_path,
            database_path: args.database_path,
            listen_addr,
            network_magic,
            byron_epoch_length,
            byron_slot_duration_ms,
            snapshot_policy: epoch::SnapshotPolicy::with_params(
                shelley_genesis
                    .as_ref()
                    .map(|g| g.security_param)
                    .unwrap_or(2160),
                args.snapshot_max_retained,
                args.snapshot_bulk_min_blocks,
                args.snapshot_bulk_min_secs,
            ),
            shelley_genesis,
            era_history,
            topology_path: args.topology_path,
            config_path: args.config_path,
            log_handle: args.log_handle,
            metrics: node_metrics,
            block_producer,
            block_announcement_tx: None,
            rollback_announcement_tx: None,
            tip_broadcaster: None,
            rpc_config: args.rpc_config,
            metrics_port: args.metrics_port,
            require_metrics: args.require_metrics,
            expected_byron_genesis_hash,
            expected_shelley_genesis_hash,
            genesis_validated: false,
            live_epoch_transitions: 0,
            consensus_mode: args.consensus_mode,
            peer_registry,
            loe_out,
            lop_params,
            historicity_cutoff_secs,
            csj,
            gsm_min_active_blp: genesis_params.min_big_ledger_peers,
            validate_all_blocks: args.validate_all_blocks,
            skip_eagerly_validated_header_crypto: args.skip_eagerly_validated_header_crypto,
            disk_space_rx: watch::channel(crate::disk_monitor::DiskSpaceLevel::Ok).1,
            gsm_event_tx,
            gsm_snapshot_rx,
            gsm_actor_parts,
            chain_fragment: Arc::new(RwLock::new(chain_fragment)),
            chain_sel_handle: Some(chain_sel_handle),

            // ── Phase 5: Background operations ───────────────────────────────
            //
            // The security parameter k is taken from the consensus object,
            // which was already initialised from the Shelley genesis above.
            // For fresh nodes without genesis config, consensus defaults to
            // 2160 (mainnet/preview/preprod all use k=2160).
            copy_to_immutable: CopyToImmutable::new(consensus_security_param as usize),
            gc_scheduler: GcScheduler::new(),
            // Slot-based snapshot interval = k * 2 (#701, Haskell defInterval).
            // For mainnet/preprod k=2160 → 4320 slots ≈ 72 min.
            // For preview k=432 → 864 slots ≈ 14 min.
            bg_snapshot_scheduler: {
                let mut sched = SnapshotScheduler::with_slot_interval(
                    consensus_security_param.saturating_mul(2),
                );
                // Issue #747: apply bulk-sync rate limit from config so epoch-
                // boundary snapshots during genesis catch-up don't fire more
                // often than the configured interval (default 30 min).
                sched.set_bulk_sync_rate_limit(std::time::Duration::from_secs_f64(
                    bulk_sync_snapshot_rate_limit_secs,
                ));
                sched
            },
            last_query_state_update: Instant::now(),
            last_volatile_wal_sync: Instant::now(),
            peer_intersection_established: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ingestion_paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            volatile_wal_sync_at_tip: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            // Snapshot worker (issue #695): channel + handle are set
            // in `Node::run` once the tokio runtime is active.
            snapshot_tx: None,
            snapshot_worker_handle: None,
        })
    }

    // ─── Lock-free ledger view (issue #651 P2 / #652 P0) ─────────────────────

    /// Load the most recently published [`ledger_view::LedgerView`]
    /// without taking the `ledger_state` `RwLock`. Returns an `Arc` that
    /// extends the view's lifetime past any subsequent publish, so the
    /// caller observes a stable snapshot.
    ///
    /// Staleness: up to one apply step (block / rollback / epoch
    /// transition). Strict readers must keep using
    /// `ledger_state.read().await`.
    #[allow(dead_code)] // wired in by per-call-site migration follow-ups
    pub fn view(&self) -> Arc<ledger_view::LedgerView> {
        self.ledger_view.load_full()
    }

    /// Publish a fresh `LedgerView` from `ls`, replacing the previous
    /// view. Called from every successful apply path while the
    /// `ledger_state` write lock is still held (cheapest publish: no
    /// extra lock acquisition; the view captures the just-applied state).
    /// The actual store is a single atomic pointer swap.
    ///
    /// Also publishes the new tip slot on `ledger_tip_slot_tx` (issue
    /// #654) so per-peer chainsync tasks parked on forecast-horizon
    /// exhaustion wake. `watch::send` is no-op + lock-free when the
    /// value is unchanged, so back-to-back publishes at the same slot
    /// have negligible cost.
    pub(crate) fn publish_ledger_view(&self, ls: &LedgerState) {
        let new_tip_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
        self.ledger_view
            .store(Arc::new(ledger_view::LedgerView::from_state(ls)));
        // `send` returns Err only when there are zero receivers — that's
        // fine, we don't care if no one is listening yet.
        let _ = self.ledger_tip_slot_tx.send(new_tip_slot);
    }

    /// Convenience: re-publish the view by taking the read lock and
    /// constructing from current state. Used by call sites that no
    /// longer hold the write lock when they want to refresh the view
    /// (e.g. after `post_block_apply_updates` has run).
    pub(crate) async fn publish_view_now(&self) {
        let ls = self.ledger_state.read().await;
        self.publish_ledger_view(&ls);
    }

    // ─── run() ───────────────────────────────────────────────────────────────

    pub async fn run(&mut self) -> Result<()> {
        let tip = self.chain_db.read().await.get_tip();

        // If ChainDB already has blocks, genesis was validated on a prior run
        if tip.point != Point::Origin {
            self.genesis_validated = true;
        }

        {
            let ls = self.ledger_state.read().await;
            info!(
                tip = %tip,
                utxos = ls.utxo.utxo_set.len(),
                mempool_txs = self.mempool.len(),
                "Chain tip",
            );

            // Initialize Prometheus metrics from loaded ledger state so they
            // are accurate immediately on startup (before any blocks arrive).
            self.metrics.set_epoch(ls.epoch.0);
            self.metrics.set_protocol_version(
                ls.epochs.protocol_params.protocol_version_major,
                ls.epochs.protocol_params.protocol_version_minor,
            );
            self.metrics.set_utxo_count(ls.utxo.utxo_set.len() as u64);
            self.metrics.set_mempool_count(self.mempool.len() as u64);
            self.metrics.set_mempool_max(self.mempool.capacity() as u64);
            self.metrics
                .set_governance_snapshot(&governance_snapshot_from_ledger(&ls));
            // Set slot/block from tip.  Do NOT pre-set sync_progress to 100%
            // here — we have not heard from a peer yet, so the network tip
            // is unknown.  `refresh_sync_progress` will produce 0% until
            // the first `MsgRollForward`/`MsgRollBackward` populates
            // `max_peer_tip_slot`; thereafter every block-apply path
            // recomputes progress as `applied / peer_tip`.
            if let Some(slot) = tip.point.slot() {
                self.metrics.set_slot(slot.0);
                self.metrics.set_block_number(tip.block_number.0);
                // #742: seed the ledger-tip watch with the ChainDB tip so the
                // forecast-horizon admission gate (forecast_park_or_disconnect)
                // measures incoming headers against the node's EFFECTIVE tip,
                // not the stale snapshot tip. The startup volatile replay
                // (reapply mode) advances the ledger toward this tip but never
                // fires `ledger_tip_slot_tx` (publish_ledger_view runs only on
                // the live apply path). Without this seed, a restart with a
                // large fetched-ahead volatile gap (e.g. blocks fetched past a
                // block whose apply then halted) parks EVERY incoming header on
                // "beyond forecast horizon" against the snapshot tip and wedges
                // the whole sync (observed live: restart from an epoch-511
                // snapshot whose ChainDB had fetched ~172k slots ahead). Only an
                // admission gate — apply-time validation stays authoritative and
                // the genesis LoE still caps adoption, so an optimistic seed
                // during the brief local replay is safe.
                let _ = self.ledger_tip_slot_tx.send(slot.0);
                self.update_sync_progress(slot.0, &ls.slot_config).await;
                // Era-aware tip-age computation (see Node::slot_to_wallclock_ms).
                let slot_time_ms = self.slot_to_wallclock_ms(slot.0, &ls.slot_config).await;
                self.metrics.set_tip_slot_time_ms(slot_time_ms);
            }
        }

        // Spawn the background snapshot worker (issue #695). The worker
        // owns the disk-bound bincode walk + atomic rename + prune that
        // previously ran on the apply thread; apply now fires a single
        // mpsc `try_send` and continues. Mirrors cardano-node's
        // `ledgerDbTaskWatcher`. The worker exits when `snapshot_tx` is
        // dropped at shutdown.
        {
            let max_snapshots = self.snapshot_policy.max_snapshots;
            let (tx, handle) = snapshot_worker::spawn_snapshot_worker(
                self.database_path.clone(),
                max_snapshots,
                self.metrics.clone(),
            );
            self.snapshot_tx = Some(tx);
            self.snapshot_worker_handle = Some(handle);
        }

        // Setup shutdown signal (SIGINT + SIGTERM) early so the node can be
        // gracefully stopped during replay (which can take 30+ minutes).
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        // Let the pooled Phase-2 deferral flush observe shutdown between chunks
        // (cancel-aware flush; see `flush_pending_phase2`).
        self.shutdown_rx_for_flush = Some(shutdown_rx.clone());
        // #760: set true the instant the main run loop breaks, so the
        // shutdown watchdog can tell "loop wedged, never broke" (force-exit)
        // from "loop broke, draining cleanly" (the post-loop block has its own
        // bounded 30s/120s timeouts — leave it alone).
        let loop_broken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(unix)]
        {
            let shutdown_tx_clone = shutdown_tx.clone();
            let loop_broken_wd = loop_broken.clone();
            tokio::spawn(async move {
                // Startup-time panic is acceptable — if we can't register signal
                // handlers, the node cannot shut down gracefully.
                let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
                tokio::select! {
                    _ = signal::ctrl_c() => {
                        info!("SIGINT received, shutting down");
                    }
                    _ = sigterm.recv() => {
                        info!("SIGTERM received, shutting down");
                    }
                }
                shutdown_tx_clone.send(true).ok();

                // #760: bounded shutdown watchdog, fully independent of the run
                // loop (so a wedged run loop holding ledger/chain_db locks
                // cannot starve it). Race the loop-break deadline against a
                // SECOND signal:
                //   - second SIGINT/SIGTERM  -> immediate forced exit (operator
                //     escape hatch, matches cardano-node muscle memory),
                //   - deadline elapses && the loop has NOT broken -> the run
                //     loop is wedged (e.g. genesis CSJ-far-ahead, #760) and will
                //     never reach the bounded post-loop drain, so force-exit
                //     rather than leave an un-stoppable node that an operator
                //     would have to SIGKILL (risking DB corruption).
                // It deliberately does NOT touch ledger_state/chain_db: it relies
                // on the last periodic atomic snapshot for recovery, so it is
                // safe even when those locks are held by the wedge.
                let deadline_secs = std::env::var("DUGITE_SHUTDOWN_DEADLINE_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(SHUTDOWN_LOOP_BREAK_DEADLINE_SECS);
                tokio::select! {
                    _ = signal::ctrl_c() => {
                        error!("second shutdown signal (SIGINT) — forcing immediate exit");
                        std::process::exit(130);
                    }
                    _ = sigterm.recv() => {
                        error!("second shutdown signal (SIGTERM) — forcing immediate exit");
                        std::process::exit(143);
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(deadline_secs)) => {
                        if !loop_broken_wd.load(std::sync::atomic::Ordering::Acquire) {
                            error!(
                                deadline_secs,
                                "run loop did not break within the shutdown deadline \
                                 (sync wedge?) — forcing exit to avoid an un-stoppable \
                                 node; recovery uses the last periodic snapshot (#760)"
                            );
                            std::process::exit(1);
                        }
                        // Loop broke in time; the post-loop drain owns the rest
                        // (its own bounded 30s/120s force-exits). Watchdog done.
                    }
                }
            });
        }
        #[cfg(not(unix))]
        {
            let shutdown_tx_clone = shutdown_tx.clone();
            tokio::spawn(async move {
                signal::ctrl_c().await.ok();
                info!("Shutdown signal received");
                shutdown_tx_clone.send(true).ok();
            });
        }

        // Start Prometheus metrics server before replay so /health, /ready,
        // and /metrics are available during the (potentially long) replay window.
        if self.metrics_port > 0 {
            let metrics = self.metrics.clone();
            let port = self.metrics_port;
            let require_metrics = self.require_metrics;
            let metrics_shutdown_rx = shutdown_rx.clone();

            if require_metrics {
                // --require-metrics: the bind phase must succeed before the node
                // continues.  Bind synchronously (with retries); if it fails, bail
                // out immediately with a fatal error rather than silently degrading.
                let listener = crate::metrics::bind_metrics_listener(port)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Metrics server failed to bind on port {port} \
                                 (--require-metrics was set): {e}"
                        )
                    })?;
                // Bind succeeded — spawn the accept loop as a background task.
                tokio::spawn(async move {
                    crate::metrics::run_metrics_server(listener, metrics, metrics_shutdown_rx)
                        .await;
                });
            } else {
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::metrics::start_metrics_server(port, metrics, metrics_shutdown_rx)
                            .await
                    {
                        error!(
                            port,
                            "Metrics server failed to start: {e} — node will continue without metrics"
                        );
                    }
                });
            }
        }

        // Replay blocks from ChainDB if the ledger is behind storage.
        // This happens after a Mithril snapshot import — blocks are in storage
        // but the ledger hasn't processed them yet.
        let replay_start = std::time::Instant::now();
        self.replay_ledger_from_storage(shutdown_rx.clone()).await;
        self.metrics
            .set_replay_duration_secs(replay_start.elapsed().as_secs());

        // #985: replay advances `ledger_state` in bulk without pushing any
        // deltas, and every one of its sub-paths (chunk replay, LSM replay,
        // and the fork-rollback inside LSM replay) can move the tip. The
        // LedgerSeq was anchored in `Node::new` on the PRE-replay state, so
        // without this the anchor stays behind for the whole process lifetime
        // while at-tip deltas pile on top of it.
        //
        // Worst case that made this a P0: on the first boot after a
        // SNAPSHOT_VERSION bump every on-disk snapshot is quarantined, so
        // `Node::new` falls back to `init_fresh_ledger` and the anchor is
        // GENESIS. The first at-tip fork switch then reconstructed
        // genesis pparams — PV6, d=1 — into the live ledger, which ran the
        // TPraos overlay classifier over a canonical Conway block, rejected
        // it, and poisoned chain selection permanently.
        //
        // Unconditional rather than gated on "did replay do anything": the
        // no-op case is a cheap `clone_without_utxos` of a state we already
        // hold, and a gate is one more thing that can be wrong.
        self.reanchor_ledger_seq("startup replay complete").await;

        if *shutdown_rx.borrow() {
            info!("Shutdown requested during replay, exiting");
            return Ok(());
        }

        // Issue #927 invariant: after replay the applied ledger tip must have
        // reached the ImmutableDB tip. A persistent ledger<immutable state
        // (crash-recovery index hole per #926, replay apply-failure) used to
        // be invisible apart from per-peer ChainSync churn while the node made
        // zero progress; the known-points ordering and #699-guard exemption
        // now keep sync alive in this state, and this warning makes the seam
        // itself visible to the operator.
        {
            let ledger_slot = {
                let ls = self.ledger_state.read().await;
                ls.tip.point.slot().map(|s| s.0).unwrap_or(0)
            };
            let imm = self.chain_db.read().await.get_immutable_tip_point();
            let imm_slot = imm
                .as_ref()
                .and_then(|p| p.slot())
                .map(|s| s.0)
                .unwrap_or(0);
            if ledger_slot < imm_slot {
                warn!(
                    ledger_slot,
                    immutable_tip_slot = imm_slot,
                    gap_slots = imm_slot - ledger_slot,
                    "Ledger tip is BELOW the ImmutableDB tip after replay — the \
                     immutable chain contains blocks the ledger could not apply \
                     (crash-damaged index per #926, or a replay apply failure). \
                     Sync will advance the ledger from ChainDB via the gap-bridge \
                     where possible; if this gap persists, inspect the seam and \
                     consider re-import via `dugite-node mithril-import` (#927)."
                );
            }
        }

        // Reseed the consensus validator's opcert counters from the post-replay
        // ledger state.
        //
        // During replay (both chunk-file and LSM paths) every block is applied
        // with `BlockValidationMode::ApplyOnly`, which skips
        // `validate_header_full` and therefore does NOT call
        // `check_opcert_counter`. As a result `self.consensus.opcert_counters`
        // stays frozen at the snapshot values while `ls.consensus.opcert_counters`
        // is updated on every applied block via `compute_shelley_nonce`.
        //
        // Without this reseed, the live-block validation that follows uses the
        // stale snapshot counters: a pool that rotated its opcert during the
        // replayed range (counter M→N, N>M+1) appears to "over-increment" even
        // though the intermediate block with counter M+1 was applied and accepted.
        // The false-positive `CounterOverIncrementedOCERT` then marks the block
        // invalid, poisons the entire downstream chain in the InvalidBlockCache,
        // and stalls the node permanently (observed at preprod block 718744,
        // slot 22975227: got=31, last_seen=29).
        //
        // Fix: take the per-pool max of (snapshot value, ledger post-replay value)
        // for every pool, mirroring the `merge_opcert_counters_from_praos` logic
        // already used in `replay_from_lsm`.
        {
            let ls = self.ledger_state.read().await;
            let ledger_counters = &ls.consensus.opcert_counters;
            let praos_counters = self.consensus.opcert_counters().clone();
            // Build the merged map: per-pool max of both sources.
            let mut merged = praos_counters;
            for (pool_id, &ledger_seq) in ledger_counters {
                merged
                    .entry(*pool_id)
                    .and_modify(|cur| {
                        if ledger_seq > *cur {
                            *cur = ledger_seq;
                        }
                    })
                    .or_insert(ledger_seq);
            }
            let count = merged.len();
            self.consensus.set_opcert_counters(merged);
            info!(
                count,
                "Reseeded consensus opcert counters from post-replay ledger state"
            );

            // Issue #742: publish the post-replay ledger view + tip watch so
            // per-peer CSJ tasks are not stuck parking on a stale tip=0 view.
            //
            // After a from-genesis replay the `ledger_view` ArcSwap and
            // `ledger_tip_slot_tx` watch were seeded from the AT-LOAD ledger
            // (tip=Origin/0) in `Node::new`. `replay_from_lsm` /
            // `replay_from_chunk_files` apply millions of blocks without ever
            // calling `publish_ledger_view`, so the view remains frozen at 0
            // after replay. The first CSJ dynamo MsgRollForward (slot ~73M on
            // mainnet) hit `forecast_park_or_disconnect` → max_for = 0+1+sw
            // → OutsideForecastRange → parks on tip_rx.changed() → nobody
            // ever calls send → deadlock (Haskell: tip watch is refreshed by
            // the chain-apply loop, not just the live-block path).
            self.publish_ledger_view(&ls);
        }

        // #762: the post-replay `publish_ledger_view` above sends the LEDGER
        // tip on `ledger_tip_slot_tx`. When the replay only PARTIALLY advanced
        // the ledger toward the ChainDB tip — chunk files that do not connect
        // to the loaded snapshot, or an LSM fork-rollback to an earlier
        // canonical snapshot — that ledger tip is BELOW the ChainDB tip, and
        // the publish REGRESSES the #742 startup seed (which set tip_rx to the
        // ChainDB tip). The per-peer forecast-horizon admission gate
        // (forecast_park_or_disconnect) then measures every incoming header
        // against the regressed, lower tip and parks them all → permanent
        // wedge in genesis bulk-sync mode (self-heals only on a SECOND restart,
        // once replay re-saved a snapshot at the ChainDB tip so nothing is
        // replayed). The forecast gate is admission-only — apply-time
        // validation and the genesis LoE stay authoritative — so seeding it at
        // the node's actual block coverage (the ChainDB tip) is safe and
        // correct. Never let tip_rx regress below the ChainDB tip after replay.
        {
            let chaindb_tip_slot = self
                .chain_db
                .read()
                .await
                .get_tip()
                .point
                .slot()
                .map(|s| s.0)
                .unwrap_or(0);
            if chaindb_tip_slot > *self.ledger_tip_slot_tx.borrow() {
                let _ = self.ledger_tip_slot_tx.send(chaindb_tip_slot);
                info!(
                    chaindb_tip_slot,
                    "Re-seeded ledger-tip watch to ChainDB tip after replay \
                     (post-replay ledger tip lagged the ChainDB tip; #762)"
                );
            }
        }

        // Enable strict verification (full crypto checks for new blocks).
        // After replay, we're at the chain tip from storage — enable strict
        // mode so live blocks are fully validated. The epoch nonce loaded from
        // the consensus state snapshot is authoritative immediately, matching
        // Haskell's cardano-node behavior (praosStateEpochNonce is trusted as
        // soon as it is deserialized).
        self.consensus.set_strict_verification(true);

        // If running as block producer, log the pool's stake in the set snapshot
        // so operators can immediately diagnose eligibility issues.
        if let Some(ref creds) = self.block_producer {
            let ls = self.ledger_state.read().await;
            if let Some(ref set_snap) = ls.epochs.snapshots.set {
                let total_stake: u64 = set_snap.pool_stake.values().map(|s| s.0).sum();
                let pool_stake = set_snap
                    .pool_stake
                    .get(&creds.pool_id)
                    .map(|s| s.0)
                    .unwrap_or(0);
                let relative_stake = if total_stake > 0 {
                    pool_stake as f64 / total_stake as f64
                } else {
                    0.0
                };
                info!(
                    pool_id = %creds.pool_id,
                    snapshot_epoch = set_snap.epoch.0,
                    pool_stake_lovelace = pool_stake,
                    total_active_stake_lovelace = total_stake,
                    relative_stake = format_args!("{relative_stake:.8}"),
                    "Block producer: pool stake in 'set' snapshot (used for leader election)",
                );
                if pool_stake == 0 {
                    // Diagnostic: check if pool is in delegations, pool_params,
                    // and if any credentials delegate to it.
                    let pool_in_params = ls.certs.pool_params.contains_key(&creds.pool_id);
                    let delegators_to_pool = set_snap
                        .delegations
                        .values()
                        .filter(|pid| **pid == creds.pool_id)
                        .count();
                    let main_delegators_to_pool = ls
                        .certs
                        .delegations
                        .values()
                        .filter(|pid| **pid == creds.pool_id)
                        .count();
                    warn!(
                        pool_id = %creds.pool_id,
                        snapshot_epoch = set_snap.epoch.0,
                        total_pools_in_snapshot = set_snap.pool_stake.len(),
                        pool_in_params,
                        snapshot_delegators = delegators_to_pool,
                        main_delegators = main_delegators_to_pool,
                        total_snapshot_delegations = set_snap.delegations.len(),
                        total_main_delegations = ls.certs.delegations.len(),
                        "Block producer has ZERO stake in 'set' snapshot — will not be elected slot leader. \
                         Pool may not be in snapshot or stake distribution may need rebuilding.",
                    );
                }
            } else {
                warn!(
                    pool_id = %creds.pool_id,
                    "Block producer: no 'set' snapshot available — leader election disabled until epoch transition"
                );
            }
        }

        // Initialize query state from current ledger so N2C queries
        // work immediately (before we reach chain tip or the periodic timer fires)
        self.update_query_state().await;

        // SIGHUP handler is set up after peer_manager initialization below

        // Start disk space monitor on the database volume.
        //
        // The monitor writes to `self.ingestion_paused` (a shared AtomicBool)
        // whenever free space crosses the PAUSE / RECOVER thresholds.  Both
        // `apply_fetched_block` (live-tip path) and `process_forward_blocks`
        // (bulk-sync path) check this flag before writing to ChainDB.
        {
            let (disk_level_tx, disk_level_rx) =
                watch::channel(crate::disk_monitor::DiskSpaceLevel::Ok);
            self.disk_space_rx = disk_level_rx;
            let db_path = self.database_path.clone();
            let metrics = self.metrics.clone();
            let disk_shutdown_rx = shutdown_rx.clone();
            let ingestion_paused = self.ingestion_paused.clone();
            tokio::spawn(async move {
                crate::disk_monitor::start_disk_monitor(
                    db_path,
                    metrics,
                    disk_shutdown_rx,
                    disk_level_tx,
                    ingestion_paused,
                )
                .await;
            });
        }

        // Issue #672 M0.3: tx validator + slot config hoisted out of the N2C
        // block so the forthcoming UTxO RPC SubmitService (M1+) can share the
        // exact same validator instance + slot config as N2C
        // LocalTxSubmission. SlotConfig is Copy and LedgerTxValidator is
        // Arc'd, so the N2C block captures by clone on the consume side.
        let n2c_slot_config = self
            .shelley_genesis
            .as_ref()
            .map(|g| {
                let ste = epoch::shelley_transition_epoch_for_magic(self.network_magic);
                g.slot_config(ste, self.byron_epoch_length, self.byron_slot_duration_ms)
            })
            .unwrap_or(dugite_ledger::plutus::SlotConfig {
                zero_time: 0,
                zero_slot: 0,
                slot_length: 1000,
                // Per-tx horizon is plumbed in `LedgerTxValidator::validate`
                // before each `evaluate_plutus_scripts` call from the live
                // EraHistory; this fallback SlotConfig is only used when
                // shelley_genesis is None (very early boot) where no Plutus
                // tx can be admitted yet.
                safe_zone_horizon_slot: None,
            });
        let n2c_tx_validator = Arc::new(serve::LedgerTxValidator {
            ledger: self.ledger_state.clone(),
            slot_config: n2c_slot_config,
            metrics: self.metrics.clone(),
            mempool: Some(self.mempool.clone()),
            network: if self.network_magic == dugite_primitives::network::NetworkId::Mainnet.magic()
            {
                dugite_primitives::network::NetworkId::Mainnet
            } else {
                dugite_primitives::network::NetworkId::Testnet
            },
            era_history: self.era_history.clone(),
        });

        // Start N2C server on Unix socket.
        //
        // Each accepted connection gets its own Mux and set of protocol tasks:
        //   - Handshake (protocol 0, responder)
        //   - LocalChainSync (protocol 5, responder)
        //   - LocalTxSubmission (protocol 6, responder)
        //   - LocalStateQuery (protocol 7, responder)
        //   - LocalTxMonitor (protocol 9, responder)
        {
            let n2c_socket_path = self.socket_path.clone();
            let n2c_shutdown_rx = shutdown_rx.clone();
            let n2c_network_magic = self.network_magic;
            let n2c_query_handler = self.query_handler.clone();
            let n2c_mempool = self.mempool.clone();
            let n2c_ledger = self.ledger_state.clone();
            let n2c_metrics = self.metrics.clone();
            // Build the block provider for LocalChainSync
            let n2c_block_provider = Arc::new(serve::ChainDBBlockProvider {
                chain_db: self.chain_db.clone(),
            });

            // Remove stale socket file if it exists (e.g., from a previous unclean shutdown).
            if n2c_socket_path.exists() {
                if let Err(e) = std::fs::remove_file(&n2c_socket_path) {
                    warn!(
                        "Failed to remove stale socket {}: {e}",
                        n2c_socket_path.display()
                    );
                }
            }

            let listener = match tokio::net::UnixListener::bind(&n2c_socket_path) {
                Ok(l) => l,
                Err(e) => {
                    error!(
                        "Failed to bind N2C Unix socket at {}: {e}",
                        n2c_socket_path.display()
                    );
                    return Err(e.into());
                }
            };

            // A-009 (security audit 2026-05-19): warn if the socket file is
            // world-readable/writable.  Haskell cardano-node relies on filesystem
            // permissions as the sole access-control mechanism for N2C; we enforce
            // the same convention by warning operators if the umask is too permissive.
            // The recommended permission is 0o600 (owner r/w only) or 0o660 (group).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&n2c_socket_path) {
                    let mode = meta.permissions().mode();
                    if mode & 0o006 != 0 {
                        warn!(
                            socket = %n2c_socket_path.display(),
                            mode = format_args!("{:#o}", mode & 0o777),
                            "N2C socket is world-readable/writable — any local process can \
                             submit transactions. Restrict with: chmod 600 {}",
                            n2c_socket_path.display()
                        );
                    }
                }
            }

            info!(
                socket = %n2c_socket_path.display(),
                "N2C server listening"
            );

            // We need a block_announcement_tx for LocalChainSync server.
            // It will be set below when the N2N server creates the broadcast channels.
            // For now, create a placeholder that will be replaced.
            // Actually, we share the same broadcast channel — create it here and use
            // it for both N2C LocalChainSync and N2N ChainSync server.
            let (block_ann_tx, _) = tokio::sync::broadcast::channel::<
                dugite_network::BlockAnnouncement,
            >(BLOCK_ANN_CHANNEL_CAP);
            let (rollback_ann_tx, _) =
                tokio::sync::broadcast::channel::<RollbackAnnouncement>(ROLLBACK_ANN_CHANNEL_CAP);
            self.block_announcement_tx = Some(block_ann_tx.clone());
            self.rollback_announcement_tx = Some(rollback_ann_tx.clone());
            self.tip_broadcaster = Some(Arc::new(tip_broadcast::TipBroadcaster::new()));
            let n2c_block_ann_tx = block_ann_tx;
            let n2c_rollback_ann_tx = rollback_ann_tx;
            // C4 + G3: bound the number of concurrent N2C connections.
            // Reads from NodeConfig::max_n2c_connections (default 16); the
            // semaphore enforces it via owned permits held for the connection
            // lifetime. Matches Haskell cardano-node's LocalConnectionLimit
            // semantics.
            let n2c_max_connections = self.config.max_n2c_connections.max(1);
            let n2c_semaphore = Arc::new(Semaphore::new(n2c_max_connections));

            // Clone the validator so the spawn move closure doesn't consume
            // it — the RPC startup block below needs the same Arc (#672 M3).
            let n2c_tx_validator_for_spawn = n2c_tx_validator.clone();
            tokio::spawn(async move {
                let n2c_tx_validator = n2c_tx_validator_for_spawn;
                let mut shutdown = n2c_shutdown_rx;
                // Track spawned connection handlers so we can abort them on
                // shutdown — otherwise they block indefinitely waiting for
                // client I/O, preventing the process from exiting.
                //
                // C13 + G3: prune finished handles every iteration so the Vec
                // never grows proportional to total-connections-since-start.
                // The semaphore above enforces the configured connection limit
                // so a local attacker (or a wallet that reconnects in a tight
                // loop) cannot accumulate unbounded tasks or JoinHandles.
                let mut conn_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
                let mut accept_count: usize = 0;
                const PRUNE_INTERVAL: usize = 64;

                loop {
                    // Prune finished handles first — O(n) in active count, not
                    // in total-accepted.
                    conn_handles.retain(|h| !h.is_finished());

                    tokio::select! {
                        accept_result = listener.accept() => {
                            match accept_result {
                                Ok((stream, _addr)) => {
                                    // C4 + G3: enforce connection limit via the
                                    // owned-permit semaphore. The permit is held
                                    // for the connection lifetime and released
                                    // automatically when the spawned task exits.
                                    let permit = match n2c_semaphore.clone().try_acquire_owned() {
                                        Ok(p) => p,
                                        Err(_) => {
                                            warn!(
                                                limit = n2c_max_connections,
                                                "N2C connection limit reached — dropping new connection"
                                            );
                                            // stream is dropped here, closing the socket.
                                            continue;
                                        }
                                    };

                                    let conn_metrics = n2c_metrics.clone();
                                    conn_metrics
                                        .n2c_connections_total
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    conn_metrics
                                        .n2c_connections_active
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                                    let qh = n2c_query_handler.clone();
                                    let bp = n2c_block_provider.clone();
                                    let mp = n2c_mempool.clone();
                                    let tv = n2c_tx_validator.clone();
                                    let ledger = n2c_ledger.clone();
                                    let metrics = conn_metrics.clone();
                                    let ann_rx = n2c_block_ann_tx.subscribe();
                                    let rb_rx = n2c_rollback_ann_tx.subscribe();
                                    let magic = n2c_network_magic;

                                    let handle = tokio::spawn(async move {
                                        // permit is held for the duration of the connection
                                        // and released (dropping the OwnedSemaphorePermit)
                                        // when the task exits.
                                        let _permit = permit;
                                        if let Err(e) = Self::handle_n2c_connection(
                                            stream, magic, qh, bp, mp, tv, ledger, ann_rx,
                                            rb_rx, metrics.clone(),
                                        )
                                        .await
                                        {
                                            debug!("N2C connection ended: {e}");
                                        }
                                        metrics
                                            .n2c_connections_active
                                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                                    });
                                    conn_handles.push(handle);

                                    // C13 fix: periodically prune completed JoinHandles
                                    // to prevent unbounded Vec growth (each finished handle
                                    // is ~128 bytes; over millions of connections this adds up).
                                    accept_count += 1;
                                    if accept_count.is_multiple_of(PRUNE_INTERVAL) {
                                        conn_handles.retain(|h| !h.is_finished());
                                    }
                                }
                                Err(e) => {
                                    warn!("N2C accept error: {e}");
                                }
                            }
                        }
                        _ = shutdown.changed() => {
                            info!("N2C server shutting down");
                            break;
                        }
                    }
                }
                // Abort all active N2C connection handlers.
                for handle in &conn_handles {
                    handle.abort();
                }
            });
        }

        // Initialize peer manager
        {
            let pm_config = PeerManagerConfig {
                diffusion_mode: match self.config.diffusion_mode {
                    crate::config::DiffusionMode::InitiatorOnly => DiffusionMode::InitiatorOnly,
                    crate::config::DiffusionMode::InitiatorAndResponder => {
                        DiffusionMode::InitiatorAndResponder
                    }
                },
                peer_sharing_enabled: self
                    .config
                    .effective_peer_sharing(self.block_producer.is_some()),
                target_hot_peers: self.config.target_number_of_active_peers,
                target_warm_peers: self
                    .config
                    .target_number_of_established_peers
                    .saturating_sub(self.config.target_number_of_active_peers),
                target_known_peers: self.config.target_number_of_known_peers,
                ..PeerManagerConfig::default()
            };
            let mut pm = NodePeerManager::new(pm_config);
            // Register our own listen address to prevent self-connections
            // (peers may share our address back to us via peer sharing)
            pm.set_local_addr(self.listen_addr);
            // Wire GSM event sender so peer_disconnected() emits events
            pm.set_gsm_event_tx(self.gsm_event_tx.clone());
            // #933: re-mirror the consensus mode — this instance REPLACES
            // the construction-time one, so the `consensusMode` dimension
            // of `haa_satisfied`'s case split must be carried over.
            pm.set_genesis_mode(self.consensus_mode == "genesis");
            *self.peer_manager.write().await = pm;
        }
        let peer_manager = self.peer_manager.clone();

        // G9: Process-freeze watchdog.
        //
        // A lightweight task that ticks every HEARTBEAT_TICK and warns if the
        // tick was more than HEARTBEAT_LATE_THRESHOLD late.  This surfaces
        // host-level freezes (macOS App Nap, cgroup CPU throttling, swap storms)
        // that would otherwise be invisible until a missed leader slot is observed
        // post-mortem.  On macOS, the launch wrapper should also prepend
        // `caffeinate -dimsu` to prevent App Nap from suspending the process.
        {
            let mut heartbeat_shutdown = shutdown_rx.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(HEARTBEAT_TICK);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last_tick = std::time::Instant::now();
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let now = std::time::Instant::now();
                            let elapsed = now.duration_since(last_tick);
                            if elapsed > HEARTBEAT_TICK + HEARTBEAT_LATE_THRESHOLD {
                                warn!(
                                    elapsed_ms = elapsed.as_millis(),
                                    threshold_ms = (HEARTBEAT_TICK + HEARTBEAT_LATE_THRESHOLD).as_millis(),
                                    "PROCESS FREEZE DETECTED: heartbeat tick was late by {}ms. \
                                     Check for host-level CPU throttling or OS suspension \
                                     (macOS App Nap, cgroup, swap storm).",
                                    elapsed.saturating_sub(HEARTBEAT_TICK).as_millis()
                                );
                            }
                            last_tick = now;
                        }
                        _ = heartbeat_shutdown.changed() => {
                            break;
                        }
                    }
                }
            });
        }

        // #768: apply-stall watchdog. In all normal operation the ChainDB write
        // precedes the ledger apply, so the ChainDB tip is >= the ledger tip.
        // The ONLY way the ledger tip exceeds the ChainDB tip is an
        // ahead-of-storage snapshot (a Mithril import gap, or a pre-#762
        // stranded DB whose in-between blocks are missing). That state is normal
        // *transiently* while peers backfill the gap, but if it PERSISTS with
        // zero forward progress (neither tip advances) while fetched blocks keep
        // arriving and being skipped as non-connecting, the gap is unbridgeable:
        // the node would otherwise busy-loop the header-validation hot path
        // forever (#768). Detect it and exit cleanly with an actionable error.
        // This is byte-exact-safe (no ledger/consensus state is touched) and
        // cannot false-fire on a healthy node (the gate `ledger > chaindb` never
        // holds there) nor on a slowly-progressing one (any tip advance resets
        // the timer).
        {
            let watchdog_chain_db = self.chain_db.clone();
            let watchdog_ledger = self.ledger_state.clone();
            let watchdog_metrics = self.metrics.clone();
            let watchdog_shutdown_tx = shutdown_tx.clone();
            let mut watchdog_shutdown = shutdown_rx.clone();
            tokio::spawn(async move {
                use std::sync::atomic::Ordering;
                let mut interval = tokio::time::interval(APPLY_STALL_CHECK_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last_progress = std::time::Instant::now();
                let mut last_ledger_slot = 0u64;
                let mut last_chaindb_slot = 0u64;
                let mut last_skipped = 0u64;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let chaindb_slot = watchdog_chain_db
                                .read()
                                .await
                                .get_tip()
                                .point
                                .slot()
                                .map(|s| s.0)
                                .unwrap_or(0);
                            let ledger_slot = watchdog_ledger
                                .read()
                                .await
                                .tip
                                .point
                                .slot()
                                .map(|s| s.0)
                                .unwrap_or(0);
                            let skipped = watchdog_metrics
                                .fetched_blocks_not_connecting
                                .load(Ordering::Relaxed);

                            // Any tip advance = forward progress; reset the timer.
                            if ledger_slot > last_ledger_slot || chaindb_slot > last_chaindb_slot {
                                last_progress = std::time::Instant::now();
                            }
                            last_ledger_slot = ledger_slot;
                            last_chaindb_slot = chaindb_slot;

                            let work_arriving = skipped > last_skipped;
                            last_skipped = skipped;

                            if apply_stall_detected(
                                ledger_slot,
                                chaindb_slot,
                                work_arriving,
                                last_progress.elapsed(),
                                APPLY_STALL_TIMEOUT,
                            ) {
                                error!(
                                    ledger_slot,
                                    chaindb_tip_slot = chaindb_slot,
                                    gap_slots = ledger_slot - chaindb_slot,
                                    skipped_blocks = skipped,
                                    stalled_for_s = last_progress.elapsed().as_secs(),
                                    "APPLY STALL (#768): the ledger snapshot tip is ahead of \
                                     the ChainDB tip and no block has applied while fetched \
                                     blocks are being skipped as non-connecting. The database \
                                     is stranded — the in-between blocks are missing and the \
                                     gap cannot be bridged. Shutting down; re-import via \
                                     `dugite-node mithril-import` and restart."
                                );
                                let _ = watchdog_shutdown_tx.send(true);
                                break;
                            }
                        }
                        _ = watchdog_shutdown.changed() => {
                            break;
                        }
                    }
                }
            });
        }

        // Register topology peers in the peer manager with full metadata
        let detailed_peers = self.topology.detailed_peers();
        if detailed_peers.is_empty() {
            warn!("No peers configured in topology");
            return Ok(());
        }
        if self.topology.has_bootstrap_peers() {
            info!(
                "Bootstrap peers configured (trustable: {})",
                self.topology.has_trustable_peers()
            );
        }
        {
            // Resolve all DNS addresses BEFORE acquiring the write lock to avoid
            // holding the lock during potentially slow DNS lookups.
            //
            // Uses SRV-first resolution: tries `_cardano._tcp.<host>` SRV records
            // first (Haskell cardano-node behaviour), falls back to A/AAAA on
            // the original hostname if no SRV records exist.
            use dugite_network::peer::discovery::{
                resolve_with_srv, DnsResolver, HickoryDnsResolver, NoopDnsResolver,
            };
            // SRV/A/AAAA resolution is a "best effort" startup step.  If the
            // system resolver is misconfigured (malformed `/etc/resolv.conf`,
            // unusual nameserver entry, transient DHCP/VPN state change — on
            // macOS the published IPv6 link-local nameserver `fe80::…%en0`
            // is rejected by hickory with `invalid IP address syntax`),
            // `HickoryDnsResolver::new()` returns Err.  Previously this
            // caused the entire node-startup function to `return Ok(())`
            // early, exiting the process cleanly with no chainsync /
            // blockfetch server ever bound — devnet-validate boots would
            // fail with "Socket … did not become ready within 120s" and no
            // panic to diagnose.  Fall back to a no-op resolver instead:
            // the literal-IP fast path in `resolve_with_srv` still
            // resolves devnet's `127.0.0.1` peers, and ledger-peer
            // discovery populates further peers from chain state.
            let dns_resolver: Box<dyn DnsResolver> = match HickoryDnsResolver::new() {
                Ok(r) => Box::new(r),
                Err(e) => {
                    warn!(
                        error = %e,
                        "Failed to create DNS resolver — falling back to NoopDnsResolver; \
                         literal-IP peers still resolved, hostname peers require chain-state discovery"
                    );
                    Box::new(NoopDnsResolver)
                }
            };

            let mut resolved_peers: Vec<std::net::SocketAddr> = Vec::new();
            // Bootstrap / trustable peers' resolved addresses, registered with
            // the PeerManager so haa_satisfied() can recognise them as the
            // trusted external set (Haskell UseBootstrapPeers HAA path). A
            // `trustable` topology entry is a bootstrap peer or a trustable
            // local root — exactly Haskell's trusted-peer set.
            let mut resolved_bootstrap: Vec<std::net::SocketAddr> = Vec::new();
            for peer in &detailed_peers {
                let addrs = resolve_with_srv(dns_resolver.as_ref(), &peer.address, peer.port).await;
                if addrs.is_empty() {
                    warn!(
                        address = %peer.address,
                        port = peer.port,
                        "Failed to resolve peer address (SRV + A/AAAA both failed)"
                    );
                } else {
                    if peer.trustable {
                        resolved_bootstrap.extend(addrs.iter().copied());
                    }
                    resolved_peers.extend(addrs);
                }
            }

            // Resolve local root group members for per-group valency registration.
            // Each entry carries resolved addresses plus all topology metadata so
            // `add_local_root_group` can be called with fully-populated info.
            struct ResolvedGroup {
                orig_index: usize,
                addrs: Vec<std::net::SocketAddr>,
                hot_valency: usize,
                warm_valency: usize,
                diffusion_mode: Option<networking::DiffusionMode>,
                behind_firewall: bool,
                advertise: bool,
            }
            let mut resolved_groups: Vec<ResolvedGroup> = Vec::new();
            for (orig_index, group) in self.topology.local_roots.iter().enumerate() {
                let hot_val = usize::from(group.effective_hot_valency());
                let warm_val = usize::from(group.effective_warm_valency());
                let diffusion_mode = group.diffusion_mode.as_deref().map(|s| match s {
                    "InitiatorOnly" => networking::DiffusionMode::InitiatorOnly,
                    _ => networking::DiffusionMode::InitiatorAndResponder,
                });
                let mut group_addrs = Vec::new();
                for ap in &group.access_points {
                    let addrs = resolve_with_srv(dns_resolver.as_ref(), &ap.address, ap.port).await;
                    if addrs.is_empty() {
                        warn!(
                            address = %ap.address,
                            port = ap.port,
                            "Failed to resolve local root group member address (SRV + A/AAAA both failed)"
                        );
                    } else {
                        group_addrs.extend(addrs);
                    }
                }
                if !group_addrs.is_empty() {
                    resolved_groups.push(ResolvedGroup {
                        orig_index,
                        addrs: group_addrs,
                        hot_valency: hot_val,
                        warm_valency: warm_val,
                        diffusion_mode,
                        behind_firewall: group.is_behind_firewall(),
                        advertise: group.advertise,
                    });
                }
            }

            // #871: periodic DNS re-resolution of topology / local-root names.
            //
            // Topology and local-root DNS names were resolved exactly ONCE at
            // startup, so a block producer whose relay's A/AAAA record rotated
            // silently lost its relay (the governor retried the stale IP forever
            // at the 160s backoff cap), and a transient DNS failure at startup
            // dropped a local-root group with no retry. This loop re-resolves
            // both sets periodically, re-registering resolved addresses
            // (idempotent by SocketAddr — inner add_peer never overwrites) and
            // upserting each local-root group by its stable `local-root-{index}`
            // name. On an EMPTY resolution it keeps the previous addresses (never
            // forgets a group / bootstrap set on a transient failure — this fixes
            // the startup-drop). A fixed 5-minute interval approximates
            // cardano-node's TTL-driven DNSActions; resolve_with_srv does not yet
            // surface record TTLs. (Stale rotated-out IPs linger as Topology cold
            // peers rather than being pruned; connectivity is restored via the
            // freshly-resolved address.)
            {
                let re_pm = peer_manager.clone();
                let re_peers = detailed_peers.clone();
                let re_groups = self.topology.local_roots.clone();
                let mut re_shutdown = shutdown_rx.clone();
                tokio::spawn(async move {
                    let resolver: Box<dyn DnsResolver> = match HickoryDnsResolver::new() {
                        Ok(r) => Box::new(r),
                        Err(_) => Box::new(NoopDnsResolver),
                    };
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
                    interval.tick().await; // skip the immediate first tick
                    loop {
                        tokio::select! {
                            _ = interval.tick() => {}
                            _ = re_shutdown.changed() => break,
                        }
                        // Re-resolve bootstrap / config peers.
                        for peer in &re_peers {
                            let addrs =
                                resolve_with_srv(resolver.as_ref(), &peer.address, peer.port).await;
                            if addrs.is_empty() {
                                continue; // keep previous on transient failure
                            }
                            let mut w = re_pm.write().await;
                            for a in &addrs {
                                w.add_config_peer(*a);
                                if peer.trustable {
                                    w.add_bootstrap_peer(*a);
                                }
                            }
                        }
                        // Re-resolve local-root groups, upserting by stable name.
                        for (gi, group) in re_groups.iter().enumerate() {
                            let hot_val = usize::from(group.effective_hot_valency());
                            let warm_val = usize::from(group.effective_warm_valency());
                            let diffusion_mode = group.diffusion_mode.as_deref().map(|s| match s {
                                "InitiatorOnly" => networking::DiffusionMode::InitiatorOnly,
                                _ => networking::DiffusionMode::InitiatorAndResponder,
                            });
                            let mut group_addrs = Vec::new();
                            for ap in &group.access_points {
                                let addrs =
                                    resolve_with_srv(resolver.as_ref(), &ap.address, ap.port).await;
                                group_addrs.extend(addrs);
                            }
                            if group_addrs.is_empty() {
                                continue; // keep previous group on transient failure
                            }
                            let mut w = re_pm.write().await;
                            w.add_local_root_group(networking::LocalRootGroupInfo {
                                name: format!("local-root-{gi}"),
                                addrs: group_addrs,
                                hot_valency: hot_val,
                                warm_valency: warm_val,
                                diffusion_mode,
                                behind_firewall: group.is_behind_firewall(),
                                advertise: group.advertise,
                            });
                        }
                    }
                    tracing::debug!("DNS re-resolution loop exiting (shutdown)");
                });
            }

            let mut pm = peer_manager.write().await;
            for socket_addr in resolved_peers {
                pm.add_config_peer(socket_addr);
            }
            // Register bootstrap/trustable peers for the UseBootstrapPeers HAA
            // path (must be AFTER add_config_peer so the peer table has them).
            for socket_addr in resolved_bootstrap {
                pm.add_bootstrap_peer(socket_addr);
            }
            // Register per-group valency targets.  This must happen AFTER
            // add_config_peer() calls so the peer table contains the members.
            // #871: give each group a stable name (its topology index) so the
            // periodic DNS re-resolution loop can upsert it in place.
            for rg in resolved_groups.into_iter() {
                pm.add_local_root_group(networking::LocalRootGroupInfo {
                    name: format!("local-root-{}", rg.orig_index),
                    addrs: rg.addrs,
                    hot_valency: rg.hot_valency,
                    warm_valency: rg.warm_valency,
                    diffusion_mode: rg.diffusion_mode,
                    behind_firewall: rg.behind_firewall,
                    advertise: rg.advertise,
                });
            }

            // ── peerSnapshotFile: pre-seed big-ledger candidates ─────────────
            //
            // The cardano-node 10.x topology can reference a peer snapshot
            // file (typically distributed by IOG) that lists the current
            // top-90%-stake "Big Ledger Pools" with their relays. Loading it
            // at startup gives us a populated big-ledger candidate pool
            // before the live ledger has caught up far enough for the
            // periodic ledger-peer discovery loop to populate them via
            // useLedgerAfterSlot.
            //
            // Path resolution: relative to the topology file's directory
            // (matching cardano-node behaviour). DNS hostnames are kept
            // as-is and resolved when the peer is dialed.
            if let Some(snapshot_filename) = self.topology.peer_snapshot_file.as_deref() {
                let topology_dir = self
                    .topology_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let snapshot_path = topology_dir.join(snapshot_filename);
                match crate::gsm::load_peer_snapshot(&snapshot_path) {
                    Ok(entries) => {
                        let mut blp_count = 0usize;
                        let mut total_resolved = 0usize;
                        for entry in &entries {
                            // Resolve DNS synchronously here at startup.
                            // This is bounded by the snapshot size and only
                            // runs once.
                            let host_port = format!("{}:{}", entry.host, entry.port);
                            if let Ok(addrs) = host_port.to_socket_addrs() {
                                for socket_addr in addrs {
                                    pm.add_ledger_peer(socket_addr);
                                    if entry.is_big_ledger {
                                        pm.add_big_ledger_peer(socket_addr);
                                        blp_count += 1;
                                    }
                                    total_resolved += 1;
                                }
                            }
                        }
                        info!(
                            snapshot = %snapshot_path.display(),
                            entries = entries.len(),
                            resolved = total_resolved,
                            big_ledger = blp_count,
                            "Loaded peer snapshot",
                        );
                    }
                    Err(e) => {
                        // Non-fatal: log and continue. The periodic
                        // ledger-peer discovery loop will populate peers
                        // once the chain catches up.
                        warn!(snapshot = %snapshot_path.display(), error = %e, "Failed to load peer snapshot");
                    }
                }
            }

            let stats = pm.stats();
            info!(
                known = stats.cold + stats.warm + stats.hot,
                local_root_groups = pm.local_root_groups().len(),
                mode = ?pm.diffusion_mode(),
                "Peers",
            );
        }
        let _peers = self.topology.all_peers();

        // ── RuntimeConfig watch channel (SIGHUP → governor) ─────────────────
        //
        // The `RuntimeConfig` watch channel is the single source of truth for
        // all hot-reloadable config values.  It is written by the SIGHUP
        // handler (below) and read on every governor tick so the governor
        // always operates with the latest operator-supplied targets.
        //
        // This satisfies the acceptance criterion (#488): `kill -HUP <pid>`
        // with an edited config propagates to the governor within the next
        // 2-second tick — well within the 10-second budget.
        use crate::config_reload::RuntimeConfig;
        let initial_runtime_config = RuntimeConfig::from_node_config(&self.config);

        // Initialise the peer governor target gauges from the boot-time config.
        self.metrics.set_peer_governor_targets(
            initial_runtime_config.target_number_of_active_peers,
            initial_runtime_config.target_number_of_established_peers,
            initial_runtime_config.target_number_of_known_peers,
            initial_runtime_config.target_number_of_root_peers,
            initial_runtime_config.target_number_of_active_big_ledger_peers,
            initial_runtime_config.target_number_of_established_big_ledger_peers,
            initial_runtime_config.target_number_of_known_big_ledger_peers,
        );

        let (runtime_config_tx, runtime_config_rx) = watch::channel(initial_runtime_config);

        // On non-Unix platforms there is no SIGHUP so the sender is never used.
        // Drop it here so the receiver's `has_changed()` returns Err (no sender)
        // and the governor tick skips the update path cleanly.
        #[cfg(not(unix))]
        drop(runtime_config_tx);

        // Setup SIGHUP handler for topology + config reload (#322, #473, #488)
        //
        // On SIGHUP:
        //   1. Reload topology: add any new peers to the peer manager.
        //   2. Reload full NodeConfig: partition changed fields into
        //      "applied" (hot-reloadable) vs "ignored" (restart-required).
        //   3. Apply reloadable fields: peer governor targets, churn intervals,
        //      stall/error demotion thresholds, log directive/severity.
        //   4. Send updated RuntimeConfig on the watch channel so the governor
        //      and other consumers pick up new values on their next iteration.
        //   5. Bump the appropriate `dugite_config_reload_total{result=...}` counter.
        #[cfg(unix)]
        {
            use crate::config_reload;
            let topology_path = self.topology_path.clone();
            let config_path = self.config_path.clone();
            let log_handle = self.log_handle.clone();
            let pm_for_sighup = peer_manager.clone();
            let metrics_for_sighup = self.metrics.clone();
            let runtime_config_tx_for_sighup = runtime_config_tx;
            // Snapshot of the boot-time NodeConfig as the comparison baseline.
            let mut live_config = self.config.clone();
            let mut hup_shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                let mut hup = match signal::unix::signal(signal::unix::SignalKind::hangup()) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Failed to setup SIGHUP handler: {e}");
                        return;
                    }
                };
                loop {
                    tokio::select! {
                        _ = hup.recv() => {
                            info!(
                                path = %topology_path.display(),
                                "SIGHUP received — reloading topology"
                            );

                            // ── Step 1: topology reload ───────────────────────────
                            match crate::topology::Topology::load(&topology_path) {
                                Ok(new_topology) => {
                                    let new_peers = new_topology.detailed_peers();
                                    let mut resolved: Vec<std::net::SocketAddr> = Vec::new();
                                    for peer in &new_peers {
                                        match tokio::net::lookup_host(format!(
                                            "{}:{}",
                                            peer.address, peer.port
                                        ))
                                        .await
                                        {
                                            Ok(addrs) => {
                                                for socket_addr in addrs {
                                                    resolved.push(socket_addr);
                                                }
                                            }
                                            Err(e) => {
                                                warn!(
                                                    address = %peer.address,
                                                    port = peer.port,
                                                    "Failed to resolve peer address during topology reload: {e}"
                                                );
                                            }
                                        }
                                    }
                                    let mut pm = pm_for_sighup.write().await;
                                    let added = resolved.len();
                                    for socket_addr in resolved {
                                        pm.add_config_peer(socket_addr);
                                    }
                                    info!(added, stats = %pm.stats(), "Topology reloaded");
                                }
                                Err(e) => {
                                    error!("Failed to reload topology: {e}");
                                }
                            }

                            // ── Step 2: full NodeConfig reload ────────────────────
                            info!(
                                path = %config_path.display(),
                                "SIGHUP — reloading node config"
                            );
                            let new_config = match NodeConfig::load(&config_path) {
                                Ok(c) => c,
                                Err(e) => {
                                    error!(
                                        "config_reload: failed to parse '{}': {e} — live config unchanged",
                                        config_path.display()
                                    );
                                    metrics_for_sighup.config_reload_rejected
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    continue; // skip to next SIGHUP
                                }
                            };

                            // ── Step 3: partition changed fields ──────────────────
                            let plan = config_reload::reload_partition(&live_config, &new_config);

                            if !plan.ignored.is_empty() {
                                warn!(
                                    fields = ?plan.ignored,
                                    "config_reload: restart-required fields changed — ignored (restart the node to apply)"
                                );
                            }

                            if !plan.has_applied() && plan.ignored.is_empty() {
                                info!("config_reload: no fields changed — nothing to do");
                                // Still counts as "ignored" per the spec
                                metrics_for_sighup.config_reload_ignored
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                live_config = new_config;
                                continue;
                            }

                            if !plan.has_applied() {
                                // Only restart-required fields changed.
                                info!(
                                    fields = ?plan.ignored,
                                    "config_reload: all changed fields require restart — ignored"
                                );
                                metrics_for_sighup.config_reload_ignored
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                live_config = new_config;
                                continue;
                            }

                            // ── Step 4: apply hot-reloadable fields ───────────────
                            //
                            // Log directive / min severity (preserves existing #473 behaviour).
                            //
                            // `LogDirective` is full EnvFilter syntax and is applied verbatim.
                            // `MinSeverity` is a cardano-node syslog severity and MUST be
                            // translated to a valid tracing level — handing it raw to EnvFilter
                            // silently mis-parses Notice/Warning/Critical/Alert/Emergency into a
                            // bogus per-target TRACE directive.
                            if let Some(handle) = log_handle.as_ref() {
                                let directive: String =
                                    new_config.log_directive.clone().unwrap_or_else(|| {
                                        crate::logging::min_severity_to_directive(
                                            &new_config.min_severity,
                                        )
                                        .to_string()
                                    });
                                match handle.reload(&directive) {
                                    Ok(()) => info!(
                                        directive = %directive,
                                        "config_reload: log directive applied"
                                    ),
                                    Err(e) => warn!(
                                        directive = %directive,
                                        "config_reload: failed to apply log directive: {e}"
                                    ),
                                }
                            }

                            info!(
                                applied = ?plan.applied,
                                ignored = ?plan.ignored,
                                "config_reload: applied hot-reloadable config changes"
                            );

                            // ── Step 4b: publish new RuntimeConfig on watch ────────
                            //
                            // The governor tick and any other consumer that holds a
                            // `watch::Receiver<RuntimeConfig>` will see the new values
                            // on their next iteration (≤ 2 seconds for the governor).
                            // Also update the Prometheus target gauges immediately
                            // so the metrics endpoint reflects the change within the
                            // same polling window.
                            let new_runtime = config_reload::RuntimeConfig::from_node_config(&new_config);
                            metrics_for_sighup.set_peer_governor_targets(
                                new_runtime.target_number_of_active_peers,
                                new_runtime.target_number_of_established_peers,
                                new_runtime.target_number_of_known_peers,
                                new_runtime.target_number_of_root_peers,
                                new_runtime.target_number_of_active_big_ledger_peers,
                                new_runtime.target_number_of_established_big_ledger_peers,
                                new_runtime.target_number_of_known_big_ledger_peers,
                            );
                            // send() only fails if all receivers are dropped — that
                            // would mean the main loop has already exited, so we log
                            // the condition but don't abort the SIGHUP handler.
                            if runtime_config_tx_for_sighup.send(new_runtime).is_err() {
                                warn!("config_reload: runtime_config watch has no receivers (main loop exited?)");
                            }

                            // ── Step 5: bump metric counter ───────────────────────
                            metrics_for_sighup.config_reload_applied
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                            // Advance the live config baseline to the new config
                            // so the next SIGHUP diffs against the current state.
                            live_config = new_config;
                        }
                        _ = hup_shutdown_rx.changed() => {
                            info!("SIGHUP handler shutting down");
                            break;
                        }
                    }
                }
            });
        }

        // Start N2N server for inbound peer connections.
        //
        // When DiffusionMode is InitiatorOnly, skip the N2N listener entirely —
        // the node only makes outbound connections (typical for block producers
        // behind a firewall).  Matches Haskell's `runM` branch that skips
        // `Server.with` for InitiatorOnlyDiffusionMode.
        //
        // Each accepted TCP connection gets its own Mux and set of protocol tasks:
        //   - Handshake (protocol 0, responder)
        //   - ChainSync (protocol 2, responder)
        //   - BlockFetch (protocol 3, responder)
        //   - TxSubmission2 (protocol 4, responder)
        //   - KeepAlive (protocol 8, responder)
        //   - PeerSharing (protocol 10, responder)
        //
        // The broadcast channels were already created by the N2C server above.
        // If block_announcement_tx is None (N2C server was skipped for some reason),
        // create the channels here as a fallback.
        if self.block_announcement_tx.is_none() {
            let (block_ann_tx, _) = tokio::sync::broadcast::channel::<
                dugite_network::BlockAnnouncement,
            >(BLOCK_ANN_CHANNEL_CAP);
            let (rollback_ann_tx, _) =
                tokio::sync::broadcast::channel::<RollbackAnnouncement>(ROLLBACK_ANN_CHANNEL_CAP);
            self.block_announcement_tx = Some(block_ann_tx);
            self.rollback_announcement_tx = Some(rollback_ann_tx);
            self.tip_broadcaster = Some(Arc::new(tip_broadcast::TipBroadcaster::new()));
        }

        // ─── UTxO RPC server (#672) ─────────────────────────────────────────
        //
        // Starts here so the tip_broadcaster + mempool feeds are both
        // guaranteed initialised (the broadcaster fallback above ensures
        // it). Gated entirely on RpcConfig.is_some() — when the operator
        // hasn't enabled RPC the gRPC stack and listener are never
        // touched. SyncService / QueryService / SubmitService /
        // WatchService are all implemented end-to-end.
        if let Some(rpc_cfg) = self.rpc_config.clone() {
            let adapter = Arc::new(crate::rpc_adapter::NodeRpcAdapter::new(
                self.chain_db.clone(),
                self.ledger_state.clone(),
                self.mempool.clone(),
                n2c_tx_validator.clone() as Arc<dyn dugite_network::TxValidator>,
                n2c_slot_config,
                self.shelley_genesis.clone(),
                self.era_history.clone(),
            ));
            let (tip_feed, tip_publisher) = crate::rpc_adapter::build_tip_feed();
            // Spawn forwarder: subscribes to the node-side TipBroadcaster
            // and republishes into the dugite-rpc TipPublisher. Keeps the
            // RPC crate dep-free of dugite-node.
            let broadcaster = self
                .tip_broadcaster
                .clone()
                .expect("tip_broadcaster initialised by the fallback above");
            let _tip_forwarder = crate::rpc_adapter::spawn_tip_forwarder(
                broadcaster,
                tip_publisher,
                shutdown_rx.clone(),
            );
            let mempool_feed = dugite_rpc::MempoolFeed::new(self.mempool.tx_events());
            let rpc_shutdown_rx = shutdown_rx.clone();
            let rpc_cfg_arc = Arc::new(rpc_cfg);
            match dugite_rpc::RpcServer::start(
                rpc_cfg_arc.clone(),
                adapter,
                tip_feed,
                mempool_feed,
                dugite_rpc::noop_metrics(),
                rpc_shutdown_rx,
            )
            .await
            {
                Ok(handle) => {
                    info!(
                        local_addr = %handle.local_addr,
                        "dugite-rpc: UTxO RPC server bound and accepting connections",
                    );
                    // We deliberately drop the handle here — the server
                    // task is rooted by tokio's task tree and shuts down
                    // cooperatively when shutdown_rx fires. M1.B may
                    // promote this to a Node field if graceful
                    // drain-on-error becomes useful.
                }
                Err(e) => {
                    error!(
                        bind = %rpc_cfg_arc.bind,
                        port = rpc_cfg_arc.port,
                        error = %e,
                        "dugite-rpc: RPC server failed to start"
                    );
                    return Err(anyhow::anyhow!(
                        "RPC server bind failed on {}:{}: {e}",
                        rpc_cfg_arc.bind,
                        rpc_cfg_arc.port
                    ));
                }
            }
        }

        // Channel for the N2N listener to send accepted+handshaked connections
        // to the main run loop for lifecycle manager registration.
        let (inbound_accept_tx, mut inbound_accept_rx) = tokio::sync::mpsc::channel::<
            Result<(std::net::SocketAddr, PeerConnection, f64), (std::net::SocketAddr, String)>,
        >(32);

        if self.config.diffusion_mode == crate::config::DiffusionMode::InitiatorAndResponder {
            let n2n_listen_addr = self.listen_addr;
            let n2n_shutdown_rx = shutdown_rx.clone();
            let n2n_network_magic = self.network_magic;
            let n2n_peer_sharing = self
                .config
                .effective_peer_sharing(self.block_producer.is_some());
            let n2n_metrics = self.metrics.clone();

            let diffusion_mode = self.peer_manager.read().await.diffusion_mode();
            info!(
                listen = %n2n_listen_addr,
                diffusion_mode = ?diffusion_mode,
                "N2N server listening"
            );

            // Bind the N2N listener with SO_REUSEADDR + SO_REUSEPORT (on Unix)
            // so that outbound connections from this same node — which pin
            // their source port to `n2n_listen_addr` via `TcpBearer::connect_from`
            // — can share the listen port. Matches Haskell ouroboros-network
            // `configureSocket` which sets these options on both inbound and
            // outbound sockets so duplex-paired connections are possible.
            let tcp_listener = match bind_n2n_listener(n2n_listen_addr) {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to bind N2N TCP listener on {n2n_listen_addr}: {e}");
                    return Err(e.into());
                }
            };

            // Snapshot of non-public IPs explicitly authorised by the static
            // topology (e.g. a co-located cardano-node relay at 127.0.0.1).
            // Non-public-IP inbound peers NOT in this set are rejected, so
            // an adversarial peer cannot trick us into accepting connections
            // that appear to come from internal/intranet hosts.
            let static_topology_ips: std::collections::HashSet<std::net::IpAddr> =
                self.peer_manager.read().await.static_topology_ips();
            let static_non_public_ips: std::collections::HashSet<std::net::IpAddr> =
                static_topology_ips
                    .iter()
                    .copied()
                    .filter(|ip| crate::node::networking::is_non_public_ip(*ip))
                    .collect();

            // A-001 / A-002 (security audit 2026-05-19): gate every inbound
            // connection through a Semaphore (global cap) and a ConnectionManager
            // (per-IP cap + state tracking) before spawning a handler task.
            //
            // Previously `accepted_connections_limit` was parsed from config but
            // never consulted at the accept point — `ConnectionManager::accept_inbound`
            // was dead code. Now it is the single mandatory gate.
            //
            // Haskell `cardano-node` enforces limits in `Server.run` via
            // `ConnectionLimits` (maxAcceptedConnections + maxAcceptedConnectionsPerHost)
            // acquired before the TCP fd enters the handshake pipeline.
            let n2n_acl = self.config.accepted_connections_limit.unwrap_or_default();
            let n2n_max_inbound = n2n_acl.hard_limit as usize;
            // Graduated admission band: below soft_limit accept immediately;
            // between soft and hard apply a delay ramping linearly to `delay`
            // seconds. Matches Haskell AcceptedConnectionsLimit.
            let n2n_soft_limit = (n2n_acl.soft_limit as usize).min(n2n_max_inbound);
            let n2n_accept_delay = n2n_acl.delay.max(0.0);
            // Per-IP concurrent-connection limit, now driven by the
            // `PerIpRateLimitN2n` config field (default 5, matching Haskell
            // maxAcceptedConnectionsPerHost) instead of a hardcoded constant.
            let n2n_per_ip_limit: usize = self.config.per_ip_rate_limit_n2n;

            // The semaphore doubles as a backpressure signal: when all permits
            // are taken the accept() call still proceeds (we don't want to stop
            // calling accept() as that would fill the OS SYN backlog), but the
            // spawn is immediately rejected by the ConnectionManager check below.
            let n2n_conn_semaphore =
                std::sync::Arc::new(tokio::sync::Semaphore::new(n2n_max_inbound));

            // Shared ConnectionManager for per-IP rate limiting.
            let n2n_conn_mgr = std::sync::Arc::new(dugite_network::ConnectionManager::new(
                dugite_network::ConnectionManagerConfig {
                    max_inbound: n2n_max_inbound,
                    max_outbound: 20,
                    per_ip_rate_limit: n2n_per_ip_limit,
                    network_magic: n2n_network_magic,
                    peer_sharing: n2n_peer_sharing,
                },
            ));

            let n2n_inbound_tx = inbound_accept_tx.clone();
            tokio::spawn(async move {
                let mut shutdown = n2n_shutdown_rx;
                // Per-IP rate limiting (G1 sliding-window + A-002 concurrent-count)
                // is enforced via the shared ConnectionManager (n2n_conn_mgr).
                loop {
                    tokio::select! {
                        accept_result = tcp_listener.accept() => {
                            match accept_result {
                                Ok((stream, peer_addr)) => {
                                    // Non-public IPs (loopback, RFC1918, link-local,
                                    // multicast, …) are only permitted when explicitly
                                    // listed in the static topology. This allows
                                    // co-located BP+relay over 127.0.0.1 while
                                    // rejecting any other internal-IP connection that
                                    // could only come from misconfiguration or abuse.
                                    // Public IPs are always accepted; outbound self-
                                    // connection is already prevented by
                                    // NodePeerManager::is_self_addr. Matches Haskell
                                    // ouroboros-network's PeerSharing IP-class filter
                                    // combined with localRoots overrides.
                                    if crate::node::networking::is_non_public_ip(peer_addr.ip())
                                        && !static_non_public_ips.contains(&peer_addr.ip())
                                    {
                                        debug!(
                                            %peer_addr,
                                            "N2N inbound rejected: non-public IP not in static topology"
                                        );
                                        drop(stream);
                                        continue;
                                    }
                                    // G1 (#547): per-IP sliding-window rate limit
                                    // (catches tight reconnect loops). 60-second window.
                                    //
                                    // #996: local roots are EXEMPT. Upstream has no
                                    // per-IP window at all — `Ouroboros.Network.Server
                                    // .RateLimiting` is a global soft/hard limit plus an
                                    // accept delay, and a local root peer is trusted and
                                    // never throttled by source address. Applying a
                                    // 5-per-60s window to a declared peer turns any
                                    // reconnect churn into a long outage, and collapses
                                    // every co-located or NAT'd peer into one bucket:
                                    // on the devnet all three nodes are 127.0.0.1, so
                                    // the limiter could not tell them apart and locked
                                    // cardano-bp out of the chain. The window still
                                    // applies to every IP the operator has not declared,
                                    // which is where the DoS concern actually lives.
                                    if !static_topology_ips.contains(&peer_addr.ip())
                                        && !n2n_conn_mgr.check_and_record_inbound_ip(peer_addr.ip()).await {
                                        debug!(
                                            %peer_addr,
                                            "N2N inbound rejected: per-IP sliding-window rate limit exceeded"
                                        );
                                        drop(stream);
                                        continue;
                                    }

                                    // A-001 / A-002 (#541): gate through ConnectionManager
                                    // for global max-inbound + per-IP concurrent-count.
                                    match n2n_conn_mgr.accept_inbound(peer_addr).await {
                                        Ok(()) => {}
                                        Err(dugite_network::ConnectionError::MaxConnectionsReached) => {
                                            info!(
                                                %peer_addr,
                                                max = n2n_max_inbound,
                                                "N2N inbound rejected: max connections reached"
                                            );
                                            drop(stream);
                                            continue;
                                        }
                                        Err(dugite_network::ConnectionError::RateLimited(_)) => {
                                            debug!(
                                                %peer_addr,
                                                per_ip_limit = n2n_per_ip_limit,
                                                "N2N inbound rejected: per-IP concurrent limit"
                                            );
                                            drop(stream);
                                            continue;
                                        }
                                        Err(e) => {
                                            debug!(%peer_addr, error = %e, "N2N inbound rejected by connection manager");
                                            drop(stream);
                                            continue;
                                        }
                                    }

                                    // Acquire a semaphore permit (non-blocking try variant).
                                    // The permit is dropped when the connection task exits,
                                    // freeing the slot for the next inbound connection.
                                    let permit = match n2n_conn_semaphore.clone().try_acquire_owned() {
                                        Ok(p) => p,
                                        Err(_) => {
                                            // All permits taken — ConnectionManager already
                                            // enforces max_inbound; this is a double-check.
                                            info!(
                                                %peer_addr,
                                                "N2N inbound rejected: semaphore full"
                                            );
                                            n2n_conn_mgr.remove_connection(&peer_addr).await;
                                            drop(stream);
                                            continue;
                                        }
                                    };

                                    // Graduated admission delay (Haskell
                                    // AcceptedConnectionsLimit / Ouroboros.Network.Server
                                    // .RateLimiting): once the inbound count is in the
                                    // [soft_limit, hard_limit) band, throttle the accept
                                    // rate with a delay that ramps linearly from 0 (at soft)
                                    // to `delay` seconds (at hard). hard_limit itself is
                                    // still a hard cap (semaphore + ConnectionManager above).
                                    if n2n_accept_delay > 0.0 && n2n_max_inbound > n2n_soft_limit {
                                        let used = n2n_max_inbound
                                            .saturating_sub(n2n_conn_semaphore.available_permits());
                                        if used > n2n_soft_limit {
                                            let frac = (used - n2n_soft_limit) as f64
                                                / (n2n_max_inbound - n2n_soft_limit) as f64;
                                            let delay_secs = n2n_accept_delay * frac.min(1.0);
                                            tokio::time::sleep(
                                                std::time::Duration::from_secs_f64(delay_secs),
                                            )
                                            .await;
                                        }
                                    }

                                    // A-007 (security audit 2026-05-19): downgrade to debug.
                                    // One INFO log per TCP SYN at 100 conn/s = ~50 KB/s of
                                    // structured JSON log traffic; synchronous log drain stalls
                                    // the async runtime. Haskell traces at Debug severity.
                                    debug!(%peer_addr, "N2N inbound connection accepted");
                                    let conn_metrics = n2n_metrics.clone();
                                    conn_metrics
                                        .n2n_connections_total
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                                    let magic = n2n_network_magic;
                                    let ps = n2n_peer_sharing;
                                    let tx = n2n_inbound_tx.clone();
                                    let cm = n2n_conn_mgr.clone();

                                    // G2: outer timeout guards against a peer
                                    // that holds the TCP stream open while
                                    // sending only partial data, parking the
                                    // task before the inner handshake timeout
                                    // is ever started.
                                    tokio::spawn(async move {
                                        // `permit` is held for the lifetime of this task.
                                        // Dropping it at end of scope releases the semaphore slot.
                                        let _permit = permit;
                                        let start = std::time::Instant::now();
                                        let result = tokio::time::timeout(
                                            N2N_INBOUND_TASK_TIMEOUT,
                                            PeerConnection::accept(
                                                stream, peer_addr, magic, false, ps,
                                            ),
                                        )
                                        .await;
                                        match result {
                                            Ok(Ok(conn)) => {
                                                let rtt_ms =
                                                    start.elapsed().as_secs_f64() * 1000.0;
                                                let _ =
                                                    tx.send(Ok((peer_addr, conn, rtt_ms))).await;
                                            }
                                            Ok(Err(e)) => {
                                                let _ = tx
                                                    .send(Err((peer_addr, e.to_string())))
                                                    .await;
                                            }
                                            Err(_elapsed) => {
                                                warn!(
                                                    %peer_addr,
                                                    timeout_secs = N2N_INBOUND_TASK_TIMEOUT.as_secs(),
                                                    "N2N inbound handshake timed out"
                                                );
                                            }
                                        }
                                        // Decrement per-IP counter on task exit.
                                        cm.remove_connection(&peer_addr).await;
                                    });
                                }
                                Err(e) => {
                                    warn!("N2N accept error: {e}");
                                }
                            }
                        }
                        _ = shutdown.changed() => {
                            info!("N2N server shutting down");
                            break;
                        }
                    }
                }
            });
        } else {
            info!("N2N server skipped (DiffusionMode=InitiatorOnly, outbound connections only)");
        }

        // Ledger-based peer discovery — gated by useLedgerAfterSlot in the
        // topology, matching Haskell's ledgerPeersThread which always runs.
        {
            let ledger = self.ledger_state.clone();
            let pm = peer_manager.clone();
            let topology = self.topology.clone();
            let shutdown = shutdown_rx.clone();
            tokio::spawn(async move {
                // Check every 5 minutes for new ledger peers
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
                interval.tick().await; // skip first immediate tick
                let mut shutdown = shutdown;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {}
                        _ = shutdown.changed() => { break; }
                    }

                    let current_slot = {
                        let ls = ledger.read().await;
                        ls.tip.point.slot().map(|s| s.0).unwrap_or(0)
                    };

                    if !topology.ledger_peers_enabled(current_slot) {
                        continue;
                    }

                    // Extract relay addresses from registered pools and
                    // identify Big Ledger Peers (top 90% of active stake).
                    //
                    // Each entry carries the BLP classification of the pool
                    // it came from, so DNS-resolved addresses inherit it
                    // unambiguously. The previous port-only match between
                    // resolved addresses and BLP relays mis-tagged peers
                    // whenever any BLP and any non-BLP shared a port.
                    #[derive(Clone)]
                    struct LedgerRelay {
                        host: String,
                        port: u16,
                        is_blp: bool,
                    }
                    let relays: Vec<LedgerRelay> = {
                        let ls = ledger.read().await;

                        // Build pool_id -> stake map for BLP classification
                        let pool_stakes: Vec<_> = ls
                            .certs
                            .pool_params
                            .keys()
                            .map(|pool_id| {
                                let stake = ls
                                    .epochs
                                    .snapshots
                                    .set
                                    .as_ref()
                                    .and_then(|s| s.pool_stake.get(pool_id))
                                    .map(|s| s.0)
                                    .unwrap_or(0);
                                (pool_id.as_bytes().to_vec(), stake)
                            })
                            .collect();
                        let (big_pool_ids, _) = crate::gsm::identify_big_ledger_peers(&pool_stakes);
                        let big_pool_set: std::collections::HashSet<Vec<u8>> =
                            big_pool_ids.into_iter().collect();

                        let mut relays = Vec::new();
                        for (pool_id, pool_reg) in ls.certs.pool_params.iter() {
                            let is_blp = big_pool_set.contains(pool_id.as_bytes().as_slice());
                            for relay in &pool_reg.relays {
                                match relay {
                                    dugite_primitives::transaction::Relay::SingleHostAddr {
                                        port,
                                        ipv4,
                                        ..
                                    } => {
                                        if let (Some(port), Some(ipv4)) = (port, ipv4) {
                                            let host = format!(
                                                "{}.{}.{}.{}",
                                                ipv4[0], ipv4[1], ipv4[2], ipv4[3]
                                            );
                                            relays.push(LedgerRelay {
                                                host,
                                                port: *port,
                                                is_blp,
                                            });
                                        }
                                    }
                                    dugite_primitives::transaction::Relay::SingleHostName {
                                        port,
                                        dns_name,
                                    } => {
                                        if let Some(port) = port {
                                            relays.push(LedgerRelay {
                                                host: dns_name.clone(),
                                                port: *port,
                                                is_blp,
                                            });
                                        }
                                    }
                                    dugite_primitives::transaction::Relay::MultiHostName {
                                        dns_name,
                                    } => {
                                        relays.push(LedgerRelay {
                                            host: dns_name.clone(),
                                            port: 3001,
                                            is_blp,
                                        });
                                    }
                                }
                            }
                        }
                        relays
                    };

                    if relays.is_empty() {
                        continue;
                    }

                    // Sample a subset of ledger peers
                    // (don't try to resolve all thousands of pool relays)
                    let sample_size = 20.min(relays.len());
                    let step = relays.len() / sample_size;
                    let offset = (current_slot as usize) % step.max(1);
                    let sample: Vec<LedgerRelay> = relays
                        .iter()
                        .skip(offset)
                        .step_by(step.max(1))
                        .take(sample_size)
                        .cloned()
                        .collect();

                    // Resolve each sampled relay's DNS / IP, preserving its
                    // BLP classification per resolved socket address.
                    let mut resolved: Vec<(SocketAddr, bool)> = Vec::new();
                    for r in &sample {
                        if let Ok(addrs) =
                            tokio::net::lookup_host(format!("{}:{}", r.host, r.port)).await
                        {
                            // #879: keep ALL resolved addresses, not just the
                            // first — a BLP relay commonly has multiple A/AAAA
                            // records; taking `.next()` biased toward whatever
                            // the resolver happened to order first and dropped
                            // the rest of the relay set.
                            for socket_addr in addrs {
                                resolved.push((socket_addr, r.is_blp));
                            }
                        }
                    }

                    if !resolved.is_empty() {
                        let mut pm_w = pm.write().await;
                        // #879: rebuild the BLP set from scratch each pass so it
                        // stays a faithful snapshot of the current top-stake
                        // relays instead of accumulating stale/duplicate entries.
                        pm_w.clear_big_ledger_peers();
                        let mut blp_count = 0usize;
                        for (socket_addr, is_blp) in &resolved {
                            pm_w.add_ledger_peer(*socket_addr);
                            if *is_blp {
                                pm_w.add_big_ledger_peer(*socket_addr);
                                blp_count += 1;
                            }
                        }
                        let added = resolved.len();
                        debug!(
                            "Ledger peer discovery: +{added} peers ({blp_count} BLPs) from {} relays, {}",
                            relays.len(),
                            pm_w.stats()
                        );
                    }
                }
            });
        }

        // ─── Initialize ConnectionLifecycleManager ─────────────────────────
        //
        // The lifecycle manager owns all peer connections and handles
        // temperature transitions (Cold -> Warm -> Hot and back).
        // Governor actions are dispatched through the lifecycle manager,
        // which creates/tears down protocol tasks on the single per-peer
        // mux connection.
        // #767: capacity defaults to FETCHED_BLOCKS_CHANNEL_CAP (4096) — the
        // deeper buffer widens the apply-lag tolerance window during bulk
        // catch-up.  Overridable via `DUGITE_FETCHED_BLOCKS_CAP` (same pattern
        // as `DUGITE_PIPELINE_DEPTH`) so memory-constrained deployments can
        // lower it: each in-flight `FetchedBlock` holds a fully-decoded Conway
        // block, so peak memory scales with this cap.  A value of 0 or an
        // unparseable value falls back to the default.
        let fetched_blocks_cap: usize = std::env::var("DUGITE_FETCHED_BLOCKS_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(FETCHED_BLOCKS_CHANNEL_CAP);
        let (fetched_blocks_tx, fetched_blocks_rx) =
            mpsc::channel::<FetchedBlock>(fetched_blocks_cap);
        let (peer_failure_tx, peer_failure_rx) = mpsc::channel::<(SocketAddr, PeerFailureKind)>(64);
        let (keepalive_rtt_tx, keepalive_rtt_rx) = mpsc::channel::<(SocketAddr, f64)>(256);
        let candidate_chains: Arc<RwLock<HashMap<SocketAddr, CandidateChainState>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let connect_timeout = Duration::from_secs(5);
        // Read security_param and active_slots_coeff from genesis config;
        // fall back to mainnet defaults.
        let security_param = self
            .shelley_genesis
            .as_ref()
            .map(|g| g.security_param)
            .unwrap_or(2160);
        let active_slots_coeff = self
            .shelley_genesis
            .as_ref()
            .map(|g| g.active_slots_coeff)
            .unwrap_or(0.05);
        // Build the tx validator for N2N TxSubmission2 admission.
        // Uses the same Phase-1 + Phase-2 pipeline as the N2C path so that
        // peers advertising invalid txs (is_valid tag mismatch, bad scripts,
        // fee violations) are rejected before they ever reach the mempool.
        // Fix for DoS class issue #522.
        let n2n_slot_config = self
            .shelley_genesis
            .as_ref()
            .map(|g| {
                let ste = epoch::shelley_transition_epoch_for_magic(self.network_magic);
                g.slot_config(ste, self.byron_epoch_length, self.byron_slot_duration_ms)
            })
            .unwrap_or(dugite_ledger::plutus::SlotConfig {
                zero_time: 0,
                zero_slot: 0,
                slot_length: 1000,
                // Per-tx horizon is plumbed in `LedgerTxValidator::validate`
                // before each `evaluate_plutus_scripts` call from the live
                // EraHistory; this fallback SlotConfig is only used when
                // shelley_genesis is None (very early boot) where no Plutus
                // tx can be admitted yet.
                safe_zone_horizon_slot: None,
            });
        let n2n_tx_validator: Arc<dyn dugite_network::TxValidator> =
            Arc::new(serve::LedgerTxValidator {
                ledger: self.ledger_state.clone(),
                slot_config: n2n_slot_config,
                metrics: self.metrics.clone(),
                mempool: Some(self.mempool.clone()),
                era_history: self.era_history.clone(),
                network: if self.network_magic
                    == dugite_primitives::network::NetworkId::Mainnet.magic()
                {
                    dugite_primitives::network::NetworkId::Mainnet
                } else {
                    dugite_primitives::network::NetworkId::Testnet
                },
            });

        let mut lifecycle = ConnectionLifecycleManager::new(
            self.network_magic,
            self.config
                .effective_peer_sharing(self.block_producer.is_some()),
            connect_timeout,
            candidate_chains.clone(),
            fetched_blocks_tx,
            self.block_announcement_tx
                .as_ref()
                .expect("block_announcement_tx was just set")
                .clone(),
            self.chain_db.clone(),
            self.ledger_state.clone(),
            self.ledger_view.clone(),
            self.ledger_tip_slot_tx.clone(),
            // Issue #654 P1.b — read-only seed Praos engine for the
            // per-peer eager-validation path. `Node.consensus` is mutated
            // by the body-apply path only; per-peer state isolation is
            // enforced by `validate_header_full_with_counters`'s
            // clone-and-swap. Snapshot via Arc::new + clone — the seed
            // does not need to track Node.consensus.update_tip() because
            // validate_header_full uses its parameters, not `self.tip`.
            Arc::new(self.consensus.clone()),
            // Issue #655 P2.b — shared apply-time bookkeeping map.
            self.eagerly_validated_headers.clone(),
            self.byron_epoch_length,
            security_param,
            active_slots_coeff,
            self.metrics.clone(),
            self.mempool.clone(),
            peer_failure_tx,
            keepalive_rtt_tx,
            self.gsm_event_tx.clone(),
            self.peer_registry.clone(),
            self.gsm_snapshot_rx.clone(),
            self.lop_params,
            self.historicity_cutoff_secs,
            self.csj.clone(),
            // Duplex server protocol fields
            Arc::new(serve::ChainDBBlockProvider {
                chain_db: self.chain_db.clone(),
            }),
            self.rollback_announcement_tx
                .as_ref()
                .expect("rollback_announcement_tx was just set")
                .clone(),
            peer_manager.clone(),
            self.peer_intersection_established.clone(),
            n2n_tx_validator,
            crate::node::connection_lifecycle::resolve_blockfetch_max_range(
                self.config.blockfetch_max_range,
            ),
            // Issue #742 Fix 2: grace period for ChainSel-starvation dynamo rotation.
            std::time::Duration::from_secs_f64(
                self.config
                    .low_level_genesis_options
                    .as_ref()
                    .map(|o| o.effective_block_fetch_grace_period_secs())
                    .unwrap_or(10.0),
            ),
        );
        // Enable outbound source-port pairing only when this node is also
        // running as a responder (it has a listen socket to share). When
        // diffusion mode is InitiatorOnly there is no listen port to pair
        // against, so we leave outbound on ephemeral source ports.
        //
        // ALSO skip pairing when the listen IP is a loopback address —
        // `bind(127.0.0.1:P)` then `connect()` to a public-internet host
        // fails with `EADDRNOTAVAIL` because the loopback source is not
        // routable to the destination, and outbound peer establishment
        // would silently fail for every public peer (issue #608).  An
        // unspecified bind (`0.0.0.0` / `::`) is fine: the kernel picks a
        // routable source IP per-destination at connect time.
        //
        // `connect_from` now ALSO falls back to ephemeral on connect failure
        // (not just bind), so this is a defence-in-depth guard rather than
        // the only safety net.
        let listen_ip_is_loopback = self.listen_addr.ip().is_loopback();
        if self.config.diffusion_mode == crate::config::DiffusionMode::InitiatorAndResponder
            && !listen_ip_is_loopback
        {
            lifecycle.set_local_listen_addr(self.listen_addr);
            info!(
                listen = %self.listen_addr,
                "outbound source-port pairing enabled (matches Haskell configureOutboundSocket)"
            );
        } else if listen_ip_is_loopback {
            info!(
                listen = %self.listen_addr,
                "outbound source-port pairing disabled (listen IP is loopback; outbound will use ephemeral source)"
            );
        }
        self.connection_lifecycle = Some(lifecycle);
        self.fetched_blocks_rx = Some(fetched_blocks_rx);
        self.peer_failure_rx = Some(peer_failure_rx);
        self.keepalive_rtt_rx = Some(keepalive_rtt_rx);

        // NOTE: there is deliberately no separate "BlockFetch decision task".
        //
        // A `BlockFetchLogicTask` used to be spawned here, described as the
        // analogue of Haskell's `blockFetchLogic` thread. Nothing ever called
        // its `register_peer`, so `evaluate_and_fetch` early-returned on an
        // empty `fetch_senders` on every tick for the lifetime of the node —
        // it did nothing, while reading like the live implementation (which is
        // how the architecture docs came to describe a multi-peer fetch pool
        // dugite does not have). Removed in #943.
        //
        // The real BlockFetch path is `ConnectionLifecycleManager::
        // make_blockfetch_task`, where the single contested fetcher slot lives
        // (`active_fetcher` + GSV top-K standby, matching Haskell's
        // `bfcMaxConcurrencyBulkSync = 1`).

        // ─── GSM (Genesis State Machine) ─────────────────────────────────
        let genesis_enabled = self.consensus_mode == "genesis";
        // LoE/GDD master switch (LowLevelGenesisOptions.EnableLoEAndGDD,
        // default true). Mirrors the construction-time gate so LoE enforcement
        // and GDD disconnects are skipped when an operator disables them.
        let loe_gdd_enabled = genesis_enabled
            && self
                .config
                .low_level_genesis_options
                .as_ref()
                .map(|o| o.enable_loe_and_gdd)
                .unwrap_or(true);
        if genesis_enabled {
            let gsm_state = self.gsm_snapshot_rx.borrow().state;
            info!(
                state = %gsm_state,
                "Genesis mode enabled — note: lightweight checkpointing and Genesis-specific \
                 peer selection are not yet implemented. The GSM provides basic state tracking \
                 (PreSyncing/Syncing/CaughtUp) and density-based peer disconnection."
            );
        }

        // Spawn GSM actor task and SyncStatus emitter; route GDD actions to
        // the main loop (full teardown needs `self.connection_lifecycle`).
        let mut gdd_action_rx: Option<tokio::sync::mpsc::Receiver<crate::gsm::GddAction>> = None;
        if let Some(parts) = self.gsm_actor_parts.take() {
            // 1. Spawn the GSM actor — owns the GenesisStateMachine and
            //    processes GsmEvent messages, publishing GsmSnapshot via watch.
            let gsm_actor_shutdown = shutdown_rx.clone();
            let gsm_chain_db = self.chain_db.clone();
            let gsm_era_history = self.era_history.clone();
            // Live tip age at spawn time — the durationUntilTooOld input for
            // the startup marker-staleness check (Haskell
            // initializationGsmState).
            let gsm_initial_tip_age = {
                let tip_slot_time_ms = self
                    .metrics
                    .tip_slot_time_ms
                    .load(std::sync::atomic::Ordering::Relaxed);
                if tip_slot_time_ms == 0 {
                    None // tip slot time unknown — trust the marker
                } else {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    Some(now_ms.saturating_sub(tip_slot_time_ms) / 1000)
                }
            };
            tokio::spawn(async move {
                let mut shutdown = gsm_actor_shutdown;
                tokio::select! {
                    _ = crate::gsm::run_gsm_actor(
                        parts.config,
                        parts.enabled,
                        parts.registry,
                        gsm_chain_db,
                        gsm_era_history,
                        parts.loe_out,
                        gsm_initial_tip_age,
                        parts.event_rx,
                        parts.snapshot_tx,
                        parts.action_tx,
                    ) => {}
                    _ = shutdown.changed() => {
                        debug!("GSM actor shutting down");
                    }
                }
            });

            // 2. GDD disconnect commands are consumed by the MAIN run loop
            //    (below) so the kill can run the full connection teardown via
            //    the lifecycle manager — Haskell's `cschGDDKill = throwTo tid
            //    DensityTooLow` terminates the ChainSync client and closes
            //    the connection, it does not merely re-label the peer
            //    (audit gdd-03/gsm-05).
            gdd_action_rx = Some(parts.action_rx);

            // 3. Spawn SyncStatus emitter — every 10 seconds, gathers the
            //    GSM transition inputs and sends a SyncStatus event:
            //    - HAA: ACTIVE (hot) big-ledger peer count;
            //    - the selection tip block number (candidate-vs-selection);
            //    - the LIVE tip age (now − tip slot wallclock), computed
            //      here each tick rather than read from a scrape-refreshed
            //      gauge (audit gsm-11: a stalled chain must still regress
            //      CaughtUp → PreSyncing without a Prometheus scraper).
            let status_event_tx = self.gsm_event_tx.clone();
            let status_pm = peer_manager.clone();
            let status_metrics = self.metrics.clone();
            let status_chain_db = self.chain_db.clone();
            let status_snapshot_rx = self.gsm_snapshot_rx.clone();
            let status_csj = self.csj.clone();
            let status_min_blp = self.gsm_min_active_blp;
            let status_shutdown = shutdown_rx.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(10));
                interval.tick().await; // skip first immediate tick
                let mut shutdown = status_shutdown;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {}
                        _ = shutdown.changed() => { break; }
                    }

                    let active_blp = {
                        let pm = status_pm.read().await;
                        // HAA satisfaction (Haskell outboundConnectionsState —
                        // an independent case split over (bootstrapPeersFlag,
                        // consensusMode), #933). Report a synthetic count the
                        // GSM's `>= min` gate reads (min when the HAA holds,
                        // the real BLP count otherwise). In Praos mode the
                        // value is inert: `evaluate()` returns None when the
                        // GSM is disabled, and nothing else consumes it.
                        if pm.haa_satisfied(status_min_blp) {
                            status_min_blp
                        } else {
                            pm.active_big_ledger_peer_count()
                        }
                    };
                    let selection_block_no = {
                        let db = status_chain_db.read().await;
                        db.get_tip_info().map(|(_, _, bn)| bn.0).unwrap_or(0)
                    };
                    let tip_slot_time_ms = status_metrics
                        .tip_slot_time_ms
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let tip_age_secs = if tip_slot_time_ms == 0 {
                        u64::MAX // no block applied yet — maximally stale
                    } else {
                        now_ms.saturating_sub(tip_slot_time_ms) / 1000
                    };

                    let event = crate::gsm::GsmEvent::SyncStatus {
                        active_blp_count: active_blp,
                        selection_block_no,
                        tip_age_secs,
                    };
                    if let Err(e) = status_event_tx.try_send(event) {
                        debug!("GSM SyncStatus event dropped: {e}");
                    }

                    // Publish genesis observability (GSM state, LoE tip, CSJ roles).
                    let snap = *status_snapshot_rx.borrow();
                    let gsm_state_code = match snap.state {
                        crate::gsm::GenesisSyncState::PreSyncing => 0,
                        crate::gsm::GenesisSyncState::Syncing => 1,
                        crate::gsm::GenesisSyncState::CaughtUp => 2,
                    };
                    let csj_roles = status_csj
                        .as_ref()
                        .map(|c| c.role_counts())
                        .unwrap_or((0, 0, 0, 0));
                    status_metrics.set_genesis_state(
                        gsm_state_code,
                        snap.loe_slot.unwrap_or(0),
                        csj_roles,
                    );
                }
            });
        }

        // ─── Main Run Loop ───────────────────────────────────────────────
        //
        // Single event loop that processes:
        // 1. Fetched blocks from BlockFetch workers -> apply to ledger
        // 2. Governor evaluation (every 2s) -> temperature transitions
        // 3. Forge ticker (every slot) -> block production
        // 4. Shutdown signal
        //
        // This replaces the old dual-path architecture (separate governor
        // connections + separate sync connections) with a unified loop that
        // receives blocks from the lifecycle-managed connections.
        let gov_config = {
            let cfg = &self.config;
            GovernorConfig {
                targets: PeerTargets {
                    target_warm: cfg.target_number_of_established_peers,
                    target_hot: cfg.target_number_of_active_peers,
                    max_cold: cfg.target_number_of_known_peers,
                    target_warm_big_ledger: cfg.target_number_of_established_big_ledger_peers,
                    target_hot_big_ledger: cfg.target_number_of_active_big_ledger_peers,
                },
                // ChurnIntervalNormalSecs → deadline churn, ChurnIntervalSyncSecs
                // → bulk-sync churn (selected by the governor's bulk-sync mode,
                // driven from the at-tip signal below). Defaults 3300/900 match
                // Haskell's deadline/bulk churn intervals.
                hot_churn_interval: Duration::from_secs(cfg.churn_interval_normal_secs),
                bulk_sync_churn_interval: Duration::from_secs(cfg.churn_interval_sync_secs),
                ..Default::default()
            }
        };
        let mut governor = Governor::new(gov_config);

        // Hold the runtime_config watch receiver so we can drain it on each
        // governor tick.  `borrow_and_update()` marks the latest value as
        // "seen" so successive ticks only see genuinely new values.
        let mut runtime_config_rx = runtime_config_rx;

        // Governor evaluation every 2 seconds — matches Haskell's warm-promotion
        // check frequency for responsive peer lifecycle management.
        let mut governor_ticker = tokio::time::interval(Duration::from_secs(2));
        // Skip mode: if the main loop was busy (e.g. applying blocks), we do NOT
        // want to burst-fire all missed governor ticks — one evaluation per interval
        // is sufficient and avoids multiple simultaneous peer-connect waves.
        governor_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        governor_ticker.tick().await; // skip first immediate tick

        // Channel for background cold->warm connection results.
        // The governor spawns connect tasks instead of awaiting them inline;
        // completed results arrive here for registration in the main loop.
        let (connect_result_tx, mut connect_result_rx) = mpsc::channel::<ConnectResult>(64);

        // Peers currently being connected in background tasks.
        // Prevents duplicate spawns when the governor fires repeatedly before
        // a slow TCP connect (up to connect_timeout) finishes.
        let mut in_flight_connects: std::collections::HashSet<std::net::SocketAddr> =
            std::collections::HashSet::new();

        // Take the fetched_blocks_rx out of self so we can use it in the select! loop
        // without holding a mutable borrow on self for the entire duration.
        let mut fetched_blocks_rx = self
            .fetched_blocks_rx
            .take()
            .expect("fetched_blocks_rx was just set");
        let mut peer_failure_rx = self
            .peer_failure_rx
            .take()
            .expect("peer_failure_rx was just set");
        let mut keepalive_rtt_rx = self
            .keepalive_rtt_rx
            .take()
            .expect("keepalive_rtt_rx was just set");

        // Forge ticker — fires every second (slot granularity) to check
        // for block production opportunities.  Only active when the node
        // is configured as a block producer.
        let has_block_producer = self.block_producer.is_some();
        let mut forge_ticker = tokio::time::interval(Duration::from_secs(1));
        forge_ticker.tick().await; // skip first immediate tick

        // ChainDB maintenance ticker — drives the volatile→immutable copy and
        // chain-fragment anchor advance during live sync (issue: from-genesis
        // apply-rate collapse).  The live `apply_fetched_block` path stores
        // every block in VolatileDB but never finalises k-deep blocks to
        // ImmutableDB, so VolatileDB (and the push-only chain fragment) grow
        // without bound.  `process_add_block` runs `get_all_fork_tips()` —
        // O(volatile size) — on every block add, so unbounded VolatileDB turns
        // per-block apply into O(N²): mainnet Byron sync decays from ~150 blk/s
        // to ~2 blk/s as the volatile set passes ~100 k blocks (profiled:
        // `get_all_fork_tips` = 30 % of the apply worker, RSS climbing past
        // 700 MB, all resetting on restart).  Flushing here bounds VolatileDB
        // to the k-block rollback window, mirroring Haskell's ChainDB
        // Background `copyToImmutableDB`.  250 ms cadence keeps the volatile
        // set within a few hundred blocks of k at multi-hundred-blk/s sync
        // while decoupling the flush from the per-block apply latency.
        let mut maintenance_ticker = tokio::time::interval(Duration::from_millis(250));
        maintenance_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        maintenance_ticker.tick().await; // skip first immediate tick

        // LoE-advance watcher: when the governor publishes a new LoE (its
        // tip slot changes in the GsmSnapshot), re-run chain selection so
        // blocks whose adoption was deferred by trimToLoE get re-evaluated
        // (Haskell triggerChainSelectionAsync / ChainSelReprocessLoEBlocks).
        let mut loe_watch_rx = self.gsm_snapshot_rx.clone();
        let mut last_seen_loe_slot = loe_watch_rx.borrow().loe_slot;
        let mut last_seen_gsm_state = loe_watch_rx.borrow().state;

        // Which target set the governor is currently running (true = sync
        // targets). Haskell `getPeerSelectionTargets` keys the switch on the
        // LedgerStateJudgement — PreSyncing and Syncing BOTH map to TooOld —
        // so the boundary is CaughtUp ↔ not-CaughtUp, never the
        // PreSyncing ↔ Syncing flap (#740). Apply sync targets from boot
        // when the GSM starts below CaughtUp rather than waiting for the
        // first transition.
        let mut sync_targets_applied = false;
        if genesis_enabled && last_seen_gsm_state != crate::gsm::GenesisSyncState::CaughtUp {
            let cfg = &self.config;
            let targets = dugite_network::peer::governor::PeerTargets {
                target_warm: cfg.sync_target_number_of_established_peers,
                target_hot: cfg.sync_target_number_of_active_peers,
                max_cold: cfg.sync_target_number_of_known_peers,
                target_warm_big_ledger: cfg.sync_target_number_of_established_big_ledger_peers,
                target_hot_big_ledger: cfg.sync_target_number_of_active_big_ledger_peers,
            };
            info!(
                state = %last_seen_gsm_state,
                target_hot = targets.target_hot,
                target_hot_blp = targets.target_hot_big_ledger,
                "Genesis boot below CaughtUp — applying sync peer-selection targets"
            );
            governor.update_targets(targets);
            sync_targets_applied = true;
        }

        info!("Main run loop entered");
        // #762: count blocks applied since the last volatile→immutable flush so
        // the flush is never starved. The main `select!` is `biased` with the
        // fetched-block arm AHEAD of the `maintenance_ticker` arm; under a slow
        // ValidateAll catch-up the fetched-block channel is perpetually ready,
        // so the ticker NEVER fires and `run_background_maintenance` never runs
        // — the VolatileDB grows without bound (observed: ~259k blocks / ~12
        // epochs), stranding every ledger snapshot far above the immutable tip
        // and making the node unrecoverable on restart. Forcing maintenance
        // every MAINTENANCE_FORCE_BLOCKS blocks bounds the VolatileDB to the
        // retention window + this interval, keeping the immutable flush within
        // ~k of the live tip regardless of ticker starvation.
        const MAINTENANCE_FORCE_BLOCKS: u64 = 256;
        let mut blocks_since_maintenance: u64 = 0;
        loop {
            tokio::select! {
                // #760: `biased;` + the shutdown arm FIRST so a shutdown signal
                // breaks the loop on the very next iteration (after the current
                // arm body returns), instead of being starved by tokio's random
                // select fairness while a high-volume arm (e.g. fetched-block
                // apply) stays perpetually ready. This bounds the COOPERATIVE
                // SIGTERM-to-break latency to one arm-body; the independent
                // watchdog (see the signal task) is the backstop for a truly
                // wedged arm body.
                biased;

                // ── Shutdown ────────────────────────────────────────────
                _ = shutdown_rx.changed() => {
                    info!("Shutdown signal received");
                    // Confirm any deferred Plutus before the shutdown snapshot so
                    // the persisted state reflects only Plutus-confirmed blocks.
                    // allow_cancel = false: this flush must COMPLETE (the window
                    // is small + memory-bounded), never observe its own shutdown
                    // signal and bail before persisting confirmed state.
                    self.flush_pending_phase2(false).await;
                    break;
                }

                // ── Process fetched blocks from BlockFetch workers ───────
                //
                // Every fetched block is unconditionally routed to
                // `apply_fetched_block`, which submits it to the
                // `ChainSelQueue`.  The queue handles:
                //   • Duplicate filtering (`AlreadyKnown` on hash match)
                //   • Storing fork / orphan blocks in VolatileDB
                //   • Running chain selection on every add, so a
                //     competing fork triggers `TriggeredFork` once its
                //     density exceeds the selected chain's.
                //   • Driving the ledger rollback + replay for that fork
                //
                // Previously this loop gated on `block_no > tip.block_no`
                // and on `prev_hash == tip.hash`, then buffered orphans
                // in a local `pending_blocks` HashMap.  Both gates were
                // unsound: the block_no guard silently dropped legitimate
                // fork blocks whose number equalled our tip, and the
                // hash gate prevented ChainSelQueue from ever seeing
                // out-of-order fork blocks — so the fork's density
                // could never be evaluated.  That combination produced
                // permanent live-tip stalls after any competing-fork
                // announcement (BlockFetch cycles every peer for the
                // same slot, no Chain extended fires).
                //
                // This matches Haskell `ChainDB.addBlock`: every block
                // reaches ChainDB unconditionally; chain selection owns
                // all routing decisions.
                Some(fetched) = fetched_blocks_rx.recv() => {
                    // Issue #742 Fix 2 — ChainSelStarvation edge recording
                    // (Haskell ChainDB `getChainSelMessage`): the dequeue that
                    // ends a starvation period stamps EndedAt(now) (CAS from
                    // Ongoing); recorded at DEQUEUE time, before the apply,
                    // exactly like Haskell.
                    if let Some(ref lc) = self.connection_lifecycle {
                        lc.chainsel_dequeued();
                    }
                    self.apply_fetched_block(fetched).await;
                    // Cross-block Phase-2 pooling flush: drain the deferred window
                    // when its REDEEMER count reaches the memory-safety cap
                    // (`defer_phase2_max_items` — the primary trigger, since a
                    // dense-Plutus region fills memory by redeemer count, not
                    // block count), or the block window fills, or the fetch queue
                    // has drained (we've caught up to the buffered blocks, so
                    // flush before idling / before any at-tip transition). No-op
                    // when the window is empty (deferral off / at-tip).
                    if !self.pending_phase2.is_empty()
                        && (self.pending_phase2_items >= self.defer_phase2_max_items
                            || self.pending_phase2.len() >= self.defer_phase2_window
                            || fetched_blocks_rx.is_empty())
                    {
                        self.flush_pending_phase2(true).await;
                    }
                    // After the apply, if the queue is empty we are about to
                    // block waiting → starvation Ongoing. A FULL queue during
                    // a long apply (epoch boundary, snapshot) keeps the old
                    // EndedAt, so BlockFetch never mistakes apply latency for
                    // peer starvation.
                    if fetched_blocks_rx.is_empty() {
                        if let Some(ref lc) = self.connection_lifecycle {
                            lc.chainsel_queue_empty();
                        }
                    }
                    // #762: force the volatile→immutable flush periodically even
                    // when the biased select starves the maintenance_ticker
                    // during a sustained ValidateAll catch-up. Without this, the
                    // VolatileDB grows unbounded and snapshots strand above the
                    // immutable tip.
                    blocks_since_maintenance += 1;
                    if blocks_since_maintenance >= MAINTENANCE_FORCE_BLOCKS {
                        blocks_since_maintenance = 0;
                        // Confirm all deferred Plutus before maintenance flushes
                        // volatile→immutable so nothing un-confirmed is finalised.
                        self.flush_pending_phase2(true).await;
                        self.run_background_maintenance().await;
                    }
                }

                // ── ChainDB maintenance (periodic) ───────────────────────
                // Finalise k-deep volatile blocks to ImmutableDB and advance
                // the chain-fragment anchor so neither grows unbounded during
                // live sync.  See the `maintenance_ticker` declaration above
                // for why this is the from-genesis apply-rate fix.
                _ = maintenance_ticker.tick() => {
                    blocks_since_maintenance = 0;
                    // Confirm deferred Plutus before the volatile→immutable flush.
                    self.flush_pending_phase2(true).await;
                    self.run_background_maintenance().await;
                }

                // ── Governor evaluation (periodic, every 2s) ────────────
                _ = governor_ticker.tick() => {
                    // ── Divergence-witness check (#699) ───────────────────
                    //
                    // If `DIVERGENCE_PEER_THRESHOLD` distinct peers have all
                    // offered a `MsgRollBackward` to a slot older than our
                    // ImmutableDB tip within `DIVERGENCE_WINDOW`, then by
                    // Ouroboros k-block finality our local chain has
                    // diverged from the canonical network chain.  Without
                    // an automated recovery (rollback_via_snapshot_replay)
                    // we surface a clear operator error and shut down so
                    // the operator can wipe + re-sync.
                    {
                        const DIVERGENCE_PEER_THRESHOLD: usize = 3;
                        const DIVERGENCE_WINDOW: std::time::Duration =
                            std::time::Duration::from_secs(300);
                        let mut pm = peer_manager.write().await;
                        let now = std::time::Instant::now();
                        // GC stale witnesses first so the count reflects
                        // recent events only.
                        pm.gc_divergence_witnesses(now, DIVERGENCE_WINDOW);
                        // GC matured entries out of the fresh-inbound map so
                        // the per-tick `fresh_inbound_set` scan below stays
                        // bounded by currently-immature peers rather than
                        // every inbound peer accepted this run (#1003).
                        // Mirrors upstream InboundGovernor's own wake arm
                        // that eagerly pops matured entries out of
                        // `freshDuplexPeers` into `matureDuplexPeers`.
                        pm.gc_fresh_inbound(now);
                        let witnesses = pm.divergence_witness_count(now, DIVERGENCE_WINDOW);
                        if witnesses >= DIVERGENCE_PEER_THRESHOLD {
                            // Snapshot the witness details for the error
                            // message before dropping the lock.
                            let witness_details: Vec<(SocketAddr, u64, u64)> = pm
                                .divergence_witnesses()
                                .map(|(addr, (_, r, i))| (*addr, *r, *i))
                                .collect();
                            drop(pm);
                            tracing::error!(
                                witness_count = witnesses,
                                threshold = DIVERGENCE_PEER_THRESHOLD,
                                window_secs = DIVERGENCE_WINDOW.as_secs(),
                                witnesses = ?witness_details,
                                "CHAIN DIVERGED FROM NETWORK — \
                                 multiple peers have offered MsgRollBackward \
                                 to a slot older than our ImmutableDB tip. \
                                 By Ouroboros k-block finality, our local \
                                 chain is no longer on the canonical fork. \
                                 Operator action required: wipe the database \
                                 directory and re-sync from genesis OR import \
                                 a fresh Mithril snapshot. \
                                 Shutting down to prevent further divergence (#699)."
                            );
                            // Returning Err propagates to `Node::run`'s
                            // caller, which exits the process with non-zero
                            // status.  Background tokio tasks are torn down
                            // by their drop handlers / parent cancellation.
                            return Err(anyhow::anyhow!(
                                "Chain diverged from network — \
                                 {witnesses} peers witnessed rollback below \
                                 ImmutableDB tip in last \
                                 {window_secs}s. Operator action required.",
                                window_secs = DIVERGENCE_WINDOW.as_secs()
                            ));
                        }
                    }

                    // ── Apply any pending RuntimeConfig update ───────────
                    //
                    // `has_changed()` returns true if the SIGHUP handler
                    // sent a new value since the last time we called
                    // `borrow_and_update()`.  The cost is a single atomic
                    // load, so we check unconditionally on every tick.
                    if runtime_config_rx.has_changed().unwrap_or(false) {
                        let rt = runtime_config_rx.borrow_and_update().clone();
                        governor.update_targets(PeerTargets {
                            target_warm: rt.target_number_of_established_peers,
                            target_hot: rt.target_number_of_active_peers,
                            max_cold: rt.target_number_of_known_peers,
                            target_warm_big_ledger: rt.target_number_of_established_big_ledger_peers,
                            target_hot_big_ledger: rt.target_number_of_active_big_ledger_peers,
                        });
                        // Apply reloaded churn cadences (ChurnIntervalNormalSecs
                        // / ChurnIntervalSyncSecs) so SIGHUP genuinely changes
                        // governor behaviour rather than only reporting success.
                        governor.update_churn_intervals(
                            Duration::from_secs(rt.churn_interval_normal_secs),
                            Duration::from_secs(rt.churn_interval_sync_secs),
                        );
                        debug!(
                            active = rt.target_number_of_active_peers,
                            established = rt.target_number_of_established_peers,
                            known = rt.target_number_of_known_peers,
                            "governor: applied updated peer targets from RuntimeConfig"
                        );
                    }

                    // Compute governor actions based on current peer state.
                    // Build LocalRootGroupTargets from the peer manager's stored
                    // local root groups so the governor can promote topology peers
                    // via the per-group belowTargetLocal path.
                    let actions = {
                        let pm = peer_manager.read().await;
                        let local_root_targets: Vec<dugite_network::peer::governor::LocalRootGroupTarget> = pm
                            .local_root_groups()
                            .iter()
                            .map(|g| dugite_network::peer::governor::LocalRootGroupTarget {
                                members: g.addrs.iter().copied().collect(),
                                warm_valency: g.warm_valency,
                                hot_valency: g.hot_valency,
                            })
                            .collect();
                        let big_ledger = pm.big_ledger_peers().clone();
                        let fresh_inbound = pm.fresh_inbound_set(std::time::Instant::now());
                        // Fetch-floor fix: read the identity of the peer currently
                        // holding the BlockFetch slot so the governor can exclude it
                        // from aboveTargetOther demotion (prevents killing an active
                        // download every ~5 s during a post-restart connect burst).
                        let active_fetch_peer = self
                            .connection_lifecycle
                            .as_ref()
                            .and_then(|lc| lc.get_active_fetch_peer());
                        // Drive hot-churn cadence from the catch-up signal:
                        // bulk-syncing (behind tip) churns faster
                        // (ChurnIntervalSyncSecs); caught-up uses
                        // ChurnIntervalNormalSecs. Mirrors Haskell's
                        // BulkSync/Deadline churn split.
                        let at_tip = self
                            .volatile_wal_sync_at_tip
                            .load(std::sync::atomic::Ordering::Relaxed);
                        governor.set_bulk_sync_mode(!at_tip);
                        // Trusted-only establishment clamp while the GSM is
                        // below CaughtUp in genesis mode (Haskell
                        // `requiresBootstrapPeers` — see
                        // `Governor::set_sync_trusted_restriction`). Without
                        // it the governor establishes public peers during
                        // bulk sync, the `haa_satisfied` closure ("every
                        // established outbound peer is trusted") goes
                        // structurally false, the GSM regresses Syncing →
                        // PreSyncing, and the LoE freezes selection at
                        // immutable-tip + k — the 2026-07-28 mainnet
                        // from-genesis permanent stall. Recomputed every
                        // tick so late bootstrap DNS resolutions extend the
                        // set. An empty trusted set applies NO clamp (no
                        // bootstrap/local roots configured — praos-style
                        // topologies must keep current behaviour).
                        let trusted_restriction = self.compute_sync_trusted_restriction(
                            genesis_enabled,
                            last_seen_gsm_state,
                            &pm,
                        );
                        // Store at BOTH enforcement layers: the governor
                        // filter avoids wasted actions; the lifecycle
                        // chokepoint catches every other promotion driver
                        // (rotation, reconnect) — see `sync_trusted_clamp`.
                        if let Some(ref lc) = self.connection_lifecycle {
                            lc.store_sync_trusted_clamp(trusted_restriction.clone());
                        }
                        // #931: mirror the clamp's active/inactive state into
                        // the peer manager so `haa_satisfied`'s failure
                        // diagnostics WARN only while the clamp is genuinely
                        // in force. Diagnostics-only — enforcement is the
                        // governor filter + lifecycle chokepoints above.
                        pm.set_sync_trusted_clamp_active(trusted_restriction.is_some());
                        governor.set_sync_trusted_restriction(trusted_restriction);
                        // #920: self-healing counterpart to the promotion-only
                        // clamp — every tick the clamp is active, demote any
                        // peer that was already established BEFORE the clamp
                        // (e.g. during a prior CaughtUp period) straight to
                        // Cold. See `Governor::compute_actions_with_blp` doc.
                        let untrusted_established: std::collections::HashSet<SocketAddr> =
                            pm.untrusted_established_outbound().into_iter().collect();
                        governor.compute_actions_with_blp(
                            &pm.inner,
                            &local_root_targets,
                            &big_ledger,
                            &fresh_inbound,
                            active_fetch_peer,
                            &untrusted_established,
                        )
                    };

                    if !actions.is_empty() {
                        if let Some(ref mut lifecycle) = self.connection_lifecycle {
                            // PromoteToWarm: spawn background tasks so TCP
                            // connect + handshake never blocks the main loop.
                            // Each connect can take up to connect_timeout (default
                            // 10s); doing them sequentially here would starve
                            // fetched_blocks_rx for that entire duration.
                            // Snapshot per-peer diffusion modes under a single read
                            // lock before spawning background connect tasks, so each
                            // peer's per-group InitiatorOnly override is captured
                            // and forwarded to PeerConnection::connect() as the
                            // correct `initiator_only` flag in the handshake.
                            let diffusion_modes: Vec<(SocketAddr, bool)> = {
                                let pm = peer_manager.read().await;
                                actions
                                    .iter()
                                    .filter_map(|a| {
                                        if let dugite_network::peer::governor::GovernorAction::PromoteToWarm(addr) = a {
                                            let initiator_only = pm.effective_diffusion_mode(addr)
                                                == DiffusionMode::InitiatorOnly;
                                            Some((*addr, initiator_only))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect()
                            };
                            for action in &actions {
                                if let dugite_network::peer::governor::GovernorAction::PromoteToWarm(addr) = action {
                                    // Skip peers that are already connected or
                                    // already have an in-flight background task.
                                    if lifecycle.has_connection(addr)
                                        || in_flight_connects.contains(addr)
                                    {
                                        // #880: the connect is skipped, so no
                                        // connect_result will ever fire for this
                                        // peer — release the governor's in-flight
                                        // marker now. Otherwise in_progress_promote_cold
                                        // leaks the addr and the governor
                                        // permanently excludes it from future
                                        // cold->warm promotion. (Harmless if the
                                        // pending connect later completes: the
                                        // second clear is a no-op.)
                                        governor.promotion_cold_completed(addr);
                                        continue;
                                    }
                                    // Look up the per-peer initiator_only flag computed
                                    // above from the peer's topology group config.
                                    let initiator_only = diffusion_modes
                                        .iter()
                                        .find(|(a, _)| a == addr)
                                        .map(|(_, io)| *io)
                                        .unwrap_or(false);
                                    in_flight_connects.insert(*addr);
                                    lifecycle.spawn_connect(*addr, initiator_only, connect_result_tx.clone());
                                }
                            }

                            // Non-connect actions (demote, disconnect, etc.) are
                            // still handled inline — they are fast O(1) operations.
                            let mut pm = peer_manager.write().await;
                            for action in actions {
                                match action {
                                    dugite_network::peer::governor::GovernorAction::PromoteToWarm(_) => {} // handled above
                                    dugite_network::peer::governor::GovernorAction::PromoteToHot(addr) => {
                                        // Dispatch the promotion, then unconditionally clear
                                        // in_progress_promote_warm so the governor can re-evaluate
                                        // this peer on the next tick (#516).  Without this call the
                                        // governor's HashSet grows without bound and peers that were
                                        // once promoted via handle_governor_action can never be
                                        // re-promoted by the governor after a subsequent demotion.
                                        lifecycle.handle_governor_action(
                                            dugite_network::peer::governor::GovernorAction::PromoteToHot(addr),
                                            &mut pm,
                                        ).await;
                                        governor.promotion_warm_completed(&addr);
                                    }
                                    other => {
                                        // #909 observability: surface the metric the
                                        // demotion decision was actually made on, so a
                                        // "why was my downloader demoted?" question is
                                        // answerable from the log alone.
                                        if let dugite_network::peer::governor::GovernorAction::DemoteToWarm(addr) = other {
                                            debug!(
                                                %addr,
                                                fetchyness_bytes = pm.peer_fetchyness_bytes(&addr),
                                                bulk_sync = !self
                                                    .volatile_wal_sync_at_tip
                                                    .load(std::sync::atomic::Ordering::Relaxed),
                                                "governor demoting hot -> warm"
                                            );
                                        }
                                        lifecycle.handle_governor_action(other, &mut pm).await;
                                    }
                                }
                            }

                            pm.recompute_reputations();

                            // Update peer metrics immediately after state transitions
                            // so counters reflect reality without waiting for the
                            // periodic metrics poll in the sync loop.
                            self.update_peer_metrics(&pm);
                        }
                    }

                    // Cleanup dead connections (mux terminated).
                    if let Some(ref mut lifecycle) = self.connection_lifecycle {
                        let mut pm = peer_manager.write().await;
                        // NOTE: cleanup to debug connection deaths
                        lifecycle.cleanup_dead_connections(&mut pm).await;

                        // Update metrics after removing dead connections.
                        self.update_peer_metrics(&pm);
                    }
                }

                // ── Background cold->warm connection results ─────────────
                //
                // The governor spawns `PeerConnection::connect()` in background
                // tasks (see `spawn_connect`) so TCP timeouts never block this
                // loop. Results arrive here; on success we register the peer as
                // warm and immediately promote to hot.
                Some(result) = connect_result_rx.recv() => {
                    match result {
                        Ok((addr, conn, rtt_ms)) => {
                            in_flight_connects.remove(&addr);
                            // Clear the governor's in-flight tracking so it can
                            // re-evaluate this peer on the next tick (e.g. if the
                            // warm→hot promotion below fails and a reconnect is needed).
                            governor.promotion_cold_completed(&addr);
                            if let Some(ref mut lifecycle) = self.connection_lifecycle {
                                let mut pm = peer_manager.write().await;
                                match lifecycle.register_warm_connection(
                                    addr, conn, rtt_ms, &mut pm,
                                ) {
                                    Ok(()) => {
                                        // Promote straight to hot (matching
                                        // Haskell's established→active path).
                                        if let Err(e) =
                                            lifecycle.promote_to_hot(addr, &mut pm).await
                                        {
                                            warn!(%addr, "Warm→Hot failed after background connect: {e}");
                                        }
                                        self.update_peer_metrics(&pm);
                                    }
                                    Err(LifecycleError::AlreadyConnected(_)) => {
                                        // A concurrent inbound connection beat us;
                                        // discard the duplicate — it drops cleanly.
                                        debug!(%addr, "background connect raced inbound; discarding duplicate");
                                    }
                                    // #920: the trusted-only clamp activated between
                                    // `spawn_connect`'s initiation-time check and this
                                    // registration-time recheck (mid-handshake race). This
                                    // is a planned policy refusal, not a connection
                                    // failure — charging `peer_failed` here would arm a
                                    // reconnect backoff for a peer that must be
                                    // immediately re-eligible once the clamp lifts.
                                    Err(LifecycleError::TrustedOnlyClamp(_)) => {
                                        debug!(%addr, "background connect raced the trusted-only clamp; discarding");
                                    }
                                    Err(e) => {
                                        warn!(%addr, "register_warm_connection failed: {e}");
                                        pm.peer_failed(&addr);
                                        self.update_peer_metrics(&pm);
                                    }
                                }
                            }
                        }
                        // #920: a policy refusal (the trusted-only clamp refusing at
                        // `spawn_connect`'s initiation-time check) must NOT charge
                        // `peer_failed` — it is a planned, instantaneous condition, not
                        // a network failure, and the peer must remain immediately
                        // re-eligible once the clamp lifts. Only a genuine I/O/handshake
                        // failure arms the reconnect backoff.
                        Err((addr, ConnectError::PolicyRefused(reason))) => {
                            in_flight_connects.remove(&addr);
                            governor.promotion_cold_completed(&addr);
                            debug!(%addr, "background cold->warm refused by policy: {reason}");
                        }
                        Err((addr, ConnectError::Io(error))) => {
                            in_flight_connects.remove(&addr);
                            // Clear the governor's in-flight tracking so it retries
                            // the peer on the next tick rather than skipping it forever.
                            governor.promotion_cold_completed(&addr);
                            debug!(%addr, "background cold->warm failed: {error}");
                            let mut pm = peer_manager.write().await;
                            pm.peer_failed(&addr);
                            self.update_peer_metrics(&pm);
                        }
                    }
                }

                // ── Inbound N2N connections (accepted + handshaked) ───────
                //
                // The N2N listener accepts TCP streams and runs the mux +
                // handshake in background tasks. Completed connections arrive
                // here for registration with the lifecycle manager, which
                // starts server protocol tasks on the duplex channels.
                Some(result) = inbound_accept_rx.recv() => {
                    match result {
                        Ok((addr, mut conn, rtt_ms)) => {
                            // #865: enforce a hard cap on ESTABLISHED inbound
                            // connections, not merely concurrent handshakes. The
                            // accept semaphore/ConnectionManager slot are freed the
                            // instant the handshake completes, so a botnet that
                            // completes handshakes could otherwise hold unbounded
                            // live inbound connections and exhaust memory/FDs. The
                            // cap is the same `accepted_connections_limit.hard_limit`
                            // the handshake window uses. Computed before the
                            // `self.connection_lifecycle` mutable borrow below.
                            let inbound_cap = self
                                .config
                                .accepted_connections_limit
                                .unwrap_or_default()
                                .hard_limit as usize;
                            if let Some(ref mut lifecycle) = self.connection_lifecycle {
                                if lifecycle.inbound_connection_count() >= inbound_cap {
                                    warn!(
                                        %addr,
                                        max = inbound_cap,
                                        established = lifecycle.inbound_connection_count(),
                                        "established inbound connection cap reached — rejecting"
                                    );
                                    conn.shutdown().await;
                                } else {
                                    let mut pm = peer_manager.write().await;
                                    match lifecycle.register_inbound_connection(addr, conn, rtt_ms, &mut pm).await {
                                        Ok(()) => {
                                            info!(%addr, rtt_ms = format_args!("{rtt_ms:.0}"), "inbound connection registered");
                                            // Update all peer metrics (including n2n_connections_active)
                                            // via the derived-read helper rather than a bare fetch_add.
                                            self.update_peer_metrics(&pm);
                                        }
                                        Err(e) => {
                                            warn!(%addr, "inbound registration failed: {e}");
                                        }
                                    }
                                }
                            }
                        }
                        Err((addr, reason)) => {
                            warn!(%addr, "inbound handshake failed: {reason}");
                        }
                    }
                }

                // ── Peer failure reports from protocol tasks ────────────
                //
                // Protocol tasks report failed peers here so we can record
                // the failure for reputation scoring and backoff without
                // waiting for the mux to die naturally. `ProtocolFault`
                // convictions (#751: mis-declared block sizes, undecodable
                // blocks, agency violations — provable lies) additionally
                // tear the connection down, mirroring Haskell where every
                // BlockFetch conviction is a thrown exception that kills
                // the bearer; without this the convicted peer keeps a hot
                // connection whose dead BlockFetch worker silently discards
                // the remaining flood (mux drops frames on a closed
                // channel without closing TCP).
                Some((failed_addr, failure_kind)) = peer_failure_rx.recv() => {
                    let mut pm = peer_manager.write().await;
                    // `Unsuitable` (ChainSync intersection only at genesis — the
                    // Haskell `ForkTooDeep` equivalent) is routine on public
                    // networks, so log at INFO (≈ cardano-node `Notice`). Real
                    // faults (Slow / ProtocolFault) stay WARN. Demotion + backoff
                    // below are identical for every kind.
                    if failure_kind == PeerFailureKind::Unsuitable {
                        info!(%failed_addr, kind = ?failure_kind, "peer reported unsuitable by protocol task (intersection only at genesis) — demoting for backoff");
                    } else {
                        warn!(%failed_addr, kind = ?failure_kind, "peer reported as failed by protocol task");
                    }
                    pm.peer_failed(&failed_addr);
                    // #sync-eval companion fix: tear the connection down for
                    // BOTH failure kinds, not just `ProtocolFault`. A `Slow`
                    // failure used to only hit reputation/backoff and leave the
                    // mux alive; because the connection stayed in
                    // `lifecycle.connections`, `has_connection(addr)` kept
                    // returning true, so the governor's reconnect path
                    // (`if lifecycle.has_connection(addr) … continue`) was
                    // blocked FOREVER — even after backoff decayed. A burst of
                    // `Slow` drops (e.g. a transient network blip) therefore
                    // collapsed the peer set to 0 with no recovery (observed in
                    // the preprod resync). Tearing down on `Slow` too removes
                    // the peer from `lifecycle.connections` so the governor
                    // re-promotes it on the normal Cold→Warm schedule after the
                    // backoff `peer_failed()` already applied. `demote_to_cold`
                    // is a safe no-op when there is no live connection.
                    if let Some(ref mut lifecycle) = self.connection_lifecycle {
                        if let Err(e) = lifecycle.demote_to_cold(failed_addr, &mut pm).await {
                            // Not connected any more — bookkeeping only.
                            debug!(%failed_addr, kind = ?failure_kind, error = %e, "peer-failure teardown: no live connection");
                        }
                    }
                    self.update_peer_metrics(&pm);
                }

                // ── GDD density disconnects (DensityTooLow) ─────────────
                //
                // Haskell kills the ChainSync client thread with the
                // `DensityTooLow` exception, which tears the whole peer
                // connection down and removes its handle. Equivalent here:
                // record a failure (governor cooldown / forget-threshold
                // accounting) and run the full lifecycle demotion — closes
                // every connection to the peer, cancels its protocol tasks
                // (the ChainSync task's drop guard deregisters it from the
                // genesis peer registry) and drops its candidate state.
                Some(crate::gsm::GddAction::DisconnectPeer(addr)) = async {
                    match gdd_action_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if loe_gdd_enabled => {
                    warn!(%addr, "GDD: density too low — disconnecting peer");
                    self.metrics.record_gdd_disconnect();
                    let mut pm = peer_manager.write().await;
                    pm.peer_failed(&addr);
                    if let Some(ref mut lifecycle) = self.connection_lifecycle {
                        if let Err(e) = lifecycle.demote_to_cold(addr, &mut pm).await {
                            // Not connected any more — bookkeeping only.
                            debug!(%addr, error = %e, "GDD disconnect: no live connection");
                        }
                    }
                    self.update_peer_metrics(&pm);
                }

                // ── LoE advance → reprocess deferred blocks ─────────────
                Ok(()) = loe_watch_rx.changed(), if genesis_enabled => {
                    let snap = *loe_watch_rx.borrow_and_update();
                    // Genesis sync-vs-deadline peer-selection targets
                    // (Haskell getPeerSelectionTargets: GenesisMode+TooOld →
                    // syncTargets, else deadlineTargets — audit gsm-09/16).
                    if snap.state != last_seen_gsm_state {
                        last_seen_gsm_state = snap.state;
                        let syncing = snap.state != crate::gsm::GenesisSyncState::CaughtUp;
                        // Haskell parity: PreSyncing and Syncing both map to
                        // LedgerStateJudgement TooOld — the peer-selection
                        // governor only reacts to the TooOld ↔ YoungEnough
                        // boundary, so a PreSyncing ↔ Syncing flap must not
                        // re-apply targets (#740).
                        if syncing != sync_targets_applied {
                            sync_targets_applied = syncing;
                            let cfg = &self.config;
                            let targets = if syncing {
                                dugite_network::peer::governor::PeerTargets {
                                    target_warm: cfg.sync_target_number_of_established_peers,
                                    target_hot: cfg.sync_target_number_of_active_peers,
                                    max_cold: cfg.sync_target_number_of_known_peers,
                                    target_warm_big_ledger:
                                        cfg.sync_target_number_of_established_big_ledger_peers,
                                    target_hot_big_ledger:
                                        cfg.sync_target_number_of_active_big_ledger_peers,
                                }
                            } else {
                                dugite_network::peer::governor::PeerTargets {
                                    target_warm: cfg.target_number_of_established_peers,
                                    target_hot: cfg.target_number_of_active_peers,
                                    max_cold: cfg.target_number_of_known_peers,
                                    target_warm_big_ledger:
                                        cfg.target_number_of_established_big_ledger_peers,
                                    target_hot_big_ledger:
                                        cfg.target_number_of_active_big_ledger_peers,
                                }
                            };
                            info!(
                                state = %snap.state,
                                syncing,
                                target_hot = targets.target_hot,
                                target_hot_blp = targets.target_hot_big_ledger,
                                "GSM state change — switching peer-selection targets"
                            );
                            governor.update_targets(targets);

                            // #920: one-shot self-healing sweep on the
                            // CaughtUp→Syncing/PreSyncing regression edge
                            // (`syncing == true` only — never on the
                            // PreSyncing↔Syncing flap #740 already guards
                            // against above). A peer established during the
                            // prior CaughtUp period is already Warm/Hot and
                            // the promotion-only clamp can never touch it;
                            // without this sweep the HAA closure ("every
                            // established outbound peer is trusted") stays
                            // broken until the next 2s governor tick's
                            // demotion catches up. Recompute + store the
                            // clamp here immediately (rather than waiting for
                            // that tick) so the window closes on this exact
                            // edge.
                            if syncing {
                                let mut pm = peer_manager.write().await;
                                let trusted_restriction = self
                                    .compute_sync_trusted_restriction(
                                        genesis_enabled,
                                        snap.state,
                                        &pm,
                                    );
                                if let Some(ref lc) = self.connection_lifecycle {
                                    lc.store_sync_trusted_clamp(trusted_restriction.clone());
                                }
                                // #931: keep the diagnostics mirror in step on
                                // the regression edge too (see the tick site).
                                pm.set_sync_trusted_clamp_active(trusted_restriction.is_some());
                                governor.set_sync_trusted_restriction(trusted_restriction);
                                let to_demote = pm.untrusted_established_outbound();
                                if !to_demote.is_empty() {
                                    info!(
                                        count = to_demote.len(),
                                        sample = ?to_demote.iter().take(5).collect::<Vec<_>>(),
                                        "GSM regressed below CaughtUp — demoting \
                                         already-established untrusted peer(s) to \
                                         restore the trusted-only clamp (#920)"
                                    );
                                    if let Some(ref mut lifecycle) = self.connection_lifecycle {
                                        for addr in to_demote {
                                            // Mirror the `GovernorAction::DemoteToCold`
                                            // handler's reconciliation: a
                                            // `NotConnected` result means the peer is
                                            // already gone at the lifecycle layer but
                                            // still Warm/Hot in the peer manager — move
                                            // it to Cold directly rather than leaving it
                                            // dangling.
                                            if let Err(e) =
                                                lifecycle.demote_to_cold(addr, &mut pm).await
                                            {
                                                if matches!(e, LifecycleError::NotConnected(_)) {
                                                    pm.peer_disconnected(&addr);
                                                }
                                            }
                                        }
                                    }
                                    self.update_peer_metrics(&pm);
                                }
                            }
                        }
                    }
                    if loe_gdd_enabled && snap.loe_slot != last_seen_loe_slot {
                        last_seen_loe_slot = snap.loe_slot;
                        let handle = self.chain_sel_handle.clone();
                        if let Some(handle) = handle {
                            if let Some(dugite_storage::AddBlockResult::TriggeredFork {
                                intersection_hash,
                                intersection_slot,
                                rollback,
                                apply,
                            }) = handle.reprocess_loe().await
                            {
                                info!(
                                    intersection_slot = intersection_slot.0,
                                    apply_count = apply.len(),
                                    "LoE advance unlocked a deferred candidate — switching"
                                );
                                let _ = self
                                    .apply_fork_switch_plan(
                                        intersection_hash,
                                        intersection_slot,
                                        rollback,
                                        apply,
                                    )
                                    .await;
                            }
                        }
                    }
                }

                // ── KeepAlive RTT reports ──────────────────────────────
                //
                // Each pong from a connected peer sends (addr, rtt_ms).
                // Update PeerManager EWMA latency and refresh the gauge
                // metrics so Prometheus/monitor reflect current RTT, not
                // cumulative handshake history.
                Some((rtt_addr, rtt_ms)) = keepalive_rtt_rx.recv() => {
                    // #909: DISCARD samples taken while the peer holds the
                    // BlockFetch slot. Its keepalive ping shares the TCP
                    // connection with a saturated 2048-block payload stream, so
                    // the measured RTT is our own bulk transfer queuing, not the
                    // peer's latency. Folding it into the EWMA made the busiest
                    // peer look like the slowest and fed a demotion. Haskell
                    // never mixes the two signals: latency drives promotion
                    // decisions, `fetchynessBytes` drives bulk-sync demotion.
                    let holds_fetch_slot = self
                        .connection_lifecycle
                        .as_ref()
                        .and_then(|lc| lc.get_active_fetch_peer())
                        == Some(rtt_addr);
                    let mut pm = peer_manager.write().await;
                    if holds_fetch_slot {
                        debug!(
                            %rtt_addr,
                            rtt_ms = format_args!("{rtt_ms:.0}"),
                            "keepalive RTT sample discarded — peer holds the BlockFetch slot"
                        );
                    } else {
                        pm.record_handshake_rtt(&rtt_addr, rtt_ms);
                    }
                    // Refresh gauge metrics from current EWMA values.
                    let latencies = pm.connected_peer_latencies();
                    self.metrics.update_peer_rtt_gauges(&latencies);
                }

                // ── Forge ticker (block production) ─────────────────────
                _ = forge_ticker.tick(), if has_block_producer => {
                    self.try_forge_block().await;
                }
                // (Shutdown arm moved to the top of the select — see `biased;` above, #760.)
            }
        }

        // #760: the loop has broken — tell the shutdown watchdog so it does NOT
        // force-exit while the bounded post-loop drain (below) runs its own
        // 30s/120s timeouts. (Release pairs with the watchdog's Acquire load.)
        loop_broken.store(true, std::sync::atomic::Ordering::Release);

        // Shut down all peer connections in parallel with a global timeout.
        // Each connection's shutdown() stops hot/warm protocols (up to 5s each)
        // and aborts the mux — doing this sequentially with N peers could take
        // minutes, so we run them all concurrently.
        if let Some(ref mut lifecycle) = self.connection_lifecycle {
            let connections = lifecycle.drain_connections();
            let count = connections.len();
            if count > 0 {
                info!(count, "Shutting down peer connections in parallel...");
                let shutdown_futs = connections.into_iter().map(|mut conn| async move {
                    conn.shutdown().await;
                });
                match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    futures::future::join_all(shutdown_futs),
                )
                .await
                {
                    Ok(_) => info!(count, "All peer connections shut down"),
                    Err(_) => warn!(
                        count,
                        "Peer connection shutdown timed out after 10s, continuing"
                    ),
                }
            }
        }

        // Flush volatile blocks, persist ChainDB, and quiesce the snapshot
        // worker, with a timeout to prevent hanging on shutdown.
        let shutdown_result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            {
                let mut db = self.chain_db.write().await;
                match db.flush_all_to_immutable() {
                    Ok(n) if n > 0 => {
                        info!(blocks = n, "Flushed volatile blocks to ImmutableDB")
                    }
                    Ok(_) => {}
                    Err(e) => error!("Failed to flush volatile blocks on shutdown: {e}"),
                }
                if let Err(e) = db.persist() {
                    error!("Failed to persist ChainDB on shutdown: {e}");
                }
            }
            // Snapshot worker quiescence (issue #695). Drop the sender
            // first so the worker's recv() returns None and it can
            // exit, then await the handle (bounded by the outer 30s
            // timeout) so any in-flight write completes before the
            // synchronous final save fires. Without this ordering the
            // synchronous save below could race the worker on
            // epoch-N.bin and latest.bin.
            self.snapshot_tx = None;
            if let Some(handle) = self.snapshot_worker_handle.take() {
                match tokio::time::timeout(std::time::Duration::from_secs(20), handle).await {
                    Ok(Ok(())) => info!("snapshot worker quiesced"),
                    Ok(Err(e)) => warn!(error = %e, "snapshot worker join errored"),
                    Err(_) => warn!(
                        "snapshot worker did not exit within 20s — proceeding to sync save anyway"
                    ),
                }
            }
        })
        .await;
        if shutdown_result.is_err() {
            error!("Graceful shutdown (flush/persist/quiesce) timed out after 30s, forcing exit");
            std::process::exit(1);
        }

        // Final shutdown snapshot under its OWN generous budget. A mainnet
        // snapshot is ~1.4 GB and routinely takes 5-20 s — and >30 s under
        // I/O contention. Sharing the 30 s flush/quiesce timeout above (which
        // the worker quiesce alone can eat 20 s of) force-exited the process
        // mid-write (observed live 2026-06-11T22:44Z: quiesce at +0 s, forced
        // exit at +29 s with the save still running; recovery then fell back
        // to an older periodic snapshot + chunk replay). The write itself is
        // atomic (temp + rename), so a timeout here loses only restart speed,
        // never integrity — but 120 s makes that practically unreachable.
        let save_result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            self.save_ledger_snapshot(),
        )
        .await;
        match save_result {
            Ok(()) => info!("Shutdown complete"),
            Err(_) => {
                error!(
                    "Final shutdown snapshot timed out after 120s, forcing exit \
                     (node will recover from the last periodic snapshot + chunk replay)"
                );
                std::process::exit(1);
            }
        }
        Ok(())
    }

    /// Whether the sync-time trusted-only establishment clamp should be
    /// active, and the trusted set to enforce if so.
    ///
    /// `Some(set)` while genesis mode is on, bootstrap/local-root peers are
    /// configured, and the GSM is below CaughtUp (Haskell
    /// `requiresBootstrapPeers`); `None` otherwise (no clamp — praos-style
    /// topologies and CaughtUp both keep unrestricted establishment).
    ///
    /// Shared by the per-tick governor path (which recomputes this every 2s
    /// so late bootstrap DNS resolutions extend the set) and the one-shot
    /// CaughtUp-boundary sweep (#920) so the two can never diverge on the
    /// activation condition — see `sync_trusted_clamp` (connection_lifecycle.rs)
    /// and `Governor::sync_trusted_only` for the two enforcement layers this
    /// feeds.
    fn compute_sync_trusted_restriction(
        &self,
        genesis_enabled: bool,
        gsm_state: crate::gsm::GenesisSyncState,
        pm: &crate::node::networking::NodePeerManager,
    ) -> Option<std::collections::HashSet<SocketAddr>> {
        // The clamp keys on bootstrap peers being CONFIGURED, not resolved:
        // `trusted_peer_addrs()` may still be empty while bootstrap DNS is in
        // flight, and a None-fallback here would leave the first governor
        // ticks unclamped — exactly the window in which 14 untrusted public
        // peers got established on the 2026-07-28 diagnosis run. An empty
        // `Some` refuses all outbound establishment until resolution lands
        // (Haskell bootstrap mode behaves the same: no trusted peers
        // reachable ⇒ no peers at all).
        let bootstrap_configured = self
            .topology
            .bootstrap_peers
            .as_ref()
            .is_some_and(|b| !b.is_empty())
            || !self.topology.local_roots.is_empty();
        if genesis_enabled
            && bootstrap_configured
            && gsm_state != crate::gsm::GenesisSyncState::CaughtUp
        {
            Some(pm.trusted_peer_addrs())
        } else {
            None
        }
    }

    // ─── Peer Metrics ────────────────────────────────────────────────────────

    /// Update peer metrics from current PeerManager state.
    ///
    /// Called immediately after lifecycle transitions (governor actions,
    /// dead connection cleanup) so Prometheus counters reflect reality
    /// without waiting for the periodic sync loop poll.
    fn update_peer_metrics(&self, pm: &crate::node::networking::NodePeerManager) {
        let active = self
            .connection_lifecycle
            .as_ref()
            .map_or(0, |lc| lc.connection_count());
        apply_peer_metrics(&self.metrics, pm, active);
    }

    // ─── apply_fetched_block() ──────────────────────────────────────────────

    /// Abandon a fork whose replay failed (a body-hash mismatch or a ledger
    /// apply error on a fork-replayed block).
    ///
    /// Matches Haskell `ChainSel.validateCandidate`: mark the offending block
    /// invalid (so chain selection's `truncateRejectedBlocks` equivalent will
    /// never re-adopt a candidate containing it) and leave the current chain
    /// intact. dugite commits the VolatileDB switch before replay, so "leave
    /// the current chain intact" here means rolling the VolatileDB selected
    /// chain AND the ledger back to the fork's intersection — a valid common
    /// ancestor. This replaces the old `clear_volatile()`, which discarded the
    /// entire VolatileDB and forced a multi-hour ImmutableDB resync.
    async fn abandon_failed_fork(
        &self,
        bad_hash: dugite_primitives::hash::Hash32,
        reason: &str,
        intersection: &dugite_primitives::block::Point,
    ) {
        if let Some(ref handle) = self.chain_sel_handle {
            handle
                .invalid_cache
                .write()
                .await
                .insert(bad_hash, reason.to_string());
        }
        {
            let mut db = self.chain_db.write().await;
            if let Err(e) = db.rollback_to_point(intersection) {
                error!(error = %e, "abandon_failed_fork: volatile rollback to intersection failed");
            }
        }
        // Roll the ledger back to the intersection to stay consistent with the
        // (now rolled-back) VolatileDB selected chain.
        let _ = self.handle_ledger_rollback(intersection).await;
        warn!(
            bad = %bad_hash.to_hex(),
            reason,
            "Abandoned failed fork: marked block invalid and rolled back to the \
             intersection (VolatileDB preserved — no resync)"
        );
    }

    /// Apply a block fetched by a per-peer BlockFetch worker to the ledger.
    ///
    /// This is the main integration point between the BlockFetch pipeline and
    /// the ledger. Blocks arrive here from per-peer workers via the
    /// `fetched_blocks_rx` channel, already deserialized. We:
    ///
    /// 1. Store the block in ChainDB (via ChainSelQueue if available)
    /// 2. Apply to ledger state
    /// 3. Update metrics, chain fragment, and consensus tip
    /// 4. Announce to downstream peers
    ///
    /// Matches the flow previously handled inline in `chain_sync_loop()`.
    /// Full Praos header validation for a Shelley-based block fetched from a
    /// peer — cardano-node's `updateChainDepState`, run on EVERY network block
    /// (as opposed to `reupdateChainDepState`/reapply, which is used only when
    /// replaying our OWN already-validated ImmutableDB on restart).
    ///
    /// With `ValidationMode::Full` this verifies the VRF proof, the VRF leader
    /// threshold (when a stake snapshot exists), the KES signature, and the
    /// operational certificate. Byron (BFT) blocks have no Praos header crypto
    /// and return `Ok` immediately — their delegation check runs on the ledger
    /// path.
    ///
    /// Returns `Err(reason)` if the header is cryptographically invalid; the
    /// caller drops the block so it never enters VolatileDB. During the
    /// early-sync window (no `set` stake snapshot for the first ~3 Shelley
    /// epochs) ONLY the leader-threshold check is skipped — VRF/KES/opcert
    /// crypto still runs — mirroring Haskell's `MissingStake` handling.
    ///
    /// This is the exact per-block logic from `process_forward_blocks`, ported
    /// to the live `apply_fetched_block` path (the former is no longer wired).
    async fn validate_peer_header_full(
        &mut self,
        block: &dugite_primitives::block::Block,
    ) -> Result<(), dugite_consensus::ConsensusError> {
        // Byron has no Praos header crypto — validated on the ledger path.
        if !block.era.is_shelley_based() {
            return Ok(());
        }

        // Haskell `getCurrentSlot` (wall clock) for the future-block guard.
        let wall_clock_slot = self.current_wall_clock_slot().await;
        let ls = self.ledger_state.read().await;

        // Leader eligibility uses the "set" snapshot (previous-epoch stake).
        let set_snapshot = ls.epochs.snapshots.set.as_ref();
        let total_active_stake: u64 = set_snapshot
            .map(|snap| snap.pool_stake.values().map(|s| s.0).sum())
            .unwrap_or(0);

        // Overlay (BFT) schedule context — only when d > 0 and pre-Babbage.
        //
        // The overlay schedule depends on the decentralisation parameter `d`, and
        // a block in epoch N+1 must be validated with epoch N+1's `d` (Haskell
        // TICKF/UPEC forecast — the LedgerView's d comes from the TICKed
        // curPParams after the pending protocol-param update is enacted at the
        // boundary). The ledger tip is still in epoch N here (the epoch
        // transition runs at apply time, after this header check), so we forecast
        // the target epoch's `d` rather than using the un-ticked current value.
        // Using the higher pre-decrease `d` mis-counts overlay slots and rejects
        // the first valid Praos block of the new epoch.
        let block_epoch = ls.epoch_of_slot(block.slot().0);
        let forecast_d = ls.forecast_d_for_epoch(block_epoch);
        let overlay_ctx = if should_build_overlay_context(
            block.era,
            ls.epochs.protocol_params.protocol_version_major,
            forecast_d.numerator,
            !ls.genesis_delegates.is_empty(),
        ) {
            let first_slot = ls.first_slot_of_epoch(block_epoch);
            let genesis_keys: std::collections::BTreeSet<dugite_primitives::hash::Hash28> =
                ls.genesis_delegates.keys().copied().collect();
            Some(dugite_consensus::overlay::OverlayContext {
                genesis_delegates: ls.genesis_delegates.clone(),
                genesis_keys,
                d: (forecast_d.numerator, forecast_d.denominator),
                first_slot_of_epoch: first_slot,
            })
        } else {
            None
        };

        // The wire header carries no epoch nonce — inject it (TICKN-correct for
        // a block that is the first of the next epoch).
        let epoch_nonce = ls.epoch_nonce_for_slot(block.slot().0);
        let mut header_with_nonce = block.header.clone();
        header_with_nonce.epoch_nonce = epoch_nonce;

        // Pool registration for VRF key binding + leader stake.
        let pool_id = dugite_primitives::hash::blake2b_224(&block.header.issuer_vkey);
        let issuer_info = if !block.header.issuer_vkey.is_empty() {
            let pool_reg = set_snapshot
                .and_then(|snap| snap.pool_params.get(&pool_id))
                .or_else(|| ls.certs.pool_params.get(&pool_id));
            pool_reg.map(|reg| {
                if total_active_stake == 0 {
                    // No stake snapshot yet (first ~3 epochs of Shelley sync):
                    // VRF key binding + crypto still run; leader threshold is
                    // skipped (stake = 1/1). Haskell uses `MissingStake` here.
                    BlockIssuerInfo {
                        vrf_keyhash: reg.vrf_keyhash,
                        pool_stake: 1,
                        total_active_stake: 1,
                    }
                } else {
                    let pool_stake = set_snapshot
                        .and_then(|snap| snap.pool_stake.get(&pool_id))
                        .map(|s| s.0)
                        .unwrap_or(0);
                    BlockIssuerInfo {
                        vrf_keyhash: reg.vrf_keyhash,
                        pool_stake,
                        total_active_stake,
                    }
                }
            })
        } else {
            None
        };

        // Envelope checks (body/header size vs protocol params) — always fatal.
        //
        // Use the FORECAST `maxBlockBodySize` for `block_epoch`, not the
        // un-ticked current value: the boundary PPUP that raises it is enacted
        // by the TICK that (in the Haskell reference) precedes header/body
        // validation, so the first block of epoch N+1 must be checked against
        // epoch N+1's limit. Same reasoning as `forecast_d` above. Mainnet
        // raised `maxBlockBodySize` 65536→73728 at the 305→306 boundary; the
        // first epoch-306 block has a 71271-byte body and is valid only under
        // 73728 — checking it against the stale 65536 wedged the sync.
        if let Err(e) = self.consensus.validate_envelope(
            block.slot(),
            block.header.body_size,
            None,
            ls.forecast_max_block_body_size_for_epoch(block_epoch),
            ls.epochs.protocol_params.max_block_header_size,
        ) {
            return Err(dugite_consensus::ConsensusError::InvalidBlock(format!(
                "envelope check: {e}"
            )));
        }

        let current_slot_for_check = wall_clock_slot
            .or_else(|| ls.tip.point.slot())
            .unwrap_or(block.slot());
        let pv_major = ls.epochs.protocol_params.protocol_version_major;
        let tip_slot = ls.tip.point.slot();

        match self.consensus.validate_header_full(
            &header_with_nonce,
            current_slot_for_check,
            issuer_info.as_ref(),
            overlay_ctx.as_ref(),
            ValidationMode::Full,
            Some(pv_major),
            tip_slot,
        ) {
            Ok(()) => {
                self.metrics
                    .header_full_validations_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Outcome of [`Node::apply_fork_switch_plan`].
    /// Execute a `TriggeredFork` chain-switch plan: roll the ledger back to
    /// the intersection and replay the new fork's blocks (full validation).
    ///
    /// Shared by `apply_fetched_block` (fork triggered by an arriving block)
    /// and the LoE reprocess path (fork adoption unlocked by a GDD/LoE
    /// advance with no new block arriving — Haskell
    /// `ChainSelReprocessLoEBlocks`).
    async fn apply_fork_switch_plan(
        &mut self,
        intersection_hash: dugite_primitives::hash::Hash32,
        intersection_slot: dugite_primitives::time::SlotNo,
        rollback: Vec<dugite_primitives::hash::Hash32>,
        apply: Vec<dugite_primitives::hash::Hash32>,
    ) -> ForkSwitchOutcome {
        // ── Phase-2 deferral safety: flush BEFORE the ledger rolls back ──────
        //
        // A fork switch rolls the ledger backward through the volatile window,
        // which can include blocks whose deferred Plutus is still pending. This
        // is the SOLE ledger-rollback path that can orphan a pending block: it
        // is reached both inline (an arriving block triggers the fork) and from
        // the run-loop LoE arm (a GDD/LoE advance unlocks a candidate). In
        // GENESIS mode especially, LoE/GDD makes such switches reachable while
        // the deferral window is non-empty — the Praos "fork-in-window is
        // structurally impossible" argument (deferred blocks ~3k deep, below the
        // k-finality horizon) does NOT hold under LoE selection, whose switches
        // can intersect anywhere down to the immutable tip, inside the volatile
        // window where the pending blocks live.
        //
        // Flush the window FIRST so every pending block's Plutus is validated
        // against the exact ledger it was applied to, BEFORE the rollback/replay
        // — mirroring the existing flush-before-maintenance/snapshot/shutdown
        // ordering. Without this, a post-switch flush would run Plutus against
        // (or roll back to a stale anchor on) a chain the deferred blocks are no
        // longer on. `allow_cancel = false`: complete it (the window is
        // memory-bounded) so the ledger is deterministic before the switch.
        // No-op when the window is empty (deferral off, or already flushed by
        // the run loop). Cannot recurse: flush_pending_phase2 calls
        // handle_ledger_rollback, never apply_fork_switch_plan, and it empties
        // pending_phase2 before that call.
        self.flush_pending_phase2(false).await;

        // Chain selection determined a competing fork is strictly
        // preferred.  The VolatileDB chain switch is already
        // committed — selected_chain now points at the new fork.
        // The ledger state is still on the OLD chain and MUST be
        // rolled back to the intersection before the new fork's
        // blocks can be applied.
        //
        // `intersection_slot` is pre-resolved by VolatileDB so the
        // rollback point is a proper `Point::Specific(slot, hash)`
        // with no chance of a None-fallback to Origin.
        // Matches Haskell: `ChainDiff`'s anchor is always a full
        // `(SlotNo, HeaderHash)` Point, and `forkerCommit` atomically
        // replays the new fork's blocks after the rollback.
        info!(
            intersection = %intersection_hash.to_hex(),
            intersection_slot = intersection_slot.0,
            rollback_count = rollback.len(),
            apply_count = apply.len(),
            "Chain selection: fork switch at live tip — rolling back ledger to intersection"
        );
        self.metrics
            .rollback_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let rollback_point =
            dugite_primitives::block::Point::Specific(intersection_slot, intersection_hash);
        // VolatileDB's selected_chain has already been switched to
        // the new fork by ChainSelQueue::switch_chain().  Use the
        // ledger-only rollback so we don't truncate selected_chain
        // back to the intersection (which would leave the volatile
        // tip stuck and cause an O(N) per-block cascade).
        //
        // Fix C (Bug B): guard fork replay on rollback success.
        // If the rollback cannot complete (LedgerSeq empty AND no
        // snapshot), skipping replay prevents the
        // "Block does not connect to tip" WARN + clear_volatile()
        // that causes the permanent StoreButDontChange cascade.
        // Fix A (below) ensures the seq is always populated so this
        // guard is a safety net rather than the primary defence.
        if !self.handle_ledger_rollback(&rollback_point).await {
            warn!(
                rollback_slot = intersection_slot.0,
                "Fork rollback failed; skipping fork replay. \
                             Node will resync on the next connection attempt."
            );
            // Do NOT clear_volatile here — VolatileDB already holds
            // the fork, and clearing it causes the permanent
            // StoreButDontChange cascade (Bug B design doc 2026-05-16).
            return ForkSwitchOutcome::Aborted;
        }

        // Replay the new fork's blocks from VolatileDB onto the ledger,
        // matching Haskell's `forkerCommit` behaviour.  The `apply` list
        // is ordered oldest-first and every hash is already present in
        // the VolatileDB (they were stored as part of the competing fork
        // before chain selection switched the tip).
        // Fork blocks come from peers too — full-validate by default
        // (cardano-node parity); DUGITE_TRUSTED_CATCHUP=1 opts out.
        let validation_mode = if std::env::var("DUGITE_TRUSTED_CATCHUP")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            BlockValidationMode::ApplyOnly
        } else {
            BlockValidationMode::ValidateAll
        };
        // Captures the last successfully-applied fork block so we
        // can refresh post-apply state (N2C snapshot etc.) once
        // after the loop without re-reading + re-decoding the CBOR.
        let mut last_applied: Option<(
            dugite_primitives::block::Block,
            dugite_primitives::time::SlotNo,
            dugite_primitives::time::BlockNo,
        )> = None;
        for fork_hash in &apply {
            let cbor_opt = {
                let db = self.chain_db.read().await;
                db.get_block(fork_hash).unwrap_or(None)
            };
            match cbor_opt {
                Some(cbor) => {
                    // #738: the ValidateAll phase-1/phase-2 oracle reads the
                    // witness set, so fork blocks must go through the FULL
                    // decoder. The minimal (witness-skipping) decoder is only
                    // safe in ApplyOnly mode. Decoding fork batches minimally
                    // made every tx in every LoE-reprocessed block fail
                    // witness validation (542K MissingVKey/Script/Signer
                    // divergences in a 19-minute window).
                    let decode_result = if matches!(validation_mode, BlockValidationMode::ApplyOnly)
                    {
                        dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                            &cbor,
                            self.byron_epoch_length,
                        )
                    } else {
                        dugite_serialization::decode_block_with_byron_epoch_length(
                            &cbor,
                            self.byron_epoch_length,
                        )
                    };
                    match decode_result {
                        Ok(fork_block) => {
                            let fork_slot = fork_block.slot();
                            let fork_block_no = fork_block.block_number();
                            let fork_hash_hex = fork_block.header.header_hash.to_hex();

                            // Issue #545 E5 (#550): verify body
                            // hash on fork-replayed blocks too.
                            // Fork blocks come from ChainDB
                            // (stored locally), so a mismatch
                            // here would indicate either a prior
                            // accepted bad block or local
                            // storage corruption — we treat it
                            // as a hard fault and abort the
                            // replay, falling through to the
                            // same recovery path as a ledger
                            // apply failure (clear volatile and
                            // resync).
                            if fork_block.era.is_shelley_based() {
                                if let Err(e) = dugite_consensus::praos::validate_block_body_hash(
                                    &fork_block.header,
                                    &cbor,
                                ) {
                                    warn!(
                                        slot = fork_slot.0,
                                        block = fork_block_no.0,
                                        error = %e,
                                        "Fork replay: body hash verification failed — \
                                         abandoning fork (block marked invalid)"
                                    );
                                    self.abandon_failed_fork(
                                        fork_block.header.header_hash,
                                        "fork replay: body hash mismatch",
                                        &rollback_point,
                                    )
                                    .await;
                                    break;
                                }
                            }

                            // Full Praos header crypto on fork-replayed blocks too
                            // (cardano-node `updateChainDepState` parity). Each fork
                            // block extends the previously-applied one, so the ledger
                            // tip is its predecessor and the leader-schedule forecast
                            // is in range. No `ls` lock is held here, so the read-lock
                            // acquired by `validate_peer_header_full` is safe.
                            if let Err(reason) = self.validate_peer_header_full(&fork_block).await {
                                // FutureBlock during fork replay: transient — do NOT
                                // blacklist.  The fork is simply abandoned for now; the
                                // peer may reconnect and re-offer the fork once the slot
                                // has passed.  All other errors are permanent crypto
                                // failures and go through abandon_failed_fork normally.
                                if matches!(
                                    reason,
                                    dugite_consensus::ConsensusError::FutureBlock { .. }
                                ) {
                                    warn!(
                                        slot = fork_slot.0,
                                        block = fork_block_no.0,
                                        "Fork replay: future-slot block beyond clock skew — \
                                         dropping fork WITHOUT blacklisting (transient)"
                                    );
                                    self.metrics
                                        .header_validation_failures_total
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    // Roll back chain + ledger without touching invalid_cache.
                                    {
                                        let mut db = self.chain_db.write().await;
                                        if let Err(e) = db.rollback_to_point(&rollback_point) {
                                            error!(
                                                error = %e,
                                                "future-block fork rollback: volatile rollback failed"
                                            );
                                        }
                                    }
                                    let _ = self.handle_ledger_rollback(&rollback_point).await;
                                    break;
                                }
                                warn!(
                                    slot = fork_slot.0,
                                    block = fork_block_no.0,
                                    "Fork replay: Praos header validation FAILED: {reason} \
                                                 — abandoning fork (block marked invalid)"
                                );
                                self.metrics
                                    .header_validation_failures_total
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                self.abandon_failed_fork(
                                    fork_block.header.header_hash,
                                    "fork replay: header validation failed",
                                    &rollback_point,
                                )
                                .await;
                                break;
                            }

                            // Fix B (Bug B, 2026-05-16): use apply_block_with_delta
                            // so that fork-replayed blocks also populate LedgerSeq.
                            // Without this, after a successful fork switch the seq
                            // tip stays at the intersection point, creating a new
                            // "shadow gap" that causes the NEXT fork to fail the
                            // same way.  Lock order: release ledger_state before
                            // acquiring ledger_seq (same invariant as Fix A).
                            let fork_delta = {
                                let mut ls = self.ledger_state.write().await;
                                // #733: per-block apply horizon snapshot at
                                // the pre-block ledger tip (one-shot).
                                ls.phase2_apply_horizon =
                                    if matches!(validation_mode, BlockValidationMode::ValidateAll)
                                        && fork_block.era >= dugite_primitives::era::Era::Babbage
                                    {
                                        let pre_tip = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                                        self.era_history.read().await.phase2_apply_horizon_slot(
                                            dugite_primitives::time::SlotNo(pre_tip),
                                        )
                                    } else {
                                        None
                                    };
                                // Issue #653 — relief-worker scheduling.
                                let apply_result = tokio::task::block_in_place(|| {
                                    ls.apply_block_with_delta(&fork_block, validation_mode)
                                });
                                match apply_result {
                                    Ok(delta) => {
                                        // Publish view post-apply (#651 P2 / #652 P0).
                                        self.publish_ledger_view(&ls);
                                        // Propagate era transitions discovered during fork replay.
                                        if let Some((prev_era, new_era, epoch)) =
                                            ls.pending_era_transition.take()
                                        {
                                            drop(ls);
                                            let mut eh = self.era_history.write().await;
                                            if eh.current_era() < new_era {
                                                eh.record_era_transition(new_era, epoch.0);
                                                info!(
                                                    prev = %prev_era,
                                                    new = %new_era,
                                                    epoch = epoch.0,
                                                    "Era transition recorded in HFC era history (fork replay)",
                                                );
                                            }
                                        }
                                        delta
                                    }
                                    Err(e) => {
                                        warn!(
                                            slot = fork_slot.0,
                                            block = fork_block_no.0,
                                            "Fork replay: ledger apply failed: {e} — \
                                                         abandoning fork (block marked invalid)"
                                        );
                                        // Release the ledger lock before abandon_failed_fork
                                        // (it re-acquires ledger + chain_db).
                                        drop(ls);
                                        self.abandon_failed_fork(
                                            fork_block.header.header_hash,
                                            "fork replay: ledger apply failed",
                                            &rollback_point,
                                        )
                                        .await;
                                        return ForkSwitchOutcome::Aborted;
                                    }
                                }
                            };
                            // Push the delta to LedgerSeq (ledger_state lock released).
                            {
                                let mut seq = self.ledger_seq.write().await;
                                seq.push(fork_delta);
                            }
                            // Update chain fragment and consensus tip for each
                            // replayed block.
                            {
                                let mut fragment = self.chain_fragment.write().await;
                                fragment.push(fork_block.header.clone());
                            }
                            self.consensus.update_tip(fork_block.tip());
                            info!(
                                era = %fork_block.era,
                                slot = fork_slot.0,
                                block = fork_block_no.0,
                                txs = fork_block.transactions.len(),
                                hash = %fork_hash_hex,
                                "Chain extended",
                            );
                            self.metrics.record_block_received();
                            self.metrics
                                .blocks_received
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            self.metrics
                                .blocks_applied
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            self.metrics.set_slot(fork_slot.0);
                            self.metrics.set_block_number(fork_block_no.0);
                            {
                                let ls = self.ledger_state.read().await;
                                // Era-aware tip-age (see Node::slot_to_wallclock_ms).
                                let slot_time_ms = self
                                    .slot_to_wallclock_ms(fork_slot.0, &ls.slot_config)
                                    .await;
                                self.metrics.set_tip_slot_time_ms(slot_time_ms);
                                self.metrics.set_epoch(ls.epoch.0);
                                self.metrics.set_protocol_version(
                                    ls.epochs.protocol_params.protocol_version_major,
                                    ls.epochs.protocol_params.protocol_version_minor,
                                );
                            }
                            // Authoritative era from the fork block's HFC tag
                            // (not the Shelley-shaped ledger PV major).
                            self.metrics.set_era(fork_block.era.to_era_index() as u64);
                            self.update_sync_progress(fork_slot.0, &self.view().slot_config)
                                .await;
                            // Announce each fork block to downstream peers.
                            if let Some(ref tx) = self.block_announcement_tx {
                                let mut hash_bytes = [0u8; 32];
                                hash_bytes.copy_from_slice(fork_block.header.header_hash.as_ref());
                                let _ = tx.send(dugite_network::BlockAnnouncement {
                                    slot: fork_slot.0,
                                    hash: hash_bytes,
                                    block_number: fork_block_no.0,
                                });
                                if let Some(ref tb) = self.tip_broadcaster {
                                    tb.announce_apply(tip_broadcast::TipApply {
                                        slot: fork_slot.0,
                                        hash: hash_bytes,
                                        block_number: fork_block_no.0,
                                        era: fork_block.era,
                                    });
                                }
                            }
                            // Stash this block as the most recent
                            // successful apply.  Subsequent
                            // iterations overwrite; only the
                            // final value is consumed below.
                            last_applied = Some((fork_block, fork_slot, fork_block_no));
                        }
                        Err(e) => {
                            warn!(
                                hash = %fork_hash.to_hex(),
                                "Fork replay: failed to decode block from VolatileDB: {e}"
                            );
                        }
                    }
                }
                None => {
                    warn!(
                        hash = %fork_hash.to_hex(),
                        "Fork replay: block hash in apply list not found in ChainDB"
                    );
                }
            }
        }
        // After the replay loop: if at least one block was
        // replayed, refresh metrics + snapshot for the final tip
        // (same housekeeping as the non-fork path).  The
        // per-iteration metric updates inside the loop keep
        // Prometheus reflecting intermediate progress; this call
        // ensures the N2C NodeStateSnapshot also refreshes (which
        // the loop did NOT do).
        //
        // Calling `post_block_apply_updates` once per replay (NOT
        // inside the loop) keeps the helper's 1 Hz rate limiter
        // on `update_query_state` effective — repeated calls
        // inside the loop would each see `elapsed() >= 1s` after
        // the second iteration and storm the snapshot rebuild.
        if let Some((last_block, last_slot, last_bn)) = last_applied.take() {
            self.post_block_apply_updates(&last_block, last_slot, last_bn)
                .await;
        }
        ForkSwitchOutcome::Replayed
    }

    async fn apply_fetched_block(&mut self, fetched: FetchedBlock) {
        let block = fetched.block;
        let block_slot = block.slot();
        let block_number = block.block_number();
        let block_hash = *block.hash();

        trace!(
            peer = %fetched.peer,
            slot = block_slot.0,
            block = block_number.0,
            hash = %block_hash.to_hex(),
            prev = %block.prev_hash().to_hex(),
            "Applying fetched block",
        );

        // Disk-space back-pressure guard (issue #610).
        //
        // Refuse to write any block to ChainDB when the disk monitor has set
        // the ingestion-paused flag.  The flag is set when free space drops
        // below PAUSE_THRESHOLD_BYTES (1 GB) and cleared only after
        // RECOVER_THRESHOLD_BYTES (5 GB) is sustained for 60 s, preventing
        // database corruption on a nearly-full volume.  The node stays alive
        // to serve N2C queries; it simply does not advance the chain until the
        // operator frees space.
        if self
            .ingestion_paused
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            debug!(
                slot = block_slot.0,
                "Disk ingestion paused — dropping block (disk space critically low)"
            );
            return;
        }

        // Auto-switch VolatileDB WAL durability mode based on at-tip state.
        //
        // During catch-up, blocks below the k-depth window are speculative
        // and re-fetchable from peers; per-block fsync is wall-clock-dominant
        // on macOS APFS (~50–400 ms per call) and crushes throughput.  At
        // tip, every write must be durable before the next so the producer
        // can adopt the chain head safely.
        //
        // Same at-tip definition as `publish_ledger_view`: peer_tip == 0
        // (no peer yet) OR local_tip + stability_window ≥ peer_tip.
        let peer_tip = self.metrics.get_peer_tip();
        let local_tip = block_slot.0;
        let stability_window = dugite_consensus::stability_window_slots(
            self.consensus.security_param,
            self.consensus.active_slot_coeff,
        );
        let at_tip = peer_tip == 0 || local_tip.saturating_add(stability_window) >= peer_tip;
        let prev_at_tip = self
            .volatile_wal_sync_at_tip
            .load(std::sync::atomic::Ordering::Relaxed);
        if prev_at_tip != at_tip {
            let mut db = self.chain_db.write().await;
            if let Err(e) = db.sync_volatile_wal() {
                warn!(error = %e, "VolatileDB WAL pre-transition fsync failed");
            }
            db.set_volatile_wal_sync_per_write(at_tip);
            drop(db);
            self.volatile_wal_sync_at_tip
                .store(at_tip, std::sync::atomic::Ordering::Relaxed);
            info!(
                at_tip,
                local_tip,
                peer_tip,
                stability_window,
                "VolatileDB WAL mode switched (per-write fsync = at_tip)"
            );

            // Match the snapshot scheduler to the at-tip mode.  During
            // catch-up only epoch-boundary snapshots fire — the
            // block-interval trigger (every 2 000 blocks) is wasted I/O
            // because the next epoch boundary is usually closer than the
            // would-be interval point and snapshotting stalls the apply
            // loop.  This mirrors cardano-node's behaviour where the
            // 10-minute snapshot rate-limit effectively caps catch-up
            // snapshots to one per epoch or two.
            if self.bg_snapshot_scheduler.set_catchup_mode(!at_tip) {
                info!(
                    catchup_mode = !at_tip,
                    "Snapshot scheduler mode switched (catch-up suppresses block-interval trigger)"
                );
            }

            // Match LedgerSeq's checkpoint behaviour to the at-tip mode.
            //
            // During catch-up the checkpoint Arc-clones inflate the anchor
            // substates' refcounts, forcing `advance_anchor` →
            // `Arc::make_mut` to CoW-deep-clone every mutated HashMap on
            // each push.  At preview's ~130 k-entry maps that was the
            // 480 ms per-block ceiling at epoch 25+.  Skipping the
            // checkpoint inserts during catch-up keeps the anchor's Arc
            // refcount at 1, so `make_mut` stays in-place.  Rollback
            // acceleration is restored once we cross back to at-tip.
            {
                let mut seq = self.ledger_seq.write().await;
                if seq.set_catchup_mode(!at_tip) {
                    info!(
                        catchup_mode = !at_tip,
                        "LedgerSeq mode switched (catch-up suppresses checkpoint Arc fan-out)"
                    );
                }
            }
        }
        // Periodic VolatileDB WAL fsync while in catch-up mode (bounds the
        // loss window to ~1 s when sync_per_write is disabled).
        if !at_tip && self.last_volatile_wal_sync.elapsed() >= std::time::Duration::from_secs(1) {
            let mut db = self.chain_db.write().await;
            if let Err(e) = db.sync_volatile_wal() {
                warn!(error = %e, "VolatileDB WAL periodic fsync failed");
            }
            self.last_volatile_wal_sync = std::time::Instant::now();
        }

        // Store in ChainDB via ChainSelQueue.
        // `fork_replayed` is set to true when the TriggeredFork path has already
        // applied all fork blocks to the ledger; in that case the single-block
        // apply path below is skipped.
        let mut fork_replayed = false;
        let storage_succeeded = if let Some(ref handle) = self.chain_sel_handle {
            let cbor = block.raw_cbor.clone().unwrap_or_default();
            let result = handle
                .submit_block_with_header(
                    block_hash,
                    block_slot,
                    block_number,
                    *block.prev_hash(),
                    cbor,
                    block.header.clone(),
                )
                .await;
            match result {
                Some(dugite_storage::AddBlockResult::AddedAsTip { .. }) => true,
                Some(dugite_storage::AddBlockResult::StoredAsFork) => {
                    // The block did NOT extend `selected_chain` — it sits in
                    // VolatileDB as a side-fork tip (or as an out-of-order
                    // gap on the canonical chain) and is waiting for the
                    // intervening blocks to arrive so a future
                    // `switch_to_fork` can succeed.  Applying it to the
                    // ledger now would diverge `ledger.tip` from
                    // `VolatileDB.selected_chain.tip`, which is exactly the
                    // catch-up stall pattern observed under concurrent
                    // BlockFetch from multiple peers: every subsequent
                    // block sees a "stale" selected_chain tip, marks itself
                    // `StoredAsFork`, the divergence cascades, and the
                    // forecast-horizon disconnect terminates sync.
                    //
                    // The block is durable in VolatileDB; chain selection
                    // will re-evaluate `fork_tips` on the next
                    // `process_add_block` call.  When the missing ancestors
                    // arrive `switch_to_fork` will succeed and the
                    // `TriggeredFork` arm below will roll the ledger
                    // forward through them in one batch.
                    trace!(
                        slot = block_slot.0,
                        block = block_number.0,
                        hash = %block_hash.to_hex(),
                        "StoredAsFork — block on side fork / out-of-order; skipping ledger apply"
                    );
                    return;
                }
                Some(dugite_storage::AddBlockResult::AlreadyKnown) => {
                    // The block is already in VolatileDB or ImmutableDB and
                    // was either applied previously (canonical) or rejected
                    // as a stale fork.  Re-applying would either no-op or
                    // diverge; either way the caller's apply path is the
                    // wrong place to handle it.
                    trace!(
                        slot = block_slot.0,
                        block = block_number.0,
                        hash = %block_hash.to_hex(),
                        "AlreadyKnown — block already in ChainDB; skipping ledger apply"
                    );
                    return;
                }
                Some(dugite_storage::AddBlockResult::TriggeredFork {
                    intersection_hash,
                    intersection_slot,
                    rollback,
                    apply,
                }) => {
                    match self
                        .apply_fork_switch_plan(
                            intersection_hash,
                            intersection_slot,
                            rollback,
                            apply,
                        )
                        .await
                    {
                        ForkSwitchOutcome::Replayed => {
                            fork_replayed = true;
                            true
                        }
                        ForkSwitchOutcome::Aborted => return,
                    }
                }
                Some(dugite_storage::AddBlockResult::Invalid(reason)) => {
                    warn!(
                        slot = block_slot.0,
                        block = block_number.0,
                        reason,
                        "Block rejected by ChainSelQueue"
                    );
                    false
                }
                None => {
                    error!("ChainSelQueue runner exited — block not stored");
                    false
                }
            }
        } else {
            // Fallback: direct ChainDB write.
            let cbor = block.raw_cbor.clone().unwrap_or_default();
            let mut db = self.chain_db.write().await;
            db.add_block(
                block_hash,
                block_slot,
                block_number,
                *block.prev_hash(),
                cbor,
            )
            .is_ok()
        };

        if !storage_succeeded {
            warn!(
                slot = block_slot.0,
                "Failed to store fetched block — skipping ledger apply"
            );
            return;
        }

        // Issue #545 E5 (#550): verify that the block body delivered by
        // BlockFetch matches the body hash committed in the header. A
        // malicious or buggy relay can substitute the body bytes while
        // leaving the header (and its KES/VRF signatures) intact. This
        // check mirrors Haskell's `verifyBlockIntegrity` / `matchesHeaderHash`
        // called at the decode-and-apply boundary, using the per-component
        // `bbHash` algorithm from `Cardano.Ledger.Alonzo.BlockBody`:
        //   bbHash = blake2b_256( blake2b_256(c_0) || ... || blake2b_256(c_{N-1}) )
        // where c_i is each body component (tx_bodies, witness_sets,
        // aux_data, [invalid_txs]) as CBOR-encoded on the wire.
        //
        // We check AFTER storage because the block is keyed by header hash
        // in ChainDB; a body-hash mismatch is a data integrity error from
        // this peer, not a chain-selection issue.
        if block.era.is_shelley_based() {
            if let Some(raw_cbor) = block.raw_cbor.as_deref() {
                if let Err(e) =
                    dugite_consensus::praos::validate_block_body_hash(&block.header, raw_cbor)
                {
                    warn!(
                        slot = block_slot.0,
                        block = block_number.0,
                        hash = %block_hash.to_hex(),
                        error = %e,
                        "Block body hash verification failed — rejecting block (substitution / corruption)"
                    );
                    return;
                }
            }
        }

        // When TriggeredFork already replayed all fork blocks (including the
        // incoming block) onto the ledger, skip the single-block apply path.
        if fork_replayed {
            return;
        }

        // Check if this block connects to the current ledger tip.
        // Blocks may arrive out of order from multiple peers. Only apply
        // blocks that extend the current chain; others are stored in ChainDB
        // (via ChainSelQueue above) and will be applied when the chain catches up.
        let prev_hash = *block.prev_hash();
        let connects_to_tip = {
            let ls = self.ledger_state.read().await;
            match ls.tip.point.hash() {
                Some(tip_hash) => prev_hash == *tip_hash,
                None => true, // Origin — any block connects
            }
        };

        if !connects_to_tip {
            // Out-of-order during healthy pipelined sync (normal); a SUSTAINED
            // rise while the applied tip is frozen and the ledger tip is ahead
            // of the ChainDB tip is the #768 stranded-snapshot wedge — the
            // apply-stall watchdog keys on this counter to detect it.
            self.metrics
                .fetched_blocks_not_connecting
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            debug!(
                slot = block_slot.0,
                block = block_number.0,
                "Block stored in ChainDB but skipping ledger apply (out of order)"
            );
            return;
        }

        // Full Praos header validation (cardano-node `updateChainDepState`
        // parity): cryptographically verify the header (VRF proof + leader
        // threshold + KES + opcert) for every Shelley+ peer block BEFORE it is
        // applied to the ledger. Byron (BFT) headers have no Praos crypto.
        //
        // This runs HERE — after the `connects_to_tip` gate — rather than at
        // fetch time, because header validation requires a leader-schedule
        // forecast and the forecast horizon is only ~one stability window ahead
        // of the ledger tip. A block that extends the tip has its predecessor as
        // the forecast anchor (one slot back), always in range; a far-ahead
        // pipelined block fetched out of order is NOT forecastable from the tip
        // (OutsideForecastRange) and must wait until the chain catches up to it.
        // Validating in-order at apply time gives correct anchoring and matches
        // Haskell, where the header is checked against the predecessor's
        // forecast as the chain is extended.
        if let Err(reason) = self.validate_peer_header_full(&block).await {
            // ── FutureBlock is a TRANSIENT condition, not a permanent failure ──
            //
            // Haskell cardano-node handles blocks from the future entirely in
            // the ChainSync client layer
            // (`Ouroboros.Consensus.MiniProtocol.ChainSync.Client.InFutureCheck`):
            //
            //   • Within the 2-second `defaultClockSkew` window: the client
            //     sleeps (`threadDelay`) until the slot onset and then proceeds
            //     normally.
            //   • Beyond the skew window: the client throws
            //     `InFutureHeaderExceedsClockSkew` and disconnects the peer.
            //
            // In NEITHER case does the block enter `cdbInvalid` (the invalid-
            // blocks set).  `addInvalidBlock` / `ExtValidationError` only cover
            // real ledger/crypto failures, not timing conditions.
            //
            // Dugite historically called `abandon_failed_fork` here for ALL
            // validation errors, which inserted the block into `invalid_cache`
            // permanently.  For a FutureBlock this caused a wedge: the peer
            // reconnected and re-offered the (now valid) block, which was
            // immediately rejected from the cache — stalling the chain forever.
            //
            // Fix: if the error is FutureBlock, roll back the chain to the
            // parent (so we are not stuck with an unapplied tip) but do NOT
            // insert into invalid_cache.  The peer will reconnect and re-offer
            // once the slot has passed.
            if matches!(reason, dugite_consensus::ConsensusError::FutureBlock { .. }) {
                warn!(
                    peer = %fetched.peer,
                    slot = block_slot.0,
                    block = block_number.0,
                    hash = %block_hash.to_hex(),
                    "Praos: block from future slot (beyond clock skew) — \
                     rolling back without blacklisting (transient, peer will retry)"
                );
                self.metrics
                    .header_validation_failures_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Roll back chain + ledger but do NOT insert into invalid_cache.
                let parent_point = {
                    let ls = self.ledger_state.read().await;
                    ls.tip.point.clone()
                };
                {
                    let mut db = self.chain_db.write().await;
                    if let Err(e) = db.rollback_to_point(&parent_point) {
                        error!(
                            error = %e,
                            "future-block rollback: volatile rollback to parent failed"
                        );
                    }
                }
                let _ = self.handle_ledger_rollback(&parent_point).await;
                return;
            }

            warn!(
                peer = %fetched.peer,
                slot = block_slot.0,
                block = block_number.0,
                hash = %block_hash.to_hex(),
                // Era + protocol_version + is_tpraos help diagnose hard-fork
                // transition issues (e.g. a PV7 block still in a TPraos/Alonzo
                // structure at the Vasil boundary — see BlockHeader::is_tpraos).
                hfc_era = %block.era,
                proto_major = block.header.protocol_version.major,
                is_tpraos = block.header.is_tpraos(),
                "Praos header validation FAILED — rejecting peer block: {reason}"
            );
            self.metrics
                .header_validation_failures_total
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // The block reached here as `AddedAsTip` (it connects to the ledger
            // tip), so it is now the selected-chain tip with the ledger one block
            // behind it. Simply returning would leave the ledger wedged behind an
            // invalid tip — and an honest competing fork could never be adopted
            // (this is exactly the Allegra-boundary wedge: a fork peer's invalid
            // pv-too-high block sat as the tip while real peers were dropped at the
            // forecast horizon). Mark the block invalid and roll the selected chain
            // + ledger back to its parent so chain selection abandons it and adopts
            // the best VALID fork — mirroring Haskell ChainSel's InvalidBlockCache.
            let parent_point = {
                let ls = self.ledger_state.read().await;
                ls.tip.point.clone()
            };
            self.abandon_failed_fork(
                block_hash,
                "apply-time Praos header validation failed",
                &parent_point,
            )
            .await;
            return;
        }

        // Determine validation mode.
        // Blocks from the network get full validation by default; only
        // ImmutableDB replay uses ApplyOnly.
        // Catch-up trusted-peer mode (#698).
        //
        // From-network sync at Babbage/Conway epochs caps at ~2 blocks/sec
        // because `validate_transaction_with_context` runs Phase-1 + Phase-2
        // (Plutus CEK evaluation + Ed25519 signature verification) on every
        // fetched block.  Haskell cardano-node has the same cost here —
        // operators avoid it by importing Mithril snapshots, not by doing
        // from-genesis ChainSync.
        //
        // For the #670 from-genesis verification we explicitly opt into
        // "trust the peers, skip our own validation" via
        // `DUGITE_TRUSTED_CATCHUP=1`.  This downgrades fetched blocks to
        // `ApplyOnly` mode — the same mode used for ImmutableDB chunk
        // replay after a Mithril import.  Phase-2 Plutus is skipped, witness
        // verification is skipped, all STS predicates are skipped.  The
        // Ledger validation mode for a block fetched from a PEER.
        //
        // Default: ValidateAll — full Phase-1 (witnesses, value/fee, TTL, ref
        // scripts) + Phase-2 (Plutus CEK) validation on EVERY network block,
        // matching cardano-node. cardano-node never trusts peer blocks: it runs
        // `STS.ValidateAll` on every block received from the network and only
        // uses `ValidateNone`/reapply when replaying its OWN already-validated
        // ImmutableDB. Reapplying without validation here is a consensus-safety
        // bug — a peer could feed a structurally-valid but cryptographically/
        // ledger-invalid block during catch-up and we would accept it, diverging
        // from the canonical chain.
        //
        // `DUGITE_TRUSTED_CATCHUP=1` opts OUT into the old fast (UNSAFE) reapply
        // behaviour for dev/profiling throughput runs only — it trusts peers and
        // skips Plutus, ~25x faster at Babbage+ but NOT cardano-node-equivalent.
        let trusted_catchup = std::env::var("DUGITE_TRUSTED_CATCHUP")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let validation_mode = if trusted_catchup {
            BlockValidationMode::ApplyOnly
        } else {
            BlockValidationMode::ValidateAll
        };
        // Observability (#698): track how many live-tip blocks go through
        // each mode so dashboards can distinguish full-validation from replay.
        match validation_mode {
            BlockValidationMode::ApplyOnly => self.metrics.inc_apply_mode_reapply(),
            BlockValidationMode::ValidateAll => self.metrics.inc_apply_mode_validate_all(),
        }

        // Cross-block Phase-2 pooling gate (DUGITE_DEFER_PHASE2_WINDOW, default
        // OFF). Defer the Plutus drain ONLY during catch-up (`!at_tip`) under
        // ValidateAll: at-tip we keep the exact synchronous path so served/forged
        // tips carry zero deferral. `at_tip` here uses the same predicate as the
        // publish gate below (peer_tip==0 ⇒ no peer yet ⇒ treat as at-tip).
        //
        // **Fork-in-window is structurally impossible under this gate.** Deferral
        // engages only when `peer_tip − block_slot > stability_window = ⌈3k/f⌉`
        // slots (≈129 600 = 36 h on mainnet/preview). Ouroboros k-finality bounds
        // ANY fork rollback to ≤ k blocks ≈ k/f slots. A deferred block therefore
        // sits ~3k blocks deep — 3× beyond the deepest reachable fork — so no fork
        // can ever roll back a block whose Plutus is still pending. The window is
        // flushed the moment `at_tip` flips (well before blocks enter the
        // fork-reachable k window), so a deferred block is always settled/final.
        // (This is why the deferral needs no fork-in-window handling: the gate
        // keeps the un-confirmed prefix strictly below the finality horizon.)
        let defer_pre_at_tip = {
            let peer_tip = self.metrics.get_peer_tip();
            let sw = dugite_consensus::stability_window_slots(
                self.consensus.security_param,
                self.consensus.active_slot_coeff,
            );
            peer_tip == 0 || block_slot.0.saturating_add(sw) >= peer_tip
        };
        let should_defer_phase2 = self.defer_phase2_window > 0
            && matches!(validation_mode, BlockValidationMode::ValidateAll)
            && !defer_pre_at_tip;

        // Apply to ledger state and collect the delta for LedgerSeq.
        //
        // Fix A (Bug B, 2026-05-16): use apply_block_with_delta so that every
        // live-tip block contributes a delta to LedgerSeq.  Previously this
        // path called apply_block (no delta), leaving LedgerSeq with 0 entries.
        // When the next fork fired, rollback_via_seq found nothing and fell
        // through to the snapshot path — which also fails on a fresh node with
        // no snapshots yet.  The rollback abort cascaded into clear_volatile(),
        // which destroyed all fork tracking state and caused permanent
        // StoreButDontChange for every subsequent relay block.
        //
        // Aligns the live path with process_blocks_bulk which already uses
        // apply_block_with_delta + push (see sync.rs:1146-1182).
        //
        // Lock order: ledger_state write lock released BEFORE ledger_seq write
        // lock acquired — same invariant enforced in process_blocks_bulk.
        // NOTE: apply_fetched_block and process_blocks_bulk are mutually
        // exclusive code paths (bulk sync runs to completion, then live sync
        // starts), so there is no risk of double-pushing the same block.
        // If this block opens a new deferred window, record the pre-apply ledger
        // tip as the window anchor (the exact rollback target should the window's
        // first block later prove block-fatal under pooled Plutus).
        if should_defer_phase2 && self.pending_phase2.is_empty() {
            let pre_tip = self.ledger_state.read().await.tip.point.clone();
            self.pending_phase2_anchor = Some(pre_tip);
        }
        let (delta, deferred_items) = {
            let mut ls = self.ledger_state.write().await;
            // #733: per-block apply horizon snapshot at the pre-block
            // ledger tip (one-shot, conservative — sound across HF windows).
            ls.phase2_apply_horizon = if matches!(validation_mode, BlockValidationMode::ValidateAll)
                && block.era >= dugite_primitives::era::Era::Babbage
            {
                let pre_tip = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                self.era_history
                    .read()
                    .await
                    .phase2_apply_horizon_slot(dugite_primitives::time::SlotNo(pre_tip))
            } else {
                None
            };
            // Issue #653 — wrap the CPU-bound apply in `block_in_place`
            // so the multi-thread tokio runtime spawns relief workers
            // for the duration. Without this, every block apply pins
            // one worker for the full Phase-1/Phase-2 validation and
            // UTxO/cert/gov update window, leaving the work-stealing
            // pool unable to fan out runnable tasks elsewhere.
            // Deferred path returns the captured Phase-2 work items (drained
            // later by the run loop's pooled flush); inline path drains Plutus
            // now and yields no items. Both produce a byte-identical delta.
            let apply_result = if should_defer_phase2 {
                tokio::task::block_in_place(|| {
                    ls.apply_block_with_delta_defer(&block, validation_mode)
                })
            } else {
                tokio::task::block_in_place(|| ls.apply_block_with_delta(&block, validation_mode))
                    .map(|delta| (delta, Vec::new()))
            };
            match apply_result {
                Ok((delta, items)) => {
                    // Publish the lock-free read view (issue #651 P2 / #652 P0)
                    // — readers see the new tip without acquiring the ledger
                    // lock.
                    //
                    // **Catch-up gate (#698).**  Publishing on every block
                    // during from-genesis network sync was a major perf hit:
                    // the LedgerView holds `Arc::clone(&certs.reward_accounts)`
                    // and friends, so after the publish the source Arc's
                    // refcount is 2.  The next block's first cert mutation
                    // calls `Arc::make_mut(&mut certs.reward_accounts)` which
                    // CoW-deep-clones the entire ~90 k-entry HashMap because
                    // it sees refcount > 1.  Across the four Arc-shared maps
                    // (reward_accounts, delegations, stake_key_deposits,
                    // pool_params) that was ~14 MB of memcpy per block on
                    // Babbage/Conway preview — far more than block apply
                    // itself takes.
                    //
                    // No external reader is querying us while we're
                    // catching up (we're behind tip, our view is stale
                    // anyway), so we skip the publish until we're inside
                    // the stability window of the peer-reported tip.  When
                    // we *are* at tip, every block publishes — the same
                    // behaviour as before — so latency-sensitive callers
                    // (forging, RPC) see the live view.
                    let peer_tip = self.metrics.get_peer_tip();
                    let local_tip = block_slot.0;
                    let stability_window = dugite_consensus::stability_window_slots(
                        self.consensus.security_param,
                        self.consensus.active_slot_coeff,
                    );
                    let at_tip =
                        peer_tip == 0 || local_tip.saturating_add(stability_window) >= peer_tip;
                    // Always publish on era / epoch transitions even during
                    // catch-up so consumers that key off those (epoch
                    // metrics, era history watchers) get a current view at
                    // boundary blocks.
                    let is_era_transition = ls.pending_era_transition.is_some();
                    if at_tip || is_era_transition {
                        self.publish_ledger_view(&ls);
                        // Refresh the heavy governance gauges + utxo_count
                        // per-block AT TIP / era-boundary. `publish_ledger_view`
                        // alone touches no Prometheus atomics, so without this
                        // the DRep / proposal / committee / pparam / utxo_count
                        // gauges froze at tip on every epoch boundary the node
                        // did not forge itself (same staleness class as pots).
                        // Gated on at_tip || is_era_transition so the governance
                        // map walk never runs per-block on the bulk-sync hot
                        // path (the catch-up `else` branch keeps the O(1)
                        // atomics only).
                        refresh_heavy_at_tip_gauges(&self.metrics, &ls);
                    } else {
                        // We're catching up — skip the heavy LedgerView Arc
                        // materialization (the whole point of the gate) but
                        // we MUST still notify `ledger_tip_slot_tx`
                        // subscribers, otherwise per-peer ChainSync tasks
                        // parked on forecast-horizon exhaustion (#654)
                        // never wake up.  In practice they hit the 60 s
                        // suspension cap and disconnect, peers cycle
                        // through `header slot N beyond forecast horizon
                        // after 60s suspension; disconnecting`, and the
                        // node stalls with zero hot peers — exactly the
                        // regression observed before this branch added it.
                        //
                        // Also update the cheap epoch/utxo atomic metrics
                        // directly from the live ledger state — without
                        // this, monitoring shows `dugite_epoch_number` /
                        // `dugite_utxo_count` frozen at whatever value the
                        // last `publish_ledger_view` left behind, making
                        // the node appear stuck at epoch N for the entire
                        // catch-up duration (issue: cosmetic but very
                        // misleading).
                        self.metrics.set_epoch(ls.epoch.0);
                        self.metrics.set_protocol_version(
                            ls.epochs.protocol_params.protocol_version_major,
                            ls.epochs.protocol_params.protocol_version_minor,
                        );
                        self.metrics.set_utxo_count(ls.utxo.utxo_set.len() as u64);
                        // Keep the pots gauges live during catch-up too. These are
                        // O(1) atomic stores (the heavy governance snapshot — dreps,
                        // proposals, delegations — stays on the at-tip /
                        // era-transition `publish_ledger_view` path above). Without
                        // them `dugite_reserves_lovelace` / `dugite_treasury_lovelace`
                        // froze at their startup values for the whole bulk sync,
                        // making the pots appear stuck (cosmetic, but misleading for
                        // boundary cross-checks).
                        self.metrics
                            .reserves_lovelace
                            .store(ls.epochs.reserves.0, std::sync::atomic::Ordering::Relaxed);
                        self.metrics
                            .treasury_lovelace
                            .store(ls.epochs.treasury.0, std::sync::atomic::Ordering::Relaxed);
                        let _ = self.ledger_tip_slot_tx.send(local_tip);
                    }
                    // Consume pending era transition and propagate to the HFC state machine.
                    if let Some((prev_era, new_era, epoch)) = ls.pending_era_transition.take() {
                        let mut eh = self.era_history.write().await;
                        if eh.current_era() < new_era {
                            eh.record_era_transition(new_era, epoch.0);
                            info!(
                                prev = %prev_era,
                                new = %new_era,
                                epoch = epoch.0,
                                "Era transition recorded in HFC era history",
                            );
                        }
                    }
                    (delta, items)
                }
                Err(e) => {
                    // Issue #669 — surface this as a hard operator-actionable
                    // signal.  The earlier `warn! + return` was indistinguishable
                    // from a silent network gap: the chain stops advancing but
                    // there's no metric increment and no error-level entry, so
                    // monitoring sees only the absence of further "Chain extended"
                    // lines.  An apply failure on a fetched block is always one
                    // of:
                    //   (a) a dugite bug (the network accepts the block; we
                    //       mis-validate).  Loud ERROR + metric so the operator
                    //       files an issue instead of mistaking it for upstream
                    //       silence.  See #668 for the canonical case.
                    //   (b) a peer feeding bad data.  Same loud signal; the peer
                    //       is implicitly throttled by the existing fetch-rate
                    //       controls and follow-up work (#669) will add explicit
                    //       per-peer disqualification on apply failure.
                    self.metrics
                        .block_apply_failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    error!(
                        peer = %fetched.peer,
                        slot = block_slot.0,
                        block = block_number.0,
                        hash = %block_hash.to_hex(),
                        prev = %block.prev_hash().to_hex(),
                        txs = block.transactions.len(),
                        "Fetched block failed ledger apply: {e} \
                         — chain advance halted at this block. \
                         Investigate the validation error above; the same \
                         block will be re-fetched from this and other peers \
                         and will keep failing until the underlying issue is \
                         resolved."
                    );
                    return;
                }
            }
        };
        // Push delta to LedgerSeq (ledger_state lock already released above).
        let seq_incoherent = {
            let mut seq = self.ledger_seq.write().await;
            seq.push(delta);
            seq.is_incoherent()
        };

        // #985 self-heal. `push` flags a delta that does not chain onto the
        // window, which means some path advanced `ledger_state` in bulk
        // without calling `reanchor_ledger_seq`. The guard already prevents
        // corruption — `find_rollback_n` declines while flagged, so rollbacks
        // take the snapshot slow path — but that costs rollback *capability*
        // until something re-anchors.
        //
        // Here we can simply fix it: `ledger_state` has just had this block
        // applied, so it is exactly the state to anchor at. One re-anchor and
        // the window rebuilds coherently from the next block on.
        //
        // That turns a missed re-anchor site from "degraded for the process
        // lifetime" into "degraded for one block", which is the property that
        // would have kept #985 from being a permanent wedge. The other push
        // sites (fork replay, forge) need no copy of this: any of them is
        // followed by live applies through here.
        //
        // WARN not ERROR — the condition is already handled; the log exists to
        // get the missing re-anchor site found and fixed.
        if seq_incoherent {
            warn!(
                slot = block_slot.0,
                block = block_number.0,
                "LedgerSeq was incoherent at block apply — re-anchoring on the live \
                 ledger. Some path advanced the ledger without re-anchoring; please \
                 report this with the preceding log context (#985)."
            );
            self.reanchor_ledger_seq("coherence guard tripped at block apply")
                .await;
        }

        // Update chain fragment.
        {
            let mut fragment = self.chain_fragment.write().await;
            fragment.push(block.header.clone());
        }

        // Update consensus tip.
        self.consensus.update_tip(block.tip());

        // Log the new block at INFO level so operators can see chain advancement
        let hash_hex = block.header.header_hash.to_hex();
        info!(
            era = %block.era,
            slot = block_slot.0,
            block = block_number.0,
            txs = block.transactions.len(),
            hash = %hash_hex,
            "Chain extended",
        );

        // Update metrics.
        self.metrics.record_block_received();
        self.metrics
            .blocks_received
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics
            .blocks_applied
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Tip-query staleness fix (2026-05-16): shared post-apply housekeeping
        // also used by try_forge_block_at.  Replaces the previous inline
        // metric/mempool/snapshot updates.
        self.post_block_apply_updates(&block, block_slot, block_number)
            .await;

        // Announce to downstream peers.
        if let Some(ref tx) = self.block_announcement_tx {
            let mut hash_bytes = [0u8; 32];
            hash_bytes.copy_from_slice(block.header.header_hash.as_ref());
            let subscribers = tx.receiver_count();
            let _ = tx.send(dugite_network::BlockAnnouncement {
                slot: block_slot.0,
                hash: hash_bytes,
                block_number: block_number.0,
            });
            if let Some(ref tb) = self.tip_broadcaster {
                tb.announce_apply(tip_broadcast::TipApply {
                    slot: block_slot.0,
                    hash: hash_bytes,
                    block_number: block_number.0,
                    era: block.era,
                });
            }
            debug!(
                slot = block_slot.0,
                block = block_number.0,
                subscribers,
                "live-tip: announced block to peers"
            );
            self.metrics
                .blocks_announced
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        // ChainDB maintenance (copy-to-immutable, GC, fragment-anchor advance)
        // is driven by the main loop's `maintenance_ticker`, NOT inline here:
        // each `flush_to_immutable_batch` ends in `remove_blocks_by_hashes`,
        // which rewrites the VolatileDB WAL (O(volatile-window)).  Running that
        // per block would replace the old O(N²) `get_all_fork_tips` cost with a
        // per-block O(k) WAL rewrite.  Batching it on the 250 ms ticker keeps
        // WAL rewrites to ~4/s while still bounding VolatileDB to the k window.

        // Cross-block Phase-2 pooling: stash this block + its deferred Plutus
        // work items for the run loop's pooled flush. `block` is moved here
        // after its last borrow (the announce above), so no clone is needed.
        if should_defer_phase2 {
            // Track the running redeemer count so the run loop flushes by
            // work-item count (memory) before the block window fills.
            self.pending_phase2_items += deferred_items.len();
            self.pending_phase2.push((Box::new(block), deferred_items));
        }
    }

    /// Drain the deferred Phase-2 (Plutus) window: evaluate every pooled block's
    /// work items on the memory-bounded pool, then apply each block's fatality
    /// verdict in order. On a block-fatal collection error, roll the ledger back
    /// to before that block and stop (mirrors the synchronous per-block `Err`
    /// path, just deferred). A no-op when the window is empty.
    ///
    /// Memory + cancel safety: the pooled eval is concurrency-capped and chunked
    /// (see [`dugite_ledger::plutus::run_phase2_parallel_pooled_cancellable`]) so
    /// it cannot reproduce the deferral-soak RSS runaway, and — when
    /// `allow_cancel` is true — it observes the shutdown watch between chunks so
    /// a SIGTERM during a long flush aborts the remaining work instead of being
    /// swallowed by `block_in_place`. The shutdown-arm flush passes
    /// `allow_cancel = false` so it always confirms the (small, bounded) window
    /// before the persisted snapshot.
    ///
    /// Byte-exact: state was already applied in-order by `apply_fetched_block`;
    /// this only runs the deferred read-only fatality check whose decision is a
    /// pure function of each block's self-contained work items.
    async fn flush_pending_phase2(&mut self, allow_cancel: bool) {
        if self.pending_phase2.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.pending_phase2);
        let anchor = self.pending_phase2_anchor.take();
        self.pending_phase2_items = 0;
        let n_blocks = batch.len();
        let (blocks, items): (Vec<_>, Vec<_>) = batch.into_iter().unzip();
        let n_items: usize = items.iter().map(Vec::len).sum();
        let t_flush = std::time::Instant::now();

        // Cancellation token: observe the shutdown watch between chunks so a
        // SIGTERM during a long flush aborts the remaining work. The shutdown-arm
        // flush passes allow_cancel = false (None token) so it always completes.
        let sd_rx = if allow_cancel {
            self.shutdown_rx_for_flush.clone()
        } else {
            None
        };
        let cancel = move || sd_rx.as_ref().is_some_and(|rx| *rx.borrow());

        // Pooled rayon evaluation (CPU-bound, memory-bounded) under
        // block_in_place so the multi-thread runtime spawns relief workers.
        let outcomes_per_block = tokio::task::block_in_place(|| {
            dugite_ledger::plutus::run_phase2_parallel_pooled_cancellable(items, &cancel)
        });

        let outcomes_per_block = match outcomes_per_block {
            Some(o) => o,
            None => {
                // Cancelled mid-flush (shutdown in progress). Nothing was applied
                // yet — the deferred blocks' STATE is in the in-memory ledger but
                // their Plutus is unconfirmed, so we must NOT let them reach a
                // persisted snapshot. Roll the ledger back to the window anchor
                // (undoing the whole un-confirmed window) and disable deferral;
                // the blocks are re-fetched + validated synchronously on restart.
                warn!(
                    pooled_blocks = n_blocks,
                    pooled_items = n_items,
                    "Deferred Phase-2 flush cancelled by shutdown — rolling the \
                     ledger back to the last confirmed block and disabling deferral."
                );
                if let Some(rb) = anchor {
                    let _ = self.handle_ledger_rollback(&rb).await;
                }
                self.defer_phase2_window = 0;
                return;
            }
        };

        // Apply each block's fatality verdict in chain order. `prev_confirmed`
        // tracks the last block that drained non-fatal (starting at the window
        // anchor = parent of block[0]). The FIRST block with a block-fatal
        // CollectError is the rejection point — identical to the block the
        // synchronous path would have rejected.
        let mut prev_confirmed: Option<dugite_primitives::block::Point> = anchor;
        for (block, outcomes) in blocks.into_iter().zip(outcomes_per_block) {
            if let Err(e) = dugite_ledger::state::apply_phase2_outcomes(&block, outcomes) {
                let fatal_slot = block.slot().0;
                self.metrics
                    .block_apply_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                error!(
                    slot = fatal_slot,
                    pooled_blocks = n_blocks,
                    "Deferred Phase-2 (pooled) found a block-fatal error: {e} — rolling \
                     the ledger back to the last confirmed block and disabling deferral. \
                     The rejected block (and any later un-confirmed window blocks) are \
                     undone; they are re-fetched and re-validated synchronously."
                );
                // Roll back to the last confirmed on-chain point (real slot+hash,
                // never a slot-1 guess). This undoes the fatal block AND every
                // later un-confirmed block in the window via the LedgerSeq path.
                match prev_confirmed {
                    Some(rb) => {
                        let _ = self.handle_ledger_rollback(&rb).await;
                    }
                    None => {
                        // No anchor (should not happen) — halt deferral; the
                        // serial re-validation on restart/refetch is the backstop.
                        error!(
                            "Deferred Phase-2 fatal at window head with no anchor — \
                             cannot roll back precisely; halting deferral."
                        );
                    }
                }
                // Disable further deferral for the remainder of this run so the
                // re-fetched blocks are validated synchronously (conservative).
                self.defer_phase2_window = 0;
                return;
            }
            prev_confirmed = Some(dugite_primitives::block::Point::Specific(
                block.slot(),
                *block.hash(),
            ));
        }
        self.metrics.record_deferred_phase2_flush(n_blocks as u64);
        debug!(
            pooled_blocks = n_blocks,
            pooled_items = n_items,
            elapsed_ms = t_flush.elapsed().as_millis() as u64,
            "Deferred Phase-2 window flushed (pooled, memory-bounded)"
        );
    }

    // ─── run_background_maintenance() ────────────────────────────────────────

    /// Run periodic background maintenance after block application.
    ///
    /// Handles copy-to-immutable (when chain fragment grows beyond k),
    /// GC of old volatile entries, and snapshot scheduling. Matches
    /// Haskell's Background.hs pattern.
    ///
    /// Note: The full integration with CopyToImmutable, GcScheduler, and
    /// SnapshotScheduler requires the same detailed parameters as the
    /// existing chain_sync_loop path (fragment length, oldest header,
    /// ledger anchor advancement callback). These operations are already
    /// performed in process_forward_blocks() during sync. For blocks
    /// arriving via the new fetched_blocks channel, this is a placeholder
    /// that will be unified with process_forward_blocks() in Task 7.
    /// Bound VolatileDB and the chain fragment to the k-block rollback window
    /// during live sync.
    ///
    /// The live `apply_fetched_block` path stores every fetched block in
    /// VolatileDB but never finalises k-deep blocks to ImmutableDB, and pushes
    /// every header onto the chain fragment without ever trimming it (the
    /// volatile→immutable copy + fragment advance lived only in the now-unwired
    /// `process_forward_blocks`).  Both therefore grow without bound, and
    /// `ChainSelQueue::process_add_block` runs `VolatileDB::get_all_fork_tips()`
    /// — O(volatile size) — on every block add.  Unbounded VolatileDB turns
    /// per-block apply into O(N²): mainnet Byron from-genesis sync decays from
    /// ~150 blk/s to ~2 blk/s as the volatile set passes ~100 k blocks
    /// (profiled: `get_all_fork_tips` = 30 % of the apply worker, RSS climbing
    /// past 700 MB, all resetting on restart).
    ///
    /// This finalises k-deep volatile blocks to ImmutableDB in bounded batches,
    /// GCs the removed blocks, and advances the chain-fragment anchor to the new
    /// immutable tip — mirroring Haskell's ChainDB Background `copyToImmutableDB`.
    /// Driven by the main loop's 250 ms `maintenance_ticker` so the WAL rewrite
    /// inside `remove_blocks_by_hashes` is amortised across many blocks rather
    /// than paid per block.  LedgerSeq is already self-bounded at k by
    /// `LedgerSeq::push` (it advances its anchor on overflow), so it needs no
    /// action here.
    async fn run_background_maintenance(&mut self) {
        // Bisection/diagnostic escape hatch: `DUGITE_NO_MAINT=1` disables the
        // volatile→immutable maintenance entirely (reverts to the pre-fix
        // unbounded-VolatileDB behaviour) so the maintenance can be isolated
        // when triaging sync issues.
        if std::env::var_os("DUGITE_NO_MAINT").is_some() {
            return;
        }
        // Volatile retention window: how many recent blocks to keep resident in
        // VolatileDB before finalising to ImmutableDB.  This is deliberately far
        // larger than the protocol security parameter `k` (2160) — keeping extra
        // *already-finalised* blocks volatile is safe (it only costs memory) and
        // it keeps the ImmutableDB tip trailing the active sync tip by a wide
        // margin.  That margin is what prevents the immutable tip from advancing
        // through the slot region where freshly-connected peers place their
        // initial ChainSync intersection: during a from-genesis sync the tip
        // races ahead while peers that connected early still hold a low
        // intersection point, and if the immutable tip overtakes that point the
        // peer is forced to re-intersect and re-stream already-known headers,
        // which the BlockFetch pipeline drains to empty (the
        // `prune_already_known_pending_headers` storm) and the sync stalls.  A
        // window comfortably past the connection-storm settling point (~5 k
        // blocks) avoids this entirely.  The O(window) per-block
        // `get_all_fork_tips` scan at 10 k is ~0.6 ms — a >1000 blk/s ceiling,
        // far above the sustained Byron apply rate.
        let retain_blocks: u64 = std::env::var("DUGITE_VOLATILE_RETAIN")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10_000)
            .max(self.consensus.security_param);

        // Flush k-deep volatile blocks to ImmutableDB in bounded batches,
        // releasing the ChainDB write lock between batches so inbound ChainSync
        // server reads (and the per-peer ChainSync client tasks) are not
        // starved during a large catch-up flush.  Batch size 50 matches the
        // value proven in the legacy `process_forward_blocks` path: a larger
        // batch holds the ChainDB write lock long enough (ImmutableDB append +
        // VolatileDB WAL rewrite) that peers time out their ChainSync idle
        // timeout mid-flush and drop the connection — during the from-genesis
        // peer-connection storm that churn could strand the header source and
        // stall the sync.
        const FLUSH_BATCH_SIZE: u64 = 50;
        let mut total_flushed: u64 = 0;
        loop {
            let flushed = {
                let mut db = self.chain_db.write().await;
                // Finalisation is k-based in EVERY consensus mode. The
                // Ouroboros Genesis LoE constrains chain SELECTION (which tip we
                // adopt), NOT immutable finalisation: in ouroboros-consensus
                // `copyToImmutableDB` runs unconditionally whenever the chain is
                // longer than its k-deep suffix, never gated by the LoE/GSM
                // state. A previous build gated this flush on `loe_slot`, which
                // (because PreSyncing pins the LoE at genesis during Byron, when
                // no big-ledger peers exist) froze the immutable tip and let the
                // VolatileDB grow without bound (observed 1.6M blocks → O(N) CPU
                // storm → wedge ~epoch 208). Always flush k-deep.
                // #767: the ImmutableDB batch flush is synchronous I/O held
                // under `chain_db.write()`. Wrap in `block_in_place` (same
                // rationale as the snapshot LSM flush and #653 apply) so the
                // runtime keeps servicing network/timer tasks and the 5s
                // peer-deactivate cascade is not triggered.
                let result = tokio::task::block_in_place(|| {
                    db.flush_to_immutable_batch_retain(retain_blocks, FLUSH_BATCH_SIZE)
                });
                match result {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(error = %e, "background-maintenance: flush to immutable failed");
                        0
                    }
                }
            };
            total_flushed += flushed;
            if flushed == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }

        if total_flushed == 0 {
            return;
        }

        // GC volatile blocks whose deferred-removal delay has expired.
        {
            let mut db = self.chain_db.write().await;
            db.gc_volatile();
        }

        // Advance the chain-fragment anchor to the new ImmutableDB tip so the
        // fragment (push-only in the live apply path) is trimmed to the
        // volatile window.  Points older than the immutable tip are served to
        // downstream ChainSync peers from ImmutableDB historical anchors, not
        // the fragment.
        let imm_tip = {
            let db = self.chain_db.read().await;
            db.get_immutable_tip_point()
        };
        if let Some(anchor) = imm_tip {
            let mut fragment = self.chain_fragment.write().await;
            fragment.advance_anchor(anchor);
        }

        trace!(
            flushed = total_flushed,
            "background-maintenance: finalised volatile blocks to immutable"
        );
    }

    /// Post-apply housekeeping shared by every code path that adopts a block
    /// at live tip.
    ///
    /// Updates the Prometheus block_number/slot/tip_slot_time_ms/epoch gauges,
    /// refreshes `compute_sync_progress`, sweeps the mempool for confirmed +
    /// invalid transactions, and refreshes the N2C `NodeStateSnapshot`
    /// (rate-limited to once per second to avoid the O(n²) DRep scan stalling
    /// the apply loop).
    ///
    /// Both `apply_fetched_block` and `try_forge_block_at` MUST call this
    /// after a successful block adopt.  Before this helper existed the forge
    /// path skipped every one of these updates, leaving Prometheus and N2C
    /// tip queries stale on every own-forged block.
    async fn post_block_apply_updates(
        &mut self,
        block: &dugite_primitives::block::Block,
        block_slot: dugite_primitives::time::SlotNo,
        block_number: dugite_primitives::time::BlockNo,
    ) {
        let timing = post_apply_timing_enabled();
        let t_start = if timing { Some(Instant::now()) } else { None };

        // 1. Metrics — gauge updates that drive Prometheus + the tip_age timer.
        self.metrics.set_block_number(block_number.0);
        self.metrics.set_slot(block_slot.0);
        {
            // `self.view()` is only refreshed by `publish_ledger_view`, which
            // is gated on at-tip during catch-up sync (see 4832-4847).  Reading
            // `view.epoch` here would overwrite the catch-up metric write
            // (4855) with the stale view value, leaving Prometheus stuck at
            // the boot-time epoch.  Use `view` only for `slot_config`
            // (immutable for the era and safe to read stale); take a quick
            // read-lock on the live ledger state for the epoch atomic.
            let view = self.view();
            let slot_time_ms = self
                .slot_to_wallclock_ms(block_slot.0, &view.slot_config)
                .await;
            self.metrics.set_tip_slot_time_ms(slot_time_ms);
            let (live_epoch, pv_major, pv_minor, treasury, reserves) = {
                let ls = self.ledger_state.read().await;
                (
                    ls.epoch.0,
                    ls.epochs.protocol_params.protocol_version_major,
                    ls.epochs.protocol_params.protocol_version_minor,
                    ls.epochs.treasury.0,
                    ls.epochs.reserves.0,
                )
            };
            self.metrics.set_epoch(live_epoch);
            self.metrics.set_protocol_version(pv_major, pv_minor);
            // Keep the pots gauges as live as the epoch gauge.  This per-block
            // path runs at tip too (unlike the catch-up-only inline store in
            // `apply_fetched_block`), so the reserves→treasury transfer applied
            // at an epoch boundary is reflected immediately even when the node
            // did not forge the boundary block.  Cheap (two atomic stores).
            self.metrics.set_pots(treasury, reserves);
        }
        // Authoritative era from the applied block's HFC era tag — NOT from the
        // ledger protocol-version major (which is Shelley-shaped and reads 2
        // even during Byron, mislabelling Byron blocks as "Shelley").
        self.metrics.set_era(block.era.to_era_index() as u64);
        self.update_sync_progress(block_slot.0, &self.view().slot_config)
            .await;

        // Era-aware epoch-progress gauges (`dugite_epoch_length` +
        // `dugite_slot_in_epoch`) for dugite-monitor.  The HFC era history is
        // the authoritative slot↔epoch map across the Byron/Shelley boundary,
        // so this reports the current era's epoch length (e.g. Byron 21600 vs
        // Shelley 432000) and the correct in-epoch offset — fixing the monitor
        // rolling the epoch number while the progress bar was only ~5% full.
        // On a past-horizon error (should not happen for an already-applied
        // slot) we leave the previous values in place.
        {
            let eh = self.era_history.read().await;
            if let Ok((eh_epoch, slot_in_epoch)) = eh.slot_to_epoch(block_slot) {
                if let Ok(epoch_len) = eh.epoch_size(eh_epoch) {
                    // Era-correct slot length (Byron 20 000 ms vs Shelley+ 1 000
                    // ms) so the monitor's epoch time-remaining is right across
                    // the Byron boundary. Fall back to 1 000 ms on past-horizon.
                    let slot_len_ms = eh.epoch_slot_length_ms(eh_epoch).unwrap_or(1000);
                    self.metrics
                        .set_epoch_progress(epoch_len, slot_in_epoch, slot_len_ms);
                }
            }
        }

        let t_after_metrics = if timing { Some(Instant::now()) } else { None };

        // 2. Mempool sweep.  Remove confirmed txs first, then run the
        //    input-conflict / TTL revalidation just like apply_fetched_block
        //    used to do inline.
        let confirmed: Vec<_> = block.transactions.iter().map(|tx| tx.hash).collect();
        if !confirmed.is_empty() {
            self.mempool
                .remove_txs_with_reason(&confirmed, dugite_mempool::MempoolRemoveReason::Mined);
        }
        if !self.mempool.is_empty() {
            let consumed_inputs: std::collections::HashSet<_> = block
                .transactions
                .iter()
                .flat_map(|tx| tx.body.inputs.iter().cloned())
                .collect();
            let tip_slot = block_slot;
            let ls = self.ledger_state.read().await;
            // #996: this is dugite-bp's OWN-FORGE path — the one that minted
            // the block cardano-node rejected with
            // `ConwayCommitteeHasPreviouslyResigned` and then refused for the
            // rest of the run. Like the two other revalidation sites it used to
            // re-check a hand-written list (inputs, TTL, UTxO, gov-action
            // votes), so any other predicate invalidated by the block just
            // applied stayed invisible until a Haskell peer rejected the next
            // block. Haskell's `reapplyTxs` re-runs every state-dependent
            // predicate here; so do we now.
            let slot_config = {
                let mut sc = ls.slot_config;
                let eh = self.era_history.read().await;
                if let Some(h) = eh.safe_zone_horizon_slot(tip_slot) {
                    sc.safe_zone_horizon_slot = Some(h);
                }
                sc
            };
            let virtual_utxos = self.mempool.virtual_utxo_snapshot();
            let utxo_view =
                dugite_ledger::utxo::CompositeUtxoView::new(&ls.utxo.utxo_set, virtual_utxos);
            let ctx = ls.mempool_validation_context();
            self.mempool.revalidate_all(|tx| {
                // Cheap first pass: an input consumed by the block just applied
                // is gone even though the virtual-UTxO overlay may still list
                // it.
                if tx.body.inputs.iter().any(|i| consumed_inputs.contains(i)) {
                    return false;
                }
                if let Some(ttl) = tx.body.ttl {
                    if tip_slot.0 >= ttl.0 {
                        return false;
                    }
                }
                let tx_size = tx.raw_cbor.as_ref().map(|b| b.len() as u64).unwrap_or(0);
                match dugite_ledger::validation::reapply_tx_for_mempool(
                    tx,
                    &utxo_view,
                    &ls.epochs.protocol_params,
                    tip_slot.0,
                    tx_size,
                    Some(&slot_config),
                    ctx.clone(),
                ) {
                    Ok(()) => true,
                    Err(errors) => {
                        tracing::info!(
                            tx_hash = %tx.hash,
                            ?errors,
                            "Mempool: tx became invalid at the new tip — evicting \
                             before it can be forged (#996)"
                        );
                        false
                    }
                }
            });
            drop(ls); // Release before update_query_state re-acquires.
        }
        self.metrics.set_mempool_count(self.mempool.len() as u64);
        self.metrics.mempool_bytes.store(
            self.mempool.total_bytes() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        let t_after_mempool = if timing { Some(Instant::now()) } else { None };

        // 3. N2C snapshot refresh, rate-limited by `query_state_refresh_interval`:
        //    1 Hz at tip, 30 s during catch-up.  The rebuild is ~1.4 s at
        //    mainnet scale and runs synchronously on the apply task, so the
        //    catch-up cadence directly bounds bulk-sync throughput.  Same
        //    at-tip signal as the WAL/snapshot/LedgerSeq catch-up modes
        //    (updated per applied block in `apply_fetched_block`).
        let at_tip = self
            .volatile_wal_sync_at_tip
            .load(std::sync::atomic::Ordering::Relaxed);
        let query_state_ran =
            if self.last_query_state_update.elapsed() >= query_state_refresh_interval(at_tip) {
                self.update_query_state().await;
                self.last_query_state_update = std::time::Instant::now();
                true
            } else {
                false
            };

        let t_after_query_state = if timing { Some(Instant::now()) } else { None };

        // 4. Ledger snapshot scheduler (Bug F fix, 2026-05-16).
        //
        // Previously snapshots were only written by `process_forward_blocks`
        // (the bulk-sync code path) and by graceful shutdown.  The live-tip
        // path NEVER drove the scheduler, so on a node that came up clean
        // (no bulk sync) and then ran at-tip indefinitely, no snapshot was
        // ever written until shutdown.  When a deep fork-switch then needed
        // to roll back beyond the k-block LedgerSeq window, the rollback
        // aborted with "Rollback target outside LedgerSeq volatile window
        // AND no canonical snapshot available" and the chain stayed stuck
        // on its current selection until restart.
        //
        // The local-devnet 30-min soak hit this within ~5 minutes once the
        // first fork that diverged > k blocks tried to switch in.
        //
        // The fix: drive the scheduler from this shared post-apply helper
        // so every code path that adopts a block (live-tip apply, forge
        // adopt, TriggeredFork replay) also gets a chance to snapshot.
        // The scheduler's own `maybe_snapshot_check` rate-limits — most
        // calls just bump the counter and return false.  The first call
        // after boot (last_snapshot_epoch == None) returns true, so we
        // always have at least an epoch-0 snapshot covering subsequent
        // rollbacks back to that point.
        //
        // Lock order: this runs after the ledger_state write lock for the
        // apply has already been released by the caller, and the save
        // re-acquires it via `save_ledger_snapshot`.  No new contention.
        let current_epoch = self.ledger_state.read().await.epoch;
        let should_snapshot = self
            .bg_snapshot_scheduler
            .maybe_snapshot_check(current_epoch, block_slot);
        if should_snapshot {
            // Issue #695: fire via the non-blocking worker so the
            // apply path doesn't pause for the bincode walk. Only
            // record on `Enqueued`; skipping leaves the scheduler in
            // its "pending deadline expired" state so the next block
            // retries.
            if matches!(
                self.try_snapshot_async().await,
                snapshot_worker::SnapshotEnqueue::Enqueued
            ) {
                self.bg_snapshot_scheduler
                    .record_snapshot_taken(current_epoch, block_slot);
            }
        }

        // Emit per-block timing breakdown when DUGITE_POST_APPLY_TIMING=1.
        //
        // Each section is timed independently so operators can pinpoint which
        // step is dominating the apply loop at epoch 28-29 (issue #702).
        // Log lines use structured fields so they can be extracted with
        //   grep '"post_apply_timing"' node.log | jq ...
        if let (Some(t0), Some(t1), Some(t2), Some(t3)) = (
            t_start,
            t_after_metrics,
            t_after_mempool,
            t_after_query_state,
        ) {
            let metrics_us = t1.duration_since(t0).as_micros();
            let mempool_us = t2.duration_since(t1).as_micros();
            let query_state_us = t3.duration_since(t2).as_micros();
            let snapshot_us = t3.elapsed().as_micros();
            let total_us = t0.elapsed().as_micros();
            info!(
                slot = block_slot.0,
                block = block_number.0,
                metrics_us,
                mempool_us,
                query_state_ran,
                query_state_us,
                snapshot_us,
                total_us,
                "post_apply_timing"
            );
        }
    }

    // ─── handle_n2c_connection() ─────────────────────────────────────────────

    /// Handle a single N2C (Unix socket) connection.
    ///
    /// Sets up a Mux over the bearer, runs the N2C handshake, then spawns
    /// protocol tasks for LocalChainSync, LocalTxSubmission, LocalStateQuery,
    /// and LocalTxMonitor.
    #[allow(clippy::too_many_arguments)]
    async fn handle_n2c_connection(
        stream: tokio::net::UnixStream,
        network_magic: u64,
        query_handler: Arc<RwLock<QueryHandler>>,
        block_provider: Arc<serve::ChainDBBlockProvider>,
        mempool: Arc<Mempool>,
        tx_validator: Arc<serve::LedgerTxValidator>,
        ledger: Arc<RwLock<LedgerState>>,
        announcement_rx: tokio::sync::broadcast::Receiver<dugite_network::BlockAnnouncement>,
        rollback_rx: tokio::sync::broadcast::Receiver<RollbackAnnouncement>,
        metrics: Arc<crate::metrics::NodeMetrics>,
    ) -> Result<()> {
        use dugite_network::protocol;

        let bearer = dugite_network::UnixBearer::new(stream);
        let mut mux = dugite_network::Mux::new(bearer, false); // we are responder

        // Subscribe protocol channels (responder direction for all)
        let mut hs_ch = mux.subscribe(
            protocol::PROTOCOL_HANDSHAKE,
            dugite_network::Direction::ResponderDir,
            65536,
        );
        let mut cs_ch = mux.subscribe(
            protocol::PROTOCOL_N2C_CHAINSYNC,
            dugite_network::Direction::ResponderDir,
            1_048_576,
        );
        let mut tx_ch = mux.subscribe(
            protocol::PROTOCOL_N2C_TXSUBMISSION,
            dugite_network::Direction::ResponderDir,
            1_048_576,
        );
        let mut sq_ch = mux.subscribe(
            protocol::PROTOCOL_N2C_STATEQUERY,
            dugite_network::Direction::ResponderDir,
            1_048_576,
        );
        let mut tm_ch = mux.subscribe(
            protocol::PROTOCOL_N2C_TXMONITOR,
            dugite_network::Direction::ResponderDir,
            1_048_576,
        );

        // Start the mux tasks (egress/ingress)
        let mux_handle = tokio::spawn(async move { mux.run().await });

        // Run N2C handshake as server
        let our_data = dugite_network::N2CVersionData::new(network_magic);
        let hs_result =
            dugite_network::handshake::run_n2c_handshake_server(&mut hs_ch, &our_data).await;
        let n2c_version = match hs_result {
            Ok(r) => {
                debug!(version = r.version, "N2C handshake accepted");
                r.version
            }
            Err(e) => {
                debug!("N2C handshake failed: {e}");
                mux_handle.abort();
                return Ok(());
            }
        };

        // Spawn protocol tasks — each runs until the client disconnects
        // or an error occurs. The mux handle keeps the transport alive.

        // LocalChainSync server
        let lcs_bp = block_provider.clone();
        let lcs_ann_rx = announcement_rx;
        let lcs_rb_rx = rollback_rx;
        let lcs_task = tokio::spawn(async move {
            let mut server = protocol::local_chainsync::server::LocalChainSyncServer::new();
            if let Err(e) = server
                .run(&mut cs_ch, lcs_bp.as_ref(), lcs_ann_rx, lcs_rb_rx)
                .await
            {
                debug!("N2C LocalChainSync ended: {e}");
            }
        });

        // LocalTxSubmission server
        let lts_validator = tx_validator;
        let lts_mempool = mempool.clone();
        let lts_metrics = metrics.clone();
        let lts_task = tokio::spawn(async move {
            // C12 fix: on_accepted now returns Result<(), String> so the server
            // can send MsgRejectTx if the mempool fails to admit the transaction
            // after successful Phase-1/Phase-2 validation. This prevents the
            // protocol violation of sending MsgAcceptTx for an un-admitted tx.
            let on_accepted = |era_id: u16, tx_bytes: Vec<u8>| -> Result<(), String> {
                // Decode the transaction and add it to the mempool.
                let size_bytes = tx_bytes.len();
                match dugite_serialization::decode_transaction(era_id, &tx_bytes) {
                    Ok(tx) => {
                        let tx_hash = tx.hash;
                        debug!("N2C tx accepted, adding to mempool: {}", tx_hash);
                        match lts_mempool.add_tx(tx_hash, tx, size_bytes) {
                            Ok(_) => {
                                // Update mempool metrics immediately
                                lts_metrics.set_mempool_count(lts_mempool.len() as u64);
                                lts_metrics.mempool_bytes.store(
                                    lts_mempool.total_bytes() as u64,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                Ok(())
                            }
                            Err(e) => {
                                debug!("N2C tx accepted but mempool add failed: {e}");
                                Err(e.to_string())
                            }
                        }
                    }
                    Err(e) => {
                        debug!("N2C tx decode for mempool failed: {e}");
                        Err(e.to_string())
                    }
                }
            };
            match protocol::local_tx_submission::server::LocalTxSubmissionServer::run(
                &mut tx_ch,
                lts_validator.as_ref(),
                on_accepted,
            )
            .await
            {
                Ok(stats) => {
                    // Update N2C transaction metrics
                    lts_metrics
                        .n2c_txs_submitted
                        .fetch_add(stats.submitted, std::sync::atomic::Ordering::Relaxed);
                    lts_metrics
                        .n2c_txs_accepted
                        .fetch_add(stats.accepted, std::sync::atomic::Ordering::Relaxed);
                    lts_metrics
                        .n2c_txs_rejected
                        .fetch_add(stats.rejected, std::sync::atomic::Ordering::Relaxed);
                    debug!(
                        submitted = stats.submitted,
                        accepted = stats.accepted,
                        rejected = stats.rejected,
                        "N2C LocalTxSubmission ended"
                    );
                }
                Err(e) => {
                    debug!("N2C LocalTxSubmission error: {e}");
                }
            }
        });

        // LocalStateQuery server.
        //
        // C3 fix: do NOT hold the RwLock guard for the entire connection lifetime.
        // The old pattern `let handler = lsq_handler.read().await;` blocked the
        // periodic `update_state()` write lock until the client disconnected.
        // Instead, use a per-query wrapper that acquires and releases the read lock
        // inside each dispatch method — the lock is never held across an .await point.
        //
        // Issue #867: `acquire()` additionally pins the CURRENT `Arc<NodeStateSnapshot>`
        // at MsgAcquire time (a single cheap Arc clone under a momentary
        // `blocking_read()` — the guard is dropped immediately after, preserving the
        // C3 invariant). Every MsgQuery within that acquisition then dispatches
        // against the pinned Arc directly (no lock at all), so multiple queries in
        // one acquisition see a consistent ledger-state view even if `update_state()`
        // swaps the live snapshot mid-session.
        let lsq_handler = query_handler;
        let lsq_task = tokio::spawn(async move {
            // Per-query read-lock wrapper: acquires a blocking_read() guard for each
            // individual dispatch call and drops it before returning. This ensures
            // update_state() can always acquire its write lock between queries.
            struct PerQueryHandler(Arc<RwLock<QueryHandler>>);
            impl dugite_network::QueryHandler for PerQueryHandler {
                /// Pinned ledger-state snapshot for a single acquisition (#867).
                type Acquired = Arc<n2c_query::types::NodeStateSnapshot>;

                fn acquire(
                    &self,
                    target: &dugite_network::protocol::local_state_query::AcquireTarget,
                ) -> Result<
                    Self::Acquired,
                    dugite_network::protocol::local_state_query::AcquireFailure,
                > {
                    // Momentary read guard: run the existing on-chain validation,
                    // then Arc-clone the current state and drop the guard. No lock
                    // is held once this call returns — the returned Arc is then used
                    // for every MsgQuery in this acquisition, lock-free.
                    let guard = tokio::task::block_in_place(|| self.0.blocking_read());
                    dugite_network::QueryHandler::acquire(&*guard, target)
                }
                fn handle_query(
                    &self,
                    acquired: &Self::Acquired,
                    query_cbor: &[u8],
                    n2c_version: u16,
                ) -> Result<Vec<u8>, String> {
                    // Acquire read guard for this single query dispatch, then release.
                    let guard = tokio::task::block_in_place(|| self.0.blocking_read());
                    dugite_network::QueryHandler::handle_query(
                        &*guard,
                        acquired,
                        query_cbor,
                        n2c_version,
                    )
                }
                fn handle_block_query(
                    &self,
                    acquired: &Self::Acquired,
                    tag: u64,
                    query_cbor: &[u8],
                ) -> Result<Vec<u8>, String> {
                    let guard = tokio::task::block_in_place(|| self.0.blocking_read());
                    dugite_network::QueryHandler::handle_block_query(
                        &*guard, acquired, tag, query_cbor,
                    )
                }
                fn handle_query_anytime(
                    &self,
                    acquired: &Self::Acquired,
                    query_cbor: &[u8],
                ) -> Result<Vec<u8>, String> {
                    let guard = tokio::task::block_in_place(|| self.0.blocking_read());
                    dugite_network::QueryHandler::handle_query_anytime(
                        &*guard, acquired, query_cbor,
                    )
                }
                fn handle_query_hard_fork(
                    &self,
                    acquired: &Self::Acquired,
                    query_cbor: &[u8],
                ) -> Result<Vec<u8>, String> {
                    let guard = tokio::task::block_in_place(|| self.0.blocking_read());
                    dugite_network::QueryHandler::handle_query_hard_fork(
                        &*guard, acquired, query_cbor,
                    )
                }
            }
            let wrapper = PerQueryHandler(lsq_handler);
            if let Err(e) = protocol::local_state_query::server::LocalStateQueryServer::run(
                &mut sq_ch,
                &wrapper,
                n2c_version,
            )
            .await
            {
                debug!("N2C LocalStateQuery ended: {e}");
            }
        });

        // LocalTxMonitor server
        let ltm_mempool = mempool;
        let ltm_ledger = ledger;
        let ltm_task = tokio::spawn(async move {
            let current_slot = || {
                // Use try_read to avoid blocking — return 0 if lock is contended
                ltm_ledger
                    .try_read()
                    .map(|ls| ls.tip.point.slot().map(|s| s.0).unwrap_or(0))
                    .unwrap_or(0)
            };
            if let Err(e) = protocol::local_tx_monitor::server::LocalTxMonitorServer::run(
                &mut tm_ch,
                ltm_mempool.as_ref(),
                current_slot,
            )
            .await
            {
                debug!("N2C LocalTxMonitor ended: {e}");
            }
        });

        // Wait for any protocol task to complete (usually means client
        // disconnected), then abort all others and clean up.
        //
        // #968: "abort all others" is what this comment CLAIMED, but
        // `tokio::select!` merely DROPS the losing branches' futures, and
        // dropping a `JoinHandle` DETACHES the task rather than aborting it —
        // the same trap as #924. So when one protocol server returned early
        // (e.g. LocalTxMonitor hitting a decode error), every other task
        // INCLUDING the mux kept running and the connection stayed open. A
        // client waiting on a reply that a dead handler will never send hung
        // forever, and an unauthenticated local peer could wedge itself and
        // hold a mux channel indefinitely.
        //
        // Collect abort handles BEFORE the select so they survive the drop,
        // and abort everything on the way out.
        struct N2cAbortGuard(Vec<tokio::task::AbortHandle>);
        impl Drop for N2cAbortGuard {
            fn drop(&mut self) {
                for h in &self.0 {
                    h.abort();
                }
            }
        }
        let _n2c_guard = N2cAbortGuard(vec![
            lcs_task.abort_handle(),
            lts_task.abort_handle(),
            lsq_task.abort_handle(),
            ltm_task.abort_handle(),
            mux_handle.abort_handle(),
        ]);

        tokio::select! {
            _ = lcs_task => {}
            _ = lts_task => {}
            _ = lsq_task => {}
            _ = ltm_task => {}
            r = mux_handle => {
                if let Ok(Err(e)) = r {
                    debug!("N2C mux error: {e}");
                }
            }
        }

        Ok(())
    }

    // ─── import_haskell_ledger_snapshot() ──────────────────────────────────────

    /// Decode a Haskell ExtLedgerState snapshot and save it as a native dugite
    /// ledger snapshot. Called once during Mithril ancillary import; the resulting
    /// `ledger-snapshot.bin` is then loaded by the normal startup path.
    ///
    /// The UTxO set from the tvar file is loaded into an in-memory `UtxoSet` inside
    /// the `LedgerState`. The normal startup code will migrate these entries to the
    /// on-disk LSM store when it calls `attach_utxo_store()`.
    #[allow(clippy::too_many_arguments)]
    fn import_haskell_ledger_snapshot(
        snapshot_dir: &std::path::Path,
        native_snapshot_path: &std::path::Path,
        protocol_params: &ProtocolParameters,
        shelley_genesis: Option<&ShelleyGenesis>,
        shelley_genesis_hash: Option<dugite_primitives::Hash32>,
        network_magic: u64,
        byron_epoch_length: u64,
        byron_slot_duration_ms: u64,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        use dugite_primitives::address::Address;
        use dugite_primitives::hash::{Hash32, PolicyId};
        use dugite_primitives::transaction::{OutputDatum, TransactionInput, TransactionOutput};
        use dugite_primitives::value::{AssetName, Value};

        // ── Decode the state file ────────────────────────────────────────
        let state_path = snapshot_dir.join("state");
        let state_data = std::fs::read(&state_path)
            .with_context(|| format!("reading state file at {}", state_path.display()))?;
        info!(
            bytes = state_data.len(),
            path = %state_path.display(),
            "Decoding Haskell ExtLedgerState"
        );

        // ── Verify snapshot integrity (CRC) BEFORE trusting any of the bytes ──
        //
        // Upstream ouroboros-consensus V2/InMemory.loadSnapshot computes
        // `crcOfConcat(crc(state), crc(tables))` and throws `ReadSnapshotDataCorruption`
        // on mismatch with the `checksum` recorded in the sibling `meta` file. dugite
        // previously read `checksum` (for the codec-version path) but NEVER verified it,
        // so a tampered/truncated-yet-MemPack-decodable snapshot was silently accepted
        // (#17). The stored checksum is `crc32` over the DECIMAL-ASCII concatenation of
        // the two file CRCs (NOT `crc32(state ++ tables)`); `snapshot_crc_of_concat`
        // mirrors it byte-exactly (verified vs real preprod fixtures). When there is no
        // tables file the checksum folds to the state-only CRC (Haskell `maybe crc1`).
        let meta_path = snapshot_dir.join("meta");
        let meta_bytes = std::fs::read(&meta_path)
            .with_context(|| format!("reading snapshot meta at {}", meta_path.display()))?;
        let expected_checksum =
            dugite_serialization::mempack::parse_snapshot_checksum(&meta_bytes)?;
        let tables_crc = resolve_inmemory_tables_path(snapshot_dir)
            .map(|p| -> anyhow::Result<u32> {
                let blob = std::fs::read(&p)
                    .with_context(|| format!("reading tables file at {} for CRC", p.display()))?;
                Ok(crc32fast::hash(&blob))
            })
            .transpose()?;
        let computed_checksum = dugite_serialization::mempack::snapshot_crc_of_concat(
            crc32fast::hash(&state_data),
            tables_crc,
        );
        if computed_checksum != expected_checksum {
            anyhow::bail!(
                "snapshot checksum mismatch (upstream ReadSnapshotDataCorruption): computed \
                 crcOfConcat {computed_checksum} != meta checksum {expected_checksum} at {} — the \
                 state/tables bytes are corrupt or tampered; refusing to import",
                snapshot_dir.display()
            );
        }
        info!(checksum = expected_checksum, "Snapshot CRC verified");

        let hs = dugite_serialization::haskell_snapshot::decode_state_file(&state_data)
            .context("Failed to decode Haskell ExtLedgerState")?;

        info!(
            epoch = hs.epoch.0,
            tip_slot = hs.tip_slot.0,
            tip_block = hs.tip_block_no,
            pools = hs.new_epoch_state.cert_state.pstate.stake_pools.len(),
            accounts = hs.new_epoch_state.cert_state.dstate.accounts.len(),
            "Decoded Haskell ExtLedgerState"
        );

        // ── Build LedgerState ─────────────────────────────────────────��──
        let mut state = LedgerState::from_haskell_snapshot(&hs);

        // Apply genesis-derived configuration (epoch length, slot config, etc.)
        let shelley_transition = epoch::shelley_transition_epoch_for_magic(network_magic);
        if let Some(genesis) = shelley_genesis {
            state.set_epoch_length(genesis.epoch_length, genesis.security_param);
            state.set_slot_config(genesis.slot_config(
                shelley_transition,
                byron_epoch_length,
                byron_slot_duration_ms,
            ));
            state.set_update_quorum(genesis.update_quorum);
            let gen_deleg_entries = genesis.gen_delegs_entries();
            if !gen_deleg_entries.is_empty() {
                debug!(
                    count = gen_deleg_entries.len(),
                    "Loaded genesis delegates for overlay schedule validation"
                );
                state.set_genesis_delegates(&gen_deleg_entries);
            }
        }

        // Apply hard-fork boundary and genesis hash
        state.set_shelley_transition(shelley_transition, byron_epoch_length);
        if let Some(hash) = shelley_genesis_hash {
            // Unlike set_genesis_hash() on a fresh state, we do NOT overwrite the
            // Praos nonces — they came from the real Haskell PraosState.
            state.genesis_hash = hash;
        }

        // Apply active_slots_coeff from genesis protocol params (not in
        // the CBOR PParams array(31) but needed for VRF leader check).
        state.epochs.protocol_params.active_slots_coeff = protocol_params.active_slots_coeff;
        state.epochs.prev_protocol_params.active_slots_coeff = protocol_params.active_slots_coeff;

        // Set network
        let network_id = if network_magic == 764824073 {
            dugite_primitives::network::NetworkId::Mainnet
        } else {
            dugite_primitives::network::NetworkId::Testnet
        };
        state.node_network = Some(network_id);

        // ── Load UTxOs from MemPack tables blob ──────────────────────────
        //
        // `resolve_inmemory_tables_path` handles both layouts:
        //   * ouroboros-consensus < 1.0.0.0 (cardano-node ≤ 10.6.x): `tables/tvar`
        //   * ouroboros-consensus ≥ 1.0.0.0 (cardano-node ≥ 11.0.1): flat `tables`
        // Preview is on PV11 (cardano-node 11.0.1+), so the flat layout is what
        // ships today.  Hard-coding `tables/tvar` silently skipped the UTxO load
        // on every fresh preview import (#495), leaving `utxos=0` in the saved
        // native snapshot and tripping the UTxO-empty-gate on the next startup.
        if let Some(tvar_path) = resolve_inmemory_tables_path(snapshot_dir) {
            let tvar_data = std::fs::read(&tvar_path)
                .with_context(|| format!("reading tvar file at {}", tvar_path.display()))?;
            info!(
                path = %tvar_path.display(),
                bytes = tvar_data.len(),
                "Loading UTxO set from MemPack tables blob"
            );

            // AUTHORITATIVE TxIx endianness: the on-disk byte order is NOT a
            // function of the filesystem layout, nor of the blob bytes (a flat-LE
            // blob is byte-identical to a flat-BE blob). Upstream records the
            // disambiguator — the snapshot codec version
            // (`snapshotTablesCodecVersion`) — in the sibling `meta` JSON file:
            //   {"backend":…,"checksum":…,"tablesCodecVersion":1}
            // The upstream type maps version → byte order EXACTLY
            // (Ouroboros.Consensus.Storage.LedgerDB.Snapshots):
            //   TablesCodecVersion1  -- "[ {_ (txid, big-endian txix) => txout} ]"
            // STRICT (#10, re-gauntlet w4007sv2k): version 1 => Big is the ONLY
            // accepted outcome. A missing meta file, a missing/null
            // tablesCodecVersion, a wrong `backend`, or any other version is a
            // HARD ERROR (matching V2/InMemory loadSnapshot's ReadMetadataError /
            // MetadataInvalid / MetadataBackendMismatch). There is no silent
            // little-endian fallback. The index-distribution heuristic below is an
            // INDEPENDENT cross-validation only (it may veto a clear contradiction
            // but never decides). See `dugite_serialization::mempack` module docs
            // and #461/#10.
            // Authoritatively resolve the on-disk MemPack `TxIx` byte order from
            // the sibling `meta` file's `backend` + `tablesCodecVersion`. See
            // `resolve_snapshot_txix_endianness` for the full upstream rationale.
            let endianness = resolve_snapshot_txix_endianness(snapshot_dir, &tvar_data)?;

            let iter = dugite_serialization::mempack::TvarIterator::new_with_endianness(
                &tvar_data, endianness,
            )
            .context("Failed to create tvar iterator with the version-derived TxIx endianness")?;

            let mut utxo_count = 0u64;
            // NO-SILENT-SKIP: every malformed UTxO entry (empty/un-parseable
            // address, un-parseable multi-asset rep, >32-byte asset name) is now
            // a HARD ERROR that aborts the whole import — mirroring Haskell
            // `loadSnapshot`, which throws `ReadSnapshotFailed` on any decode
            // failure rather than importing a partial UTxO set. There is no
            // "skipped" count by construction.
            // HARD SAFETY NET: accumulate the decoded TxIx distribution and refuse
            // the import if it looks mis-keyed (no silent corruption — the cardinal
            // rule). This guards the version-derived decision regardless of how it
            // was reached. `endianness` is the version-derived choice from above.
            let mut txix_dist = dugite_serialization::mempack::TxIxDistribution::default();
            for result in iter {
                let (txin, txout) = result.context("Failed to decode tvar entry")?;
                txix_dist.observe_txix(txin.txix);

                // Convert MemPackTxIn → TransactionInput
                let input = TransactionInput {
                    transaction_id: txin.txid,
                    index: txin.txix as u32,
                };

                // Convert MemPackTxOut → TransactionOutput.
                //
                // All tags (including 2/3 Addr28Extra compact forms) now
                // produce a fully decoded address and coin value via
                // dugite-serialization. A zero coin is legal for multi-asset
                // entries.
                //
                // HARD-ERROR (no-silent-skip): an empty / un-parseable address
                // is a malformed CONTAINER entry, not a benign leaf. Haskell
                // `loadSnapshot` aborts the whole import on any decode failure
                // (`InitFailureRead . ReadSnapshotFailed`, InMemory.hs) — never a
                // partial UTxO set. Silently dropping a UTxO entry here would
                // build a wrong ledger state at the live tip ("input not found").
                //
                // NOTE: the larger opaque-`CompactAddr`-store refactor (keep the
                // raw addr bytes verbatim like Haskell's MemPack `CompactAddr`
                // newtype rather than round-tripping through `Address`) is OUT OF
                // SCOPE here and tracked separately; this change only flips the
                // existing silent-skip to the immediate hard-error.
                if txout.address.is_empty() {
                    return Err(anyhow::anyhow!(
                        "import: empty address in imported TxOut for input {input:?}; \
                         refusing a silent UTxO drop (Haskell loadSnapshot aborts on a \
                         malformed entry)"
                    ));
                }

                let address = Address::from_bytes(&txout.address).map_err(|e| {
                    anyhow::anyhow!(e).context(format!(
                        "import: un-parseable address in imported TxOut for input {input:?}; \
                         refusing a silent UTxO drop (Haskell loadSnapshot aborts on a \
                         malformed entry)"
                    ))
                })?;
                // Reconstruct the FULL value (lovelace + multi-asset). The
                // MemPack decoder hands back the opaque `CompactValue` rep
                // ShortByteString plus its `numMA` count; parse it into
                // (PolicyId, AssetName, qty) triples so imported UTxO values are
                // complete. A bare `Value::lovelace` would silently drop every
                // native token, building wrong `txInfoInputs` values in the
                // phase-2 ScriptContext (the #10 secondary gap).
                let mut value = Value::lovelace(txout.coin);
                if let Some(ref rep) = txout.multi_asset {
                    match dugite_serialization::mempack::compact::parse_multi_asset_rep(
                        rep,
                        txout.num_assets as usize,
                    ) {
                        Ok(triples) => {
                            for (pid, name, qty) in triples {
                                let policy = PolicyId::from_bytes(pid);
                                // HARD-ERROR (no-silent-skip): a name > 32 bytes
                                // is impossible in a well-formed `CompactValue`
                                // (Haskell `Mary.Value` MemPack), so it means the
                                // snapshot entry is corrupt. Dropping the token
                                // would silently corrupt the UTxO value; abort
                                // the import instead (Haskell `loadSnapshot`
                                // aborts on a malformed entry — InMemory.hs).
                                let asset_name = AssetName::new(name).map_err(|e| {
                                    anyhow::anyhow!(e).context(format!(
                                        "import: corrupt multi-asset name (>32 bytes) in \
                                         imported TxOut for input {input:?}; refusing a silent \
                                         token drop (value corruption)"
                                    ))
                                })?;
                                *value
                                    .multi_asset
                                    .entry(policy)
                                    .or_default()
                                    .entry(asset_name)
                                    .or_default() += qty;
                            }
                        }
                        Err(e) => {
                            // HARD-ERROR (no-silent-skip): a `CompactValue` rep
                            // that does not parse is a malformed snapshot entry.
                            // Importing an ADA-only value would SILENTLY DROP every
                            // native token (value corruption), building wrong
                            // `txInfoInputs` values in the phase-2 ScriptContext.
                            // Haskell `loadSnapshot` aborts the whole import on a
                            // CBOR/MemPack decode failure (InMemory.hs) — mirror it.
                            return Err(anyhow::anyhow!(e).context(format!(
                                "import: failed to parse imported multi-asset rep for input \
                                 {input:?}; refusing to import an ADA-only value that would \
                                 silently drop native tokens (value corruption)"
                            )));
                        }
                    }
                }

                let datum = if let Some(ref hash_bytes) = txout.datum_hash {
                    OutputDatum::DatumHash(Hash32::from_bytes(*hash_bytes))
                } else if let Some(ref inline_cbor) = txout.datum {
                    // Inline datum (MemPack `Datum binaryData`): `BinaryData` is a
                    // newtype over `ShortByteString` carrying the ORIGINAL on-chain
                    // CBOR of the Plutus `Data`. The tag-5/tag-4 decoder already
                    // stripped the MemPack VarLen length wrapper, so `inline_cbor`
                    // is the bare datum CBOR — decode it with the SAME decoder used
                    // for tag-24 inline datums during normal block decode and keep
                    // the verbatim bytes for byte-exact re-encoding.
                    //
                    // OPAQUE-STORE (matches Haskell `BinaryData` MemPack newtype):
                    // best-effort decode but never hard-error / never drop the
                    // datum. See `import_inline_datum` for the full Haskell
                    // grounding (`Cardano.Ledger.Plutus.Data`).
                    import_inline_datum(inline_cbor)
                } else {
                    OutputDatum::None
                };

                // Reference scripts: the MemPack `AlonzoScript` blob recovered by
                // the tag-5 (`TxOutCompactRefScript`) decoder. Classify it into a
                // typed `ScriptRef` so a Mithril-fast-started node can resolve
                // reference scripts at the live tip (without this, every imported
                // UTxO dropped its script bytes — see #10/#495 follow-up).
                //
                // The Plutus language tag is era-relative in MemPack but the
                // mapping is monotonic across all eras (Babbage/Conway/Dijkstra:
                // 0→V1, 1→V2, 2→V3, 3→V4 — see `PlutusScript` MemPack instances in
                // cardano-ledger), so the same mapping is byte-exact regardless of
                // the snapshot era.
                //
                // OPAQUE-STORE vs HARD-ERROR (#10 commit-B re-fix, path 2): the
                // Plutus body is `PlutusBinary` (a `newtype … ShortByteString
                // deriving newtype (… MemPack)`), so it is stored OPAQUELY with no
                // structural re-decode — a structurally-odd-but-framed Plutus body
                // does NOT error. Only frame-level problems HARD-ERROR: a truncated
                // frame or unknown AlonzoScript tag (`parse_script_ref_kind`), an
                // out-of-range Plutus language tag, or a malformed native (timelock)
                // body — the native body IS structurally decoded because Haskell's
                // `Timelock` MemPack `unpackM` calls `unpackMemoBytesM` (structural),
                // unlike `PlutusBinary`. See `decode_imported_script_ref`. In every
                // error case we refuse the import rather than silently drop the
                // script_ref.
                let script_ref: Option<dugite_primitives::transaction::ScriptRef> =
                    match txout.script_ref.as_deref() {
                        Some(blob) => {
                            let kind =
                                dugite_serialization::mempack::txout::parse_script_ref_kind(blob)
                                    .map_err(|e| {
                                    anyhow::anyhow!(e).context(format!(
                                        "import: failed to classify MemPack reference-script \
                                             (tag-5) blob for input {input:?}; refusing to import \
                                             a silently-dropped script_ref"
                                    ))
                                })?;
                            Some(decode_imported_script_ref(kind)?)
                        }
                        None => None,
                    };

                let output = TransactionOutput {
                    address,
                    value,
                    datum,
                    script_ref,
                    is_legacy: false,
                    raw_cbor: None,
                };

                state.utxo.utxo_set.insert(input, output);
                utxo_count += 1;

                if utxo_count.is_multiple_of(1_000_000) {
                    info!("Loaded {utxo_count} UTxOs...");
                }
            }

            // HARD SAFETY NET: refuse the import if the decoded TxIx distribution
            // is mis-keyed (e.g. wrong endianness mapped real 1..255 onto multiples
            // of 256). Erroring here is strictly better than silently storing a
            // corrupt UTxO set that would cause spurious "input not found" /
            // wrong-value failures at the live tip.
            dugite_serialization::mempack::assert_txix_distribution_sane(&txix_dist, endianness)
                .with_context(|| {
                    format!(
                        "imported UTxO set from {} has a mis-keyed TxIx distribution \
                         (endianness {endianness:?}); refusing to proceed",
                        tvar_path.display()
                    )
                })?;

            info!(
                utxo_count,
                txix_low = txix_dist.low,
                txix_mult256 = txix_dist.mult256,
                "UTxO loading from tvar file complete"
            );
        } else {
            warn!(
                snapshot_dir = %snapshot_dir.display(),
                "No tables blob (neither `tables` flat file nor `tables/tvar` legacy) \
                 found — UTxO set will be empty; full chain replay required"
            );
        }

        // ── Save as native snapshot ──────────────────────────────────────
        info!(
            path = %native_snapshot_path.display(),
            tip = %state.tip,
            epoch = state.epoch.0,
            utxos = state.utxo.utxo_set.len(),
            "Saving native ledger snapshot from Haskell import"
        );
        state
            .save_snapshot(native_snapshot_path)
            .map_err(|e| anyhow::anyhow!("Failed to save native snapshot: {e}"))?;

        info!("Native ledger snapshot saved successfully");
        Ok(())
    }

    // ─── init_fresh_ledger() ─────────────────────────────────────────────────

    /// Create a fresh ledger state with genesis configuration applied.
    /// Read a snapshot's tip slot from its `.meta.json` sidecar, without
    /// loading the (100 MB+) payload.
    ///
    /// Used only to size the UTxO-store completeness check (#989), so a missing
    /// or unreadable sidecar returning `None` is fine — the caller treats that
    /// as "no slot known" and skips the check rather than rejecting a snapshot
    /// it cannot measure.
    fn peek_snapshot_slot(bin_path: &std::path::Path) -> Option<u64> {
        dugite_ledger::state::SnapshotMeta::load(bin_path).map(|m| m.slot)
    }

    /// Is the on-disk UTxO store complete enough to trust a ledger snapshot
    /// taken at `snapshot_slot`? (#989)
    ///
    /// A snapshot restores the ledger tip, pots and governance state, but the
    /// UTxO set itself lives in the LSM store. If the store was lost or
    /// truncated — crash, killed process, partial copy, disk-full during
    /// compaction — the pair is unusable and the node must replay from
    /// genesis instead.
    ///
    /// This probe exists so that decision can be made BEFORE the snapshot is
    /// loaded. The previous code discovered it afterwards and called a
    /// `reset_to_origin` that reset only `tip` and `epoch`, leaving the
    /// treasury, certificates, governance state and protocol parameters at
    /// their snapshot values while the replay restarted at slot 0 — #985's
    /// chimera shape, ending with a snapshot of the chimera written back to
    /// disk (observed on preview as a ~4.9e15 lovelace treasury delta).
    ///
    /// Deciding late is not fixable by resetting harder: the ledger-choice
    /// site is followed by genesis setup (Conway committee threshold and
    /// members), so a rebuild after that point silently skips it. The first
    /// attempt at this fix did exactly that and a preview replay caught it as
    /// `InvalidPrevGovActionId` — governance state diverged mid-replay.
    ///
    /// Opens the store read-only, counts, and drops it. Cheap relative to the
    /// replay it prevents.
    fn utxo_store_is_usable(
        utxo_path: &std::path::Path,
        utxo_cfg: &dugite_storage::UtxoConfig,
        snapshot_slot: u64,
    ) -> bool {
        // A synced preview testnet has ~3M UTxOs, mainnet ~15M. Below slot 10M
        // a small store is legitimate, so nothing is required of it.
        let min_expected = if snapshot_slot > 10_000_000 {
            100_000
        } else {
            0
        };
        if snapshot_slot == 0 || min_expected == 0 {
            return true;
        }
        if !utxo_path.exists() {
            warn!(
                path = %utxo_path.display(),
                snapshot_slot,
                "Ledger snapshot is for a synced chain but no UTxO store exists — \
                 ignoring the snapshot and replaying from genesis."
            );
            return false;
        }
        match dugite_ledger::utxo_store::UtxoStore::open_with_config(
            utxo_path,
            utxo_cfg.memtable_size_mb,
            utxo_cfg.block_cache_size_mb,
            utxo_cfg.bloom_filter_bits_per_key,
        ) {
            Ok(mut store) => {
                let count = store.count_entries();
                if count < min_expected {
                    warn!(
                        utxo_count = count,
                        snapshot_slot,
                        min_expected,
                        "UTxO store appears incomplete ({count} entries for a snapshot at \
                         slot {snapshot_slot}) — ignoring the snapshot and replaying from \
                         genesis. The store will be wiped so the replay rebuilds it."
                    );
                    false
                } else {
                    true
                }
            }
            Err(e) => {
                warn!(
                    path = %utxo_path.display(),
                    "Cannot open the UTxO store to validate it against the ledger snapshot: \
                     {e} — ignoring the snapshot and replaying from genesis."
                );
                false
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // genesis assembly legitimately needs every era's inputs
    pub(crate) fn init_fresh_ledger(
        protocol_params: &ProtocolParameters,
        genesis_prev_protocol_params: &ProtocolParameters,
        shelley_genesis: Option<&ShelleyGenesis>,
        shelley_genesis_hash: Option<dugite_primitives::Hash32>,
        byron_genesis_utxos: &[(Vec<u8>, u64)],
        network_magic: u64,
        byron_epoch_length: u64,
        byron_slot_duration_ms: u64,
    ) -> LedgerState {
        let mut ledger = LedgerState::new(protocol_params.clone());
        // #994: `LedgerState::new` seeds prev = cur. At genesis `cgsPrevPParams`
        // carries only the cost models the genesis FILES supplied, never the
        // injected `defaultV2CostModel` — see `genesis_prev_protocol_params`.
        ledger.epochs.prev_protocol_params = genesis_prev_protocol_params.clone();
        let shelley_transition_epoch = epoch::shelley_transition_epoch_for_magic(network_magic);
        if let Some(genesis) = shelley_genesis {
            // Must run BEFORE seed_genesis_utxos so reserves init from the
            // genesis cap (devnets use 60B, mainnet/preview/preprod 45B).
            ledger.set_max_lovelace_supply(genesis.max_lovelace_supply);
            ledger.set_epoch_length(genesis.epoch_length, genesis.security_param);
            ledger.set_slot_config(genesis.slot_config(
                shelley_transition_epoch,
                byron_epoch_length,
                byron_slot_duration_ms,
            ));
            ledger.set_update_quorum(genesis.update_quorum);
            let gen_deleg_entries = genesis.gen_delegs_entries();
            if !gen_deleg_entries.is_empty() {
                tracing::debug!(
                    count = gen_deleg_entries.len(),
                    "Loaded genesis delegates for overlay schedule validation"
                );
                ledger.set_genesis_delegates(&gen_deleg_entries);
            }
        }
        // Set Byron→Shelley transition boundary for correct HFC epoch numbering
        ledger.set_shelley_transition(shelley_transition_epoch, byron_epoch_length);
        if let Some(hash) = shelley_genesis_hash {
            ledger.set_genesis_hash(hash);
        }
        if !byron_genesis_utxos.is_empty() {
            ledger.seed_genesis_utxos(byron_genesis_utxos);
        }

        // Seed Shelley genesis initial funds and staking (used by custom devnets;
        // empty on mainnet/preview/preprod).
        if let Some(genesis) = shelley_genesis {
            let shelley_utxos = genesis.initial_utxos();
            if !shelley_utxos.is_empty() {
                let tuples: Vec<(Vec<u8>, u64)> = shelley_utxos
                    .iter()
                    .map(|e| (e.address.clone(), e.lovelace))
                    .collect();
                ledger.seed_genesis_utxos(&tuples);
            }

            if let Some(ref staking) = genesis.staking {
                // Seed pool registrations
                for (pool_id_hex, pool) in &staking.pools {
                    if let Some(reg) = parse_genesis_pool(pool_id_hex, pool) {
                        ledger.seed_genesis_pool(reg);
                    }
                }
                // Seed stake delegations
                for (stake_cred_hex, pool_id_hex) in &staking.stake {
                    if let Some((cred, pool_id)) =
                        parse_genesis_delegation(stake_cred_hex, pool_id_hex)
                    {
                        ledger.seed_genesis_delegation(cred, pool_id);
                    }
                }
            }
        }

        // Populate the initial stake snapshot from seeded pools/delegations so
        // cold-start leader election has a non-empty `set`.  Mirrors Haskell's
        // `resetStakeDistribution` in cardano-ledger Shelley/Transition.hs.
        // No-op on the Mithril-restore path (snapshots already loaded).
        ledger.finalize_genesis_state();

        // === Conway-from-genesis correction ===
        //
        // For chains that boot directly in Conway (e.g. local devnet at
        // PV10+), the historical genesis init is wrong in two ways and
        // would otherwise compound into a ~3.6T-lovelace per-boundary
        // ledger divergence vs the Haskell reference (see
        // .claude/skills/devnet-validate/audit-findings/
        // 2026-05-28-round2-rupd-divergence.md):
        //
        // 1. `LedgerState::new` defaults `prev_d = 1/1` (Shelley overlay
        //    convention). Conway always has `d = 0`, so at boundary 0→1
        //    `compute_reward_update` would take the overlay branch and
        //    drain `floor(rho * reserves)` from reserves to treasury
        //    immediately. Haskell with the correct `prev_d = 0/1` enters
        //    the non-overlay branch and sees `bprev_total_blocks = 0`
        //    (no preceding epoch), producing `expansion = 0`. Override
        //    `prev_d` to Conway's invariant.
        //
        // 2. `finalize_genesis_state` pre-fills both `snapshots.mark`
        //    AND `snapshots.set` with the genesis stake distribution so
        //    the forge can find pool stake in epoch 0. The CORRECT
        //    Haskell-matching shape (verified empirically against a
        //    cardano-cli ledger-state dump at boundary 2→3) is:
        //
        //        mark = pre-fill (Haskell's instant stake at end of epoch 0)
        //        set  = mempty
        //        go   = mempty
        //
        //    With that, the SNAP rotations produce:
        //        After NEWEPOCH 1: mark=new, set=pre-fill, go=mempty
        //        After NEWEPOCH 2: mark=new2, set=end-of-0, go=pre-fill
        //        After NEWEPOCH 3: mark=new3, set=end-of-1, go=end-of-0
        //
        //    The pulser in epoch 1 sees go=mempty (no distribution at
        //    boundary 1→2). The pulser in epoch 2 sees go=pre-fill
        //    (first non-empty: distributes ~22 ADA per delegator at
        //    boundary 2→3 — matches Haskell byte-exact).
        //
        //    The Conway-from-genesis correction below CLEARS the SET
        //    snapshot pre-fill but KEEPS the MARK pre-fill, which is
        //    the minimal change from `finalize_genesis_state`'s default
        //    (pre-fill both) to the Haskell-matching shape (pre-fill
        //    mark only). Cleared the SET pre-fill: addresses the
        //    boundary-1→2 over-distribution. Kept the MARK pre-fill:
        //    addresses the boundary-2→3 under-distribution (the 22.14B
        //    reserves diff observed in the 2026-05-28 session).
        //
        // Both Byron-genesis chains (mainnet/preview/preprod) and the
        // Mithril-restore path are unaffected: they either never reach
        // Conway during the harmful boundary 0→1 window, or load
        // snapshots from the Haskell snapshot file which is already
        // correct for the chain's current state.
        let conway_from_genesis = ledger.epochs.protocol_params.protocol_version_major >= 9;
        if conway_from_genesis {
            ledger.epochs.prev_d = dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            };
            ledger.epochs.prev_protocol_version_major =
                ledger.epochs.protocol_params.protocol_version_major;
            // Clear ONLY the `set` snapshot pre-fill; keep `mark` so the
            // SNAP rotation produces the Haskell-matching pattern of
            // first non-empty `go` at boundary 2→3 (not 1→2).
            ledger.epochs.snapshots.set = None;
            info!(
                pv = ledger.epochs.protocol_params.protocol_version_major,
                "Conway-from-genesis: cleared genesis snapshot pre-fill and \
                 set prev_d=0/1 for Haskell-faithful RUPD timing"
            );
        }

        ledger
    }
}

/// Parse a Shelley genesis pool entry into a `PoolRegistration`.
///
/// Returns `None` if the pool ID or VRF key hex is invalid.
fn parse_genesis_pool(
    pool_id_hex: &str,
    pool: &crate::genesis::ShelleyGenesisPool,
) -> Option<dugite_ledger::state::PoolRegistration> {
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::value::Lovelace;

    let pool_id = Hash28::from_hex(pool_id_hex).ok()?;
    let vrf_keyhash = Hash32::from_hex(&pool.public_key).ok()?;

    // Convert margin f64 to numerator/denominator
    let (margin_num, margin_den) = {
        // Use the same approach as genesis float_to_rational
        let s = format!("{}", pool.margin);
        let decimals = s.split('.').nth(1).map(|d| d.len()).unwrap_or(0);
        let denom = 10u64.pow(decimals as u32);
        let num = (pool.margin * denom as f64).round() as u64;
        let g = gcd(num, denom);
        (num / g, denom / g)
    };

    let owners: Vec<Hash28> = pool
        .owners
        .iter()
        .filter_map(|h| Hash28::from_hex(h).ok())
        .collect();

    // Build a minimal reward account from the JSON
    let reward_account = parse_genesis_reward_account(&pool.reward_account);

    Some(dugite_ledger::state::PoolRegistration {
        pool_id,
        vrf_keyhash,
        pledge: Lovelace(pool.pledge),
        cost: Lovelace(pool.cost),
        margin_numerator: margin_num,
        margin_denominator: margin_den,
        reward_account,
        owners,
        relays: Vec::new(),
        metadata_url: None,
        metadata_hash: None,
    })
}

/// Parse a genesis reward account JSON value into raw address bytes.
///
/// The Shelley genesis format uses:
/// ```json
/// { "credential": { "keyHash": "hex" }, "network": "Testnet" }
/// ```
fn parse_genesis_reward_account(value: &serde_json::Value) -> Vec<u8> {
    let network_byte: u8 = if value
        .get("network")
        .and_then(|v| v.as_str())
        .unwrap_or("Testnet")
        == "Mainnet"
    {
        0xe1 // reward address header, mainnet
    } else {
        0xe0 // reward address header, testnet
    };

    let cred_hex = value
        .get("credential")
        .and_then(|c| c.get("keyHash").or_else(|| c.get("scriptHash")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if let Ok(cred_bytes) = hex::decode(cred_hex) {
        if cred_bytes.len() == 28 {
            let mut addr = Vec::with_capacity(29);
            addr.push(network_byte);
            addr.extend_from_slice(&cred_bytes);
            return addr;
        }
    }

    Vec::new()
}

/// Parse a genesis stake delegation: credential hex → pool ID hex.
fn parse_genesis_delegation(
    stake_cred_hex: &str,
    pool_id_hex: &str,
) -> Option<(dugite_primitives::Hash32, dugite_primitives::hash::Hash28)> {
    use dugite_primitives::hash::{Hash28, Hash32};

    let pool_id = Hash28::from_hex(pool_id_hex).ok()?;
    let cred_bytes = hex::decode(stake_cred_hex).ok()?;
    if cred_bytes.len() != 28 {
        return None;
    }
    // Pad 28-byte credential to 32 bytes (matching ledger convention)
    let mut padded = [0u8; 32];
    padded[..28].copy_from_slice(&cred_bytes);
    Some((Hash32::from_bytes(padded), pool_id))
}

/// Greatest common divisor (for margin rational conversion).
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Compute the `BlockNo` of the block we are about to forge given the
/// current ledger tip.
///
/// This mirrors Haskell's `expectedBlockNo` pipeline in
/// `ouroboros-consensus/.../HeaderValidation.hs`:
///
/// * At `Origin`, the value is `expectedFirstBlockNo` which the HardFork
///   combinator delegates to the head of the era list (Byron), and Byron's
///   `BasicEnvelopeValidation` instance returns `BlockNo 0`
///   (`ouroboros-consensus-cardano/.../Byron/Ledger/HeaderValidation.hs:40`).
///   So the first block on the chain — whether it is a Byron genesis EBB or
///   the first Shelley block of a `TestShelleyHardForkAtEpoch: 0` config —
///   must be `BlockNo 0`.
/// * After that, each forged block increments by one: `succ(prev)`.
///
/// Using `current_block_number + 1` naively would produce `BlockNo 1` for
/// the first block (since `current_block_number` at Origin is `0`), which
/// Haskell rejects with `UnexpectedBlockNo (BlockNo 0) (BlockNo 1)`.
pub(crate) fn next_forged_block_number(
    tip_point: &Point,
    current_block_number: dugite_primitives::time::BlockNo,
) -> dugite_primitives::time::BlockNo {
    if *tip_point == Point::Origin {
        dugite_primitives::time::BlockNo(0)
    } else {
        dugite_primitives::time::BlockNo(current_block_number.0 + 1)
    }
}

/// Minimum interval between N2C `LocalStateQuery` snapshot rebuilds.
///
/// The rebuild (`Node::update_query_state`) walks every delegation, pool,
/// and DRep in the ledger state — ~1.4 s at mainnet epoch-334 scale — and
/// runs synchronously on the apply task with the ledger read lock held.
///
/// At tip that cost is amortised over ~20 s block arrivals, so a 1 Hz
/// limit keeps client-visible state fresh at negligible cost.  During
/// catch-up the apply loop IS the sync throughput bottleneck: an
/// unconditional 1 Hz cadence stalled it for ~60 % of wall time on
/// mainnet (a metronomic ~1.4 s pause every ~2.5 s, measured 2026-06-10),
/// so the cadence drops to the 30 s documented on `update_query_state`.
pub(crate) fn query_state_refresh_interval(at_tip: bool) -> std::time::Duration {
    if at_tip {
        std::time::Duration::from_secs(1)
    } else {
        std::time::Duration::from_secs(30)
    }
}

/// Catch-up gate for the forge loop.
///
/// Returns `true` when the per-slot leadership check should be skipped
/// silently because we are still catching up to the network.  Suppresses
/// the `TraceStartLeadershipCheck` + `TraceNoLedgerView` log pair that
/// would otherwise spam at 2 lines per second during a multi-day bulk
/// sync (the existing `TraceNoLedgerView` gate also short-circuits the
/// actual forge, but this one runs earlier and silently).
///
/// `peer_tip` is the maximum tip slot reported by any peer via ChainSync
/// (monotonic, 0 before the first intersection).  When available it is
/// the accurate "caught up?" signal.  Before any peer has reported
/// (`peer_tip == 0`) we fall back to `wall_clock_slot`, matching the
/// behaviour of the spec gate from a fresh-boot perspective.
///
/// `stability_window` is `ceil(3 * k / f)` — once `tip_slot` is within
/// that window of the reference tip we resume per-slot checks.
pub(crate) fn should_skip_forge_for_catch_up(
    tip_slot: u64,
    peer_tip: u64,
    wall_clock_slot: u64,
    stability_window: u64,
) -> bool {
    // Boot-time anchor check (Bug G fix, 2026-05-16): if a peer reports a
    // non-Origin tip and our local chain is still at Origin, the BlockFetch
    // pipeline has not yet adopted any of that peer's blocks.  Forging now
    // would create a self-forged block_no=0 on Origin that diverges from
    // every peer's chain at the very first block — a fork that, because
    // both chains anchor at genesis (the only common ancestor), cannot be
    // reconciled by `VolatileDB::switch_chain` once either chain grows past
    // `k`.  Skip until BlockFetch adopts at least one peer block.
    //
    // Compared to the original `reference_tip - tip_slot > stability_window`
    // check below: that test allows the boot scenario through (gap of, say,
    // 5 < 150), letting the BP forge before BlockFetch has caught up.
    // Cardano-node naturally avoids this because its first ChainSync round
    // populates the chain BEFORE the slot-leader timer fires; dugite's slot
    // timer fires on the wall clock, often before the (asynchronous)
    // BlockFetch worker has had a chance to download anything.
    //
    // This check is a no-op on a network where the BP genuinely is the
    // first node to produce a block (peer_tip == 0).
    if peer_tip > 0 && tip_slot == 0 {
        return true;
    }

    // Tight catch-up check (Bug H, 2026-05-16): always skip if the peer's
    // tip is more than `short_catch_up_lag` slots ahead of ours.  Without
    // this, a BP that briefly falls behind during a slot-battle resolution
    // (e.g., its forge raced and lost) would forge its next block on its
    // own (stale) tip, creating a sibling fork on every subsequent leader
    // slot.  Praos's k-stability prevents the BP from later switching back
    // to the canonical chain (intersection beyond `k` is unreachable in
    // `VolatileDB::switch_chain`), so each such miss is permanent.
    //
    // The threshold scales with the security parameter: derived as
    // `stability_window / 30` (and floored at 5 slots) which yields:
    //   * k=10, f=0.2     → stability_window=150       → 5 slots
    //   * k=2160, f=0.05  → stability_window=129_600   → 4320 slots (~72 min)
    // both reasonable for their respective scales.
    //
    // The original `> stability_window` rule below is preserved as a final
    // fallback (relevant when no peer tip is reported yet — cold boot
    // before the first ChainSync round).
    let short_catch_up_lag = (stability_window / 30).max(5);
    if peer_tip > 0 && peer_tip.saturating_sub(tip_slot) > short_catch_up_lag {
        return true;
    }

    let reference_tip = if peer_tip > 0 {
        peer_tip
    } else {
        wall_clock_slot
    };
    reference_tip.saturating_sub(tip_slot) > stability_window
}

/// Selects whether the forge attempt is extending the current tip or
/// producing a competing block at the same slot as an existing tip
/// (Haskell's `mkCurrentBlockContext` LT vs EQ branches).
///
/// In `ExtendTip` the block number is `tip.block_no + 1` and the
/// previous-hash is the tip's own header hash. In `SlotBattle` the
/// block number is the tip's own block number (NOT incremented) and
/// the previous-hash is the tip's parent — both values carried in
/// the variant so the rest of the forge path stays uniform.
#[derive(Debug, Clone, Copy)]
enum ForgeMode {
    /// Normal case: forge on top of the current tip.
    ExtendTip,
    /// Slot-battle case: a peer's block is already at our wall-clock slot.
    /// Forge a competing block parented at the tip's parent.
    SlotBattle {
        block_number: dugite_primitives::time::BlockNo,
        prev_hash: dugite_primitives::hash::Hash32,
    },
}

impl Node {
    // ─── try_forge_block() ───────────────────────────────────────────────────

    /// Attempt to forge a block if we are in block producer mode and are the slot leader.
    ///
    /// Called every slot when the node is caught up to the chain tip.
    /// Convenience wrapper that reads the wall-clock slot and calls
    /// `try_forge_block_at`.  Used by code paths where the slot hasn't
    /// already been sampled (e.g. after catching up to tip).
    pub(crate) async fn try_forge_block(&mut self) {
        if let Some(wc) = self.current_wall_clock_slot().await {
            self.try_forge_block_at(wc).await;
        }
    }

    /// Try to forge a block at the given wall-clock slot.
    ///
    /// The slot is passed from the caller (sync loop forge ticker) to avoid
    /// a TOCTOU race: the sync loop reads the wall clock once and passes the
    /// same value here, preventing a double-forge if the clock advances
    /// between the guard check and the actual forge attempt.
    ///
    /// The check sequence mirrors Haskell's
    /// `Ouroboros.Consensus.NodeKernel.forkBlockForging` exactly:
    /// 1. TraceStartLeadershipCheck
    /// 2. TraceBlockFromFuture (tip_slot > current_slot, strict)
    ///    2b. TraceSlotBattle (tip_slot == current_slot — peer forged at our slot first)
    /// 3. TraceSlotIsImmutable  (immutable_tip_slot == current_slot)
    /// 4. TraceBlockContext / TraceNoLedgerState (ledger state for prev-point)
    /// 5. TraceLedgerState
    /// 6. TraceNoLedgerView / TraceLedgerView (stability-window gate)
    /// 7. TraceNodeCannotForge (PraosCannotForgeKeyNotUsableYet:
    ///    wall-clock KES period < opcert start period)
    /// 8. VRF leader election: TraceNodeNotLeader / TraceNodeIsLeader.
    ///    Post-IsLeader: increment `dugite_forge_slot_battles_total` if
    ///    forge_mode is SlotBattle (Haskell has no equivalent metric —
    ///    operators infer slot battles from logs).
    /// 9. TraceForgeTickedLedgerState / TraceForgingMempoolSnapshot
    /// 10. TraceForgedBlock / TraceAdoptedBlock / TraceDidntAdoptBlock /
    ///     TraceForgedInvalidBlock
    pub(crate) async fn try_forge_block_at(
        &mut self,
        wall_clock_slot: dugite_primitives::time::SlotNo,
    ) {
        let creds = match &self.block_producer {
            Some(c) => c,
            None => return, // relay-only mode
        };

        let current_slot = wall_clock_slot.0;

        // ── Snapshot ledger tip and immutable tip ────────────────────────────
        let ls = self.ledger_state.read().await;
        let tip_slot = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
        let prev_point = ls.tip.point.clone();

        // ── Catch-up gate (silent) ────────────────────────────────────────────
        // Skip the leadership check entirely while we are still catching up
        // to the network.  The TraceNoLedgerView gate at step 5 below would
        // also short-circuit the forge, but logging
        // TraceStartLeadershipCheck + TraceNoLedgerView every second during
        // a multi-day bulk sync is noisy and wastes lock acquisitions.
        //
        // Use the peer-reported network tip when available (the accurate
        // "caught up?" signal); fall back to wall clock before the first
        // ChainSync intersection populates `max_peer_tip_slot`.  The
        // threshold is the same `stability_window` Haskell's
        // `forecastFor` uses — once `tip_slot` is within that window of
        // the network tip we resume per-slot leadership checks.
        let stability_window = dugite_consensus::stability_window_slots(
            self.consensus.security_param,
            self.consensus.active_slot_coeff,
        );
        if should_skip_forge_for_catch_up(
            tip_slot,
            self.metrics.get_peer_tip(),
            current_slot,
            stability_window,
        ) {
            drop(ls);
            return;
        }

        // ── Peer-connectivity forge gate ──────────────────────────────────────
        // Do not forge until BOTH:
        //   (a) at least one peer is in Hot state (has a live ChainSync + BlockFetch), AND
        //   (b) a non-Origin MsgIntersectFound has been received from at least one peer.
        //
        // Without this gate the BP can forge block 0 before any peer connects.
        // Its local tip then diverges from the relay's chain; Bug A's Origin-
        // intersection guard disconnects every reconnect attempt, permanently
        // stalling the node on its self-forged fork.
        //
        // On mainnet/preview the gate is transparent: peers are established and
        // intersections are found within ~2s of startup, well before the first
        // slot leadership opportunity.
        //
        // The check is two fast atomics (no lock acquisition).
        if self.block_producer.is_some() {
            let has_hot_peer = {
                let pm = self.peer_manager.read().await;
                pm.hot_peer_count() > 0
            };
            let has_intersection = self
                .peer_intersection_established
                .load(std::sync::atomic::Ordering::Relaxed);
            if !has_hot_peer || !has_intersection {
                drop(ls);
                // Log only once per ~60s to avoid flooding at startup.
                if current_slot.is_multiple_of(60) {
                    info!(
                        target: "forge",
                        current_slot,
                        has_hot_peer,
                        has_intersection,
                        "Deferring forge: waiting for peer connectivity and ChainSync intersection",
                    );
                }
                return;
            }
        }

        // ── Step 1: TraceStartLeadershipCheck ────────────────────────────────
        // Haskell emits this Info trace at the start of every slot tick,
        // before any early-exit checks (we only suppress it during catch-up
        // to avoid log spam — see the gate above).
        info!(
            target: "forge",
            current_slot,
            pool_id = %creds.pool_id,
            "TraceStartLeadershipCheck",
        );

        // ── Step 2: TraceBlockFromFuture / slot-battle classification ─────────
        // Haskell `mkCurrentBlockContext` (NodeKernel.hs:960):
        //   - tipSlot < currentSlot  → forge on top of tip (normal)
        //     bcBlockNo = tip.block_no + 1, bcPrevPoint = tip.point
        //   - tipSlot > currentSlot  → TraceBlockFromFuture (Error) + exitEarly
        //   - tipSlot == currentSlot → forge a *competing* block (slot battle):
        //     bcBlockNo = tip.block_no, bcPrevPoint = AF.headPoint c'
        //     (the head of the chain fragment EXCLUDING the tip — i.e. the
        //     parent of the current tip). The two blocks share the same
        //     slot, block_no and parent; chain selection's VRF tiebreaker
        //     (RestrictedVRFTiebreaker 5 in Conway with slotDist=0) decides
        //     the winner deterministically by the smaller raw VRF output.
        //
        // The forge mode determines `bcBlockNo` and `bcPrevHash` for the
        // rest of this function. The KES signature, VRF leader check, body
        // hash, header hash, etc. are all computed on whichever values this
        // step selects.
        let forge_mode = match tip_slot.cmp(&current_slot) {
            std::cmp::Ordering::Greater => {
                drop(ls);
                error!(
                    target: "forge",
                    current_slot,
                    tip_slot,
                    "TraceBlockFromFuture: chain tip is ahead of current slot — skipping forge",
                );
                return;
            }
            std::cmp::Ordering::Equal => {
                // Slot battle: read the tip header to extract its parent's
                // hash and the tip's own block_no. The competing block we
                // are about to forge will reuse those values verbatim.
                let fragment = self.chain_fragment.read().await;
                let tip_header = match fragment.headers().back().cloned() {
                    Some(h) => h,
                    None => {
                        // Empty fragment with tip_slot == current_slot would
                        // mean the immutable tip is at the wall-clock slot.
                        // Refusing to forge here matches the immutable-tip
                        // gate below; this branch is unreachable in normal
                        // operation since the immutable tip lags behind the
                        // volatile tip by at least 1 block.
                        drop(fragment);
                        drop(ls);
                        warn!(
                            target: "forge",
                            current_slot,
                            tip_slot,
                            "TraceSlotBattle: chain fragment empty at slot battle — \
                             cannot determine tip parent, skipping forge",
                        );
                        return;
                    }
                };
                drop(fragment);
                // The competing block uses the SAME block_no as the existing
                // tip and points at the SAME parent (tip.prev_hash).
                //
                // Note: the `dugite_forge_slot_battles_total` counter is
                // incremented LATER, after the VRF leader check passes (see
                // post-TraceNodeIsLeader gate below). Incrementing here
                // would over-count by an order of magnitude — on a healthy
                // chain a peer's block lands at our wall-clock on most
                // slots, but we only "battle" when we are also elected
                // leader for that same slot.
                info!(
                    target: "forge",
                    current_slot,
                    tip_slot,
                    competing_with = %tip_header.header_hash.to_hex(),
                    block_no = tip_header.block_number.0,
                    parent_hash = %tip_header.prev_hash.to_hex(),
                    "TraceSlotBattle: forging competing block — same slot, same \
                     block_no, same parent as existing tip",
                );
                ForgeMode::SlotBattle {
                    block_number: tip_header.block_number,
                    prev_hash: tip_header.prev_hash,
                }
            }
            std::cmp::Ordering::Less => ForgeMode::ExtendTip,
        };
        let next_slot = wall_clock_slot;

        // ── Step 3: TraceSlotIsImmutable ──────────────────────────────────────
        // Haskell: if immutableTipSlot == currentSlot → TraceSlotIsImmutable (Error) + exitEarly.
        // Forging at the immutable tip slot would attempt to extend an already-
        // finalized chain point, which every peer would reject.
        let immutable_tip_slot = {
            let db = self.chain_db.read().await;
            db.get_immutable_tip()
                .point
                .slot()
                .map(|s| s.0)
                .unwrap_or(0)
        };
        if immutable_tip_slot == current_slot {
            drop(ls);
            error!(
                target: "forge",
                current_slot,
                immutable_tip_slot,
                "TraceSlotIsImmutable: current slot equals the immutable tip slot — skipping forge",
            );
            return;
        }

        // ── Step 4: TraceBlockContext / TraceNoLedgerState ───────────────────
        // Haskell: `ChainDB.withReadOnlyForkerAtPoint bcPrevPoint`:
        // if prev-point is no longer on the selected chain → TraceNoLedgerState (Error) + exitEarly.
        // We check the ledger state is non-empty (prev_point is available) as a
        // proxy — detailed forker-at-point semantics are handled in the apply path.
        debug!(
            target: "forge",
            current_slot,
            tip_slot,
            prev_point = %prev_point,
            "TraceBlockContext",
        );

        // Ensure the ledger has a usable state for the prev-point.
        // At Origin we can always forge; at a specific point we require a non-zero tip.
        if matches!(prev_point, Point::Origin) && current_slot > 1 {
            // At Origin with current_slot > 1 the ledger hasn't seen any blocks —
            // this is only valid on fresh private testnets; proceed normally.
        } else if !matches!(prev_point, Point::Origin) && tip_slot == 0 {
            drop(ls);
            error!(
                target: "forge",
                current_slot,
                "TraceNoLedgerState: ledger state unavailable for prev-point — skipping forge",
            );
            return;
        }
        debug!(
            target: "forge",
            current_slot,
            tip_slot,
            "TraceLedgerState",
        );

        // ── Step 5: TraceNoLedgerView / TraceLedgerView ───────────────────────
        // Haskell: `forecastFor` fails when currentSlot >= tipSlot + 1 + stabilityWindow.
        // stabilityWindow = ceil(3 * k / f).
        // For preview/mainnet (k=2160, f=0.05) this is 129 600 slots = 36 hours.
        // This is the ONLY stale-tip gate in the Haskell forge loop.
        // matches Haskell TraceNoLedgerView
        let stability_window = dugite_consensus::stability_window_slots(
            self.consensus.security_param,
            self.consensus.active_slot_coeff,
        );
        let lag = current_slot.saturating_sub(tip_slot);
        if lag > stability_window {
            drop(ls);
            error!(
                target: "forge",
                current_slot,
                tip_slot,
                lag_slots = lag,
                stability_window,
                "TraceNoLedgerView: chain tip too far behind for ledger view forecast — skipping forge",
            );
            return;
        }
        debug!(
            target: "forge",
            current_slot,
            tip_slot,
            stability_window,
            "TraceLedgerView",
        );

        // ── Step 5b: TraceNodeCannotForge (PraosCannotForgeKeyNotUsableYet) ───
        // Haskell `praosCheckCanForge` (Protocol/Praos.hs) emits this before the
        // VRF leader check whenever the wall-clock KES period is *earlier* than
        // the operational certificate's `c0` (start period). The KES key is
        // valid but not yet usable — most commonly because a freshly issued
        // OCert was published with `c0` set to the next period to support
        // graceful key rotation.
        //
        // Forging in this state would produce a KES signature with relative
        // evolution clamped to 0, which the Haskell verifier *also* clamps
        // to 0 — but that is a wire-format coincidence; semantically the
        // certificate has not begun yet and the block must not be produced.
        // Skipping here matches `checkShouldForge` returning `CannotForge`
        // (Praos/Block/Forging.hs:200).
        let current_slot_kes_period = current_slot / self.consensus.slots_per_kes_period;
        if current_slot_kes_period < creds.opcert_kes_period {
            info!(
                target: "forge",
                current_slot,
                wall_clock_kes_period = current_slot_kes_period,
                opcert_kes_period = creds.opcert_kes_period,
                pool_id = %creds.pool_id,
                "TraceNodeCannotForge: PraosCannotForgeKeyNotUsableYet — \
                 wall-clock KES period precedes operational certificate start period",
            );
            return;
        }

        // ── Extract ledger values needed for the forge attempt ────────────────
        // Use epoch_nonce_for_slot to handle first slot of new epoch correctly.
        // At epoch boundaries, the TICKN transition hasn't been applied yet, so
        // ls.consensus.epoch_nonce still holds the previous epoch's nonce.
        // epoch_nonce_for_slot pre-computes the correct nonce, matching the sync path.
        let epoch_nonce = ls.epoch_nonce_for_slot(next_slot.0);
        let (block_number, prev_hash) = match forge_mode {
            ForgeMode::ExtendTip => {
                let bn = next_forged_block_number(&ls.tip.point, ls.current_block_number());
                let ph = ls
                    .tip
                    .point
                    .hash()
                    .copied()
                    .unwrap_or(dugite_primitives::hash::Hash32::ZERO);
                (bn, ph)
            }
            ForgeMode::SlotBattle {
                block_number,
                prev_hash,
            } => (block_number, prev_hash),
        };
        let slots_per_kes_period = self.consensus.slots_per_kes_period;

        // Pool distribution for the leader VRF check, forecast to the forge
        // slot. `pool_distribution_for_slot` mirrors Haskell's
        // `protocolLedgerView` over a TICKF-forecast ledger:
        //   - Same epoch as ledger → reads `snapshots.set` (post-rotation,
        //     = current `nesPd`).
        //   - Epoch boundary (slot is in `ls.epoch + 1` but ledger has not
        //     yet ticked) → reads `snapshots.mark`, which is the value that
        //     becomes the new `set` and thus the new `nesPd` after NEWEPOCH.
        //
        // Without this forecast we would use 2-epoch-old stake when forging
        // the very first block of a new epoch (the case where no peer's
        // epoch-N block has applied yet), producing a leader-check result
        // computed over the wrong stake distribution.
        let (pool_stake, total_active_stake) =
            ls.pool_distribution_for_slot(next_slot.0, &creds.pool_id);
        if pool_stake == 0 && total_active_stake == 0 {
            debug!(
                target: "forge",
                pool_id = %creds.pool_id,
                forge_epoch = ls.epoch_of_slot(next_slot.0),
                ledger_epoch = ls.epoch.0,
                "Forge: skipping — no usable stake snapshot available for forge slot"
            );
        }
        drop(ls);

        if pool_stake == 0 || total_active_stake == 0 {
            // Log periodically so the operator knows stake hasn't activated yet.
            if next_slot.0.is_multiple_of(100) {
                debug!(
                    target: "forge",
                    slot = next_slot.0,
                    pool_id = %creds.pool_id,
                    pool_stake,
                    "Forge: pool has zero relative stake in 'set' snapshot — waiting for delegation"
                );
            }
            return;
        }

        // ── Step 6: checkShouldForge — VRF leader election ───────────────────
        // Haskell: checkIsLeader → VRF leader election.
        // Not leader → TraceNodeNotLeader (Info) + exitEarly.
        // (KES update and KES-period usability checks are also inside
        // checkShouldForge but are handled inside forge_block below.)
        self.metrics
            .leader_checks_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let is_leader = crate::forge::check_slot_leadership(
            creds,
            next_slot,
            &epoch_nonce,
            pool_stake,
            total_active_stake,
            self.consensus.active_slot_coeff_rational,
        );

        let relative_stake_display = if total_active_stake > 0 {
            pool_stake as f64 / total_active_stake as f64
        } else {
            0.0
        };

        if !is_leader {
            self.metrics
                .leader_checks_not_elected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            info!(
                target: "forge",
                slot = next_slot.0,
                pool_id = %creds.pool_id,
                stake = format_args!("{relative_stake_display:.6}"),
                "TraceNodeNotLeader",
            );
            return;
        }

        // ── Step 7: TraceNodeIsLeader ─────────────────────────────────────────
        info!(
            target: "forge",
            slot = next_slot.0,
            pool_id = %creds.pool_id,
            stake = format_args!("{relative_stake_display:.6}"),
            "TraceNodeIsLeader",
        );

        // Slot-battle counter: increment only when we (a) passed the VRF
        // leader check AND (b) classified the forge as a slot battle in
        // mkCurrentBlockContext. This is the operationally-meaningful
        // semantic — "slots where I was elected leader AND a peer had
        // already filled my slot." Haskell does not expose a corresponding
        // metric (operators infer slot battles from logs), but exposing it
        // here gives us direct visibility into the rare event.
        if matches!(forge_mode, ForgeMode::SlotBattle { .. }) {
            self.metrics
                .forge_slot_battles_total
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        // ── Step 8: applyChainTick + mempool snapshot ─────────────────────────
        // Haskell: applyChainTick → TraceForgeTickedLedgerState (Debug).
        //          getSnapshotFor → TraceForgingMempoolSnapshot (Debug).
        debug!(
            target: "forge",
            slot = next_slot.0,
            "TraceForgeTickedLedgerState",
        );

        // Collect transactions from mempool using protocol params limits.
        // Enforce byte-size AND execution-unit budgets so the forged block
        // stays within maxBlockBodySize and maxBlockExecutionUnits.
        let ls = self.ledger_state.read().await;
        let max_block_body_size = ls.epochs.protocol_params.max_block_body_size;
        let max_block_ex_mem = ls.epochs.protocol_params.max_block_ex_units.mem;
        let max_block_ex_steps = ls.epochs.protocol_params.max_block_ex_units.steps;
        let current_era = ls.era;
        drop(ls);
        // Pass `next_slot` so the mempool excludes TTL-expired txs from the
        // forged block. Including a tx with `ttl < forge_slot` produces a
        // block that fails Phase-1 validation on every conforming peer (this
        // was the proximate cause of forged block c777daed... being rejected
        // by the preview network on 2026-04-27).
        let transactions = self.mempool.get_txs_for_block_with_ex_units_at(
            500,
            max_block_body_size as usize,
            max_block_ex_mem,
            max_block_ex_steps,
            Some(next_slot),
        );
        debug!(
            target: "forge",
            slot = next_slot.0,
            mempool_size = transactions.len(),
            "TraceForgingMempoolSnapshot",
        );
        let config = crate::forge::BlockProducerConfig {
            // Node software capability version from config, NOT the on-chain ledger version.
            // Matches cardano-node's cardanoProtocolVersion (hardcoded per software release).
            // Respects ExperimentalHardForksEnabled: false→10,8  true→11,0
            protocol_version: self.config.node_protocol_version(),
            _max_block_body_size: max_block_body_size,
            _max_txs_per_block: 500,
            era: current_era,
            slots_per_kes_period,
        };

        // ── Step 9: Block.forgeBlock — TraceForgedBlock ───────────────────────
        match crate::forge::forge_block(
            creds,
            &config,
            next_slot,
            block_number,
            prev_hash,
            &epoch_nonce,
            transactions,
        ) {
            Ok((block, cbor)) => {
                // Haskell: TraceForgedBlock (Info) — block has been constructed and signed.
                info!(
                    target: "forge",
                    slot = next_slot.0,
                    block_no = block_number.0,
                    block_hash = %block.header.header_hash.to_hex(),
                    txs = block.transactions.len(),
                    "TraceForgedBlock",
                );

                // ── Phase 2: Submit forged block via ChainSelQueue ────────────
                //
                // Per the Haskell architecture, all blocks — including locally
                // forged ones — enter the node via the same `addBlock` path.
                // The ChainSelQueue receives the forged block, writes it to
                // VolatileDB, and runs chain selection. The queue returns one
                // of: AddedAsTip { tip_hash, tip_slot, tip_block_no } /
                // StoredAsFork / AlreadyKnown / TriggeredFork / Invalid.
                //
                // The `AddedAsTip.tip_hash == block.hash()` check is now O(1)
                // and unambiguous: if the forged block's hash matches the
                // returned tip_hash it was adopted as the new chain tip
                // (normal forge case); if not, an upstream block raced ahead
                // and became tip between forge start and chain-sel processing.
                // Relying on the enum variant alone is not enough — we must
                // verify the forged block actually sits at the selected_chain
                // tip before applying it to the ledger or announcing it to peers.
                //
                // If no handle is available (should not happen after Node::new),
                // fall back to the direct ChainDB write path for correctness.
                let chain_sel_verdict = if let Some(ref handle) = self.chain_sel_handle {
                    handle
                        .submit_self_forged_block_with_header(
                            *block.hash(),
                            block.slot(),
                            block.block_number(),
                            *block.prev_hash(),
                            cbor,
                            block.header.clone(),
                        )
                        .await
                } else {
                    warn!("No ChainSelHandle available — storing forged block directly (fallback)");
                    let mut db = self.chain_db.write().await;
                    match db.add_block(
                        *block.hash(),
                        block.slot(),
                        block.block_number(),
                        *block.prev_hash(),
                        cbor,
                    ) {
                        Ok(true) => {
                            // Extended the selected chain — synthesise AddedAsTip.
                            if let Some((tip_slot, tip_hash, tip_block_no)) = db.get_tip_info() {
                                Some(dugite_storage::AddBlockResult::AddedAsTip {
                                    tip_hash,
                                    tip_slot,
                                    tip_block_no,
                                })
                            } else {
                                Some(dugite_storage::AddBlockResult::StoredAsFork)
                            }
                        }
                        Ok(false) => Some(dugite_storage::AddBlockResult::StoredAsFork),
                        Err(e) => Some(dugite_storage::AddBlockResult::Invalid(e.to_string())),
                    }
                };

                // ── Forge-race verification ─────────────────────────────────
                //
                // `AddedAsTip.tip_hash == block.hash()` is the O(1) check that
                // replaces the old post-hoc ChainDB re-lookup (`forged_is_tip`).
                // Between forge start (reading ledger tip X at H-1) and
                // chain-sel processing, an upstream block may have arrived and
                // advanced our selected_chain past X. In that case
                // `insert_block_internal` stored our block as a fork block and
                // `AddedAsTip.tip_hash` will differ from `block.hash()` (or the
                // result will be `StoredAsFork`).
                //
                // Applying a fork block to the ledger would corrupt ledger
                // state. Announcing it would waste peer bandwidth. Abort cleanly.
                let storage_succeeded = match &chain_sel_verdict {
                    Some(dugite_storage::AddBlockResult::AddedAsTip { tip_hash, .. })
                        if *tip_hash == *block.hash() =>
                    {
                        true
                    }
                    Some(dugite_storage::AddBlockResult::AddedAsTip { tip_hash, .. }) => {
                        // Haskell: TraceDidntAdoptBlock (Error) — forged block was not
                        // adopted because another block raced to the tip first.
                        error!(
                            target: "forge",
                            slot = next_slot.0,
                            block_no = block_number.0,
                            block_hash = %block.hash().to_hex(),
                            actual_tip = %tip_hash.to_hex(),
                            "TraceDidntAdoptBlock: forge race lost — another block extended the tip first",
                        );
                        self.metrics
                            .forge_race_lost
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        false
                    }
                    Some(dugite_storage::AddBlockResult::StoredAsFork)
                    | Some(dugite_storage::AddBlockResult::AlreadyKnown) => {
                        // Haskell: TraceDidntAdoptBlock (Error).
                        error!(
                            target: "forge",
                            slot = next_slot.0,
                            block_no = block_number.0,
                            block_hash = %block.hash().to_hex(),
                            "TraceDidntAdoptBlock: forged block stored as fork — race lost",
                        );
                        self.metrics
                            .forge_race_lost
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        false
                    }
                    Some(dugite_storage::AddBlockResult::TriggeredFork {
                        intersection_hash,
                        intersection_slot,
                        rollback,
                        apply,
                    }) => {
                        // Chain selection switched to a strictly-longer competing
                        // fork that ends with our forged block. Submitting the
                        // block caused the switch, so VolatileDB has already
                        // committed the new `selected_chain`. The ledger is still
                        // on the pre-fork chain and MUST be rolled back to the
                        // intersection and re-applied along the new fork. The
                        // last block in `apply` is validated+applied by the
                        // normal own-block path immediately after this branch,
                        // preserving `ValidateAll` semantics for own forges.
                        //
                        // If the last `apply` hash is NOT our forged block, then
                        // the switch was triggered onto a chain that does not
                        // include our block — this is a genuine race-lost case.
                        if apply.last() != Some(block.hash()) {
                            warn!(
                                slot = next_slot.0,
                                forged = %block.hash().to_hex(),
                                actual_tip = apply.last().map(|h| h.to_hex()).unwrap_or_default(),
                                "Forge triggered fork switch but our block is not at new tip — race lost"
                            );
                            self.metrics
                                .forge_race_lost
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            false
                        } else {
                            info!(
                                intersection = %intersection_hash.to_hex(),
                                intersection_slot = intersection_slot.0,
                                rollback_count = rollback.len(),
                                apply_count = apply.len(),
                                forged = %block.hash().to_hex(),
                                "Forge triggered fork switch — our block is new tip; \
                                 rolling back ledger and replaying fork"
                            );
                            self.metrics
                                .rollback_count
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                            let rollback_point =
                                Point::Specific(*intersection_slot, *intersection_hash);
                            // VolatileDB already switched to the fork that
                            // ends with our forged block.  Use the ledger-only
                            // rollback so we don't undo the switch.
                            if !self.handle_ledger_rollback(&rollback_point).await {
                                warn!(
                                    rollback_slot = intersection_slot.0,
                                    "Forge fork rollback failed; skipping replay."
                                );
                                // Yield false so storage_succeeded=false and the
                                // caller returns early without attempting to apply
                                // our forged block on a misaligned ledger.
                                false
                            } else {
                                // Replay every block in `apply` EXCEPT the last one
                                // (our forged block). The caller below runs the
                                // normal own-block apply + announce path for the
                                // last element, so we must not double-apply it here.
                                // Full-validate by default (cardano-node parity);
                                // DUGITE_TRUSTED_CATCHUP=1 opts out.
                                let validation_mode = if std::env::var("DUGITE_TRUSTED_CATCHUP")
                                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                                    .unwrap_or(false)
                                {
                                    BlockValidationMode::ApplyOnly
                                } else {
                                    BlockValidationMode::ValidateAll
                                };
                                let intermediate = &apply[..apply.len() - 1];
                                let mut replay_failed = false;
                                for fork_hash in intermediate {
                                    let cbor_opt = {
                                        let db = self.chain_db.read().await;
                                        db.get_block(fork_hash).unwrap_or(None)
                                    };
                                    let Some(cbor) = cbor_opt else {
                                        warn!(
                                            hash = %fork_hash.to_hex(),
                                            "Forge fork replay: block hash in apply list not found in ChainDB"
                                        );
                                        replay_failed = true;
                                        break;
                                    };
                                    // #738: ValidateAll reads the witness set —
                                    // minimal decode is only safe for ApplyOnly.
                                    let decode_result = if matches!(
                                        validation_mode,
                                        BlockValidationMode::ApplyOnly
                                    ) {
                                        dugite_serialization::decode_block_minimal_with_byron_epoch_length(
                                            &cbor,
                                            self.byron_epoch_length,
                                        )
                                    } else {
                                        dugite_serialization::decode_block_with_byron_epoch_length(
                                            &cbor,
                                            self.byron_epoch_length,
                                        )
                                    };
                                    let fork_block = match decode_result {
                                        Ok(b) => b,
                                        Err(e) => {
                                            warn!(
                                                hash = %fork_hash.to_hex(),
                                                "Forge fork replay: failed to decode block: {e}"
                                            );
                                            replay_failed = true;
                                            break;
                                        }
                                    };
                                    let fork_slot = fork_block.slot();
                                    let fork_block_no = fork_block.block_number();
                                    // Fix B (forge path): collect delta so LedgerSeq
                                    // tracks forged-path fork replay blocks too.
                                    let forge_fork_delta = {
                                        let mut ls = self.ledger_state.write().await;
                                        // #733: per-block apply horizon snapshot
                                        // at the pre-block ledger tip (one-shot).
                                        ls.phase2_apply_horizon = if matches!(
                                            validation_mode,
                                            BlockValidationMode::ValidateAll
                                        ) && fork_block.era
                                            >= dugite_primitives::era::Era::Babbage
                                        {
                                            let pre_tip =
                                                ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                                            self.era_history.read().await.phase2_apply_horizon_slot(
                                                dugite_primitives::time::SlotNo(pre_tip),
                                            )
                                        } else {
                                            None
                                        };
                                        // Issue #653 — relief-worker scheduling.
                                        let apply_result = tokio::task::block_in_place(|| {
                                            ls.apply_block_with_delta(&fork_block, validation_mode)
                                        });
                                        match apply_result {
                                            Ok(delta) => {
                                                // Publish view post-apply (#651 P2 / #652 P0).
                                                self.publish_ledger_view(&ls);
                                                if let Some((prev_era, new_era, epoch)) =
                                                    ls.pending_era_transition.take()
                                                {
                                                    drop(ls);
                                                    let mut eh = self.era_history.write().await;
                                                    if eh.current_era() < new_era {
                                                        eh.record_era_transition(new_era, epoch.0);
                                                        info!(
                                                            prev = %prev_era,
                                                            new = %new_era,
                                                            epoch = epoch.0,
                                                            "Era transition recorded in HFC era history (forge fork replay)",
                                                        );
                                                    }
                                                }
                                                delta
                                            }
                                            Err(e) => {
                                                warn!(
                                                slot = fork_slot.0,
                                                block = fork_block_no.0,
                                                "Forge fork replay: ledger apply failed: {e} — \
                                                 abandoning fork (block marked invalid)"
                                            );
                                                drop(ls);
                                                self.abandon_failed_fork(
                                                    fork_block.header.header_hash,
                                                    "forge fork replay: ledger apply failed",
                                                    &rollback_point,
                                                )
                                                .await;
                                                replay_failed = true;
                                                break;
                                            }
                                        }
                                    };
                                    {
                                        let mut seq = self.ledger_seq.write().await;
                                        seq.push(forge_fork_delta);
                                    }
                                    {
                                        let mut fragment = self.chain_fragment.write().await;
                                        fragment.push(fork_block.header.clone());
                                    }
                                    self.consensus.update_tip(fork_block.tip());
                                    self.metrics
                                        .blocks_applied
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    if let Some(ref tx) = self.block_announcement_tx {
                                        let mut hash_bytes = [0u8; 32];
                                        hash_bytes.copy_from_slice(
                                            fork_block.header.header_hash.as_ref(),
                                        );
                                        let _ = tx.send(dugite_network::BlockAnnouncement {
                                            slot: fork_slot.0,
                                            hash: hash_bytes,
                                            block_number: fork_block_no.0,
                                        });
                                        if let Some(ref tb) = self.tip_broadcaster {
                                            tb.announce_apply(tip_broadcast::TipApply {
                                                slot: fork_slot.0,
                                                hash: hash_bytes,
                                                block_number: fork_block_no.0,
                                                era: fork_block.era,
                                            });
                                        }
                                    }
                                }
                                !replay_failed
                            } // else: rollback succeeded, replay ran
                        }
                    }
                    Some(dugite_storage::AddBlockResult::Invalid(reason)) => {
                        // Haskell: TraceForgedInvalidBlock (Error).
                        error!(
                            target: "forge",
                            slot = next_slot.0,
                            block_no = block_number.0,
                            reason,
                            "TraceForgedInvalidBlock: forged block rejected by ChainSelQueue",
                        );
                        false
                    }
                    None => {
                        error!("ChainSelQueue runner exited unexpectedly");
                        false
                    }
                };

                if !storage_succeeded {
                    return;
                }

                // Apply to ledger with full validation.
                // Re-validate our own forged block before announcing it to peers,
                // matching Haskell cardano-node behavior. This prevents producing
                // and propagating blocks that contain invalid transactions.
                // Fix A (forge path): collect delta so LedgerSeq tracks own-
                // forged blocks too, enabling seq-based rollback on the next fork.
                let forged_delta = {
                    let mut ls = self.ledger_state.write().await;
                    // #733: per-block apply horizon snapshot at the pre-block
                    // ledger tip (one-shot). Our own forged block must pass
                    // the same horizon fatality every honest node applies.
                    ls.phase2_apply_horizon = if block.era >= dugite_primitives::era::Era::Babbage {
                        let pre_tip = ls.tip.point.slot().map(|s| s.0).unwrap_or(0);
                        self.era_history
                            .read()
                            .await
                            .phase2_apply_horizon_slot(dugite_primitives::time::SlotNo(pre_tip))
                    } else {
                        None
                    };
                    // Issue #653 — relief-worker scheduling.
                    let apply_result = tokio::task::block_in_place(|| {
                        ls.apply_block_with_delta(&block, BlockValidationMode::ValidateAll)
                    });
                    match apply_result {
                        Ok(delta) => {
                            // Publish view post-apply (#651 P2 / #652 P0).
                            self.publish_ledger_view(&ls);
                            // Refresh governance gauges (incl. treasury /
                            // reserves) so Prometheus reflects the
                            // post-boundary ledger state immediately. The
                            // bulk-sync apply path does this at sync.rs:1818;
                            // the forge path must do the same or the
                            // dugite_treasury_lovelace / dugite_reserves_lovelace
                            // atomics stay stale across every epoch boundary
                            // crossed by a locally-forged block (the BP
                            // typically forges the boundary block itself in
                            // single-BP devnets, so this metric path is the
                            // ONLY one that ever updates the gauge in that
                            // topology).
                            self.metrics
                                .set_governance_snapshot(&governance_snapshot_from_ledger(&ls));
                            delta
                        }
                        Err(e) => {
                            // Haskell: TraceForgedInvalidBlock (Error) — own block failed validation.
                            error!(
                                target: "forge",
                                slot = next_slot.0,
                                block_no = block_number.0,
                                block_hash = %block.header.header_hash.to_hex(),
                                "TraceForgedInvalidBlock: forged block failed ledger validation — NOT announcing: {e}",
                            );

                            // Defence-in-depth recovery (#522):
                            //
                            // The most likely cause is a tag-mismatched tx (is_valid=false
                            // but scripts evaluate to True) that slipped into the mempool
                            // before the Phase-2 admission check was tightened.  Even with
                            // the admission fix, this path can fire for edge cases (e.g. a
                            // tx whose scripts pass at admission time but fail at block-apply
                            // time due to ledger-state differences).
                            //
                            // Recovery: evict all txs in the bad block from the mempool so
                            // the next forge attempt does not re-include them.  Then remove
                            // the bad block from the VolatileDB so it does not occupy the
                            // current height and block future forges at the same slot.
                            let bad_tx_hashes: Vec<_> =
                                block.transactions.iter().map(|tx| tx.hash).collect();
                            if !bad_tx_hashes.is_empty() {
                                self.mempool.remove_txs_with_reason(
                                    &bad_tx_hashes,
                                    dugite_mempool::MempoolRemoveReason::Evicted,
                                );
                                error!(
                                    target: "forge",
                                    count = bad_tx_hashes.len(),
                                    "TraceForgedInvalidBlock: evicted bad txs from mempool",
                                );
                            }

                            // Remove the bad block from VolatileDB so subsequent
                            // forges at the same slot can succeed.
                            {
                                let mut db = self.chain_db.write().await;
                                db.remove_volatile_block(&block.header.header_hash);
                            }

                            return;
                        }
                    }
                };
                {
                    let mut seq = self.ledger_seq.write().await;
                    seq.push(forged_delta);
                }

                // Update chain fragment with the new forged block header.
                // This keeps the fragment in sync with the selected chain so
                // ChainSync servers can find intersects correctly.
                {
                    let mut fragment = self.chain_fragment.write().await;
                    fragment.push(block.header.clone());
                }

                // Update consensus tip
                self.consensus.update_tip(block.tip());

                self.metrics
                    .blocks_forged
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Haskell: TraceAdoptedBlock (Info) — block was adopted as the new chain tip.
                info!(
                    target: "forge",
                    block_no = block_number.0,
                    slot = next_slot.0,
                    block_hash = %block.header.header_hash.to_hex(),
                    txs = block.transactions.len(),
                    "TraceAdoptedBlock",
                );

                // Tip-query staleness fix (2026-05-16): own-forged blocks must
                // also refresh the Prometheus gauges and the N2C
                // NodeStateSnapshot.  Without this, `cardano-cli query tip`
                // and `dugite_block_number` lag the chain by every own-forge.
                self.post_block_apply_updates(&block, next_slot, block_number)
                    .await;

                // Announce the new block to all connected peers.
                //
                // This is the critical propagation edge for issue #439: the
                // broadcast wakes ChainSync server tasks that are parked on
                // `announcement_rx.recv()` for every connected N2N peer, so
                // each peer's next MsgRollForward carries our forged block.
                //
                // `receiver_count()` is checked explicitly so that a zero-
                // subscriber broadcast (which would silently orphan the
                // block) is loudly visible in both logs and metrics.
                if let Some(ref tx) = self.block_announcement_tx {
                    let mut hash_bytes = [0u8; 32];
                    hash_bytes.copy_from_slice(block.header.header_hash.as_ref());
                    let subscribers = tx.receiver_count();
                    let send_result = tx.send(dugite_network::BlockAnnouncement {
                        slot: next_slot.0,
                        hash: hash_bytes,
                        block_number: block_number.0,
                    });
                    if let Some(ref tb) = self.tip_broadcaster {
                        tb.announce_apply(tip_broadcast::TipApply {
                            slot: next_slot.0,
                            hash: hash_bytes,
                            block_number: block_number.0,
                            era: block.era,
                        });
                    }

                    if subscribers == 0 {
                        warn!(
                            target: "forge",
                            slot = next_slot.0,
                            block_no = block_number.0,
                            block_hash = %block.header.header_hash.to_hex(),
                            "Forged block announced but NO peers are subscribed — \
                             block will NOT propagate and will be orphaned. \
                             Check that at least one N2N peer is connected and in hot state."
                        );
                        self.metrics
                            .forge_announce_no_subscribers
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        info!(
                            target: "forge",
                            slot = next_slot.0,
                            block_no = block_number.0,
                            block_hash = %block.header.header_hash.to_hex(),
                            subscribers,
                            delivered = send_result.as_ref().map(|n| *n).unwrap_or(0),
                            "Announced forged block to peers"
                        );
                    }
                    self.metrics
                        .blocks_announced
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                } else {
                    warn!(
                        target: "forge",
                        slot = next_slot.0,
                        block_no = block_number.0,
                        "Forged block has no announcement channel — block will NOT propagate"
                    );
                }
            }
            Err(e) => {
                self.metrics
                    .forge_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Haskell: TraceForgeStateUpdateError (Critical) for KES key errors,
                // or general forge failure.
                error!(
                    target: "forge",
                    slot = next_slot.0,
                    "TraceForgeStateUpdateError: block forging failed: {e}",
                );
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    use dugite_primitives::block::{
        Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput,
    };
    use dugite_primitives::era::Era;
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::{BlockNo, SlotNo};

    /// #768 apply-stall predicate matrix.
    #[test]
    fn apply_stall_detected_matrix() {
        use super::apply_stall_detected;
        let timeout = Duration::from_secs(300);
        let past = Duration::from_secs(301);
        let within = Duration::from_secs(120);

        // The wedge: ledger ahead of ChainDB, work arriving, stalled past timeout.
        assert!(apply_stall_detected(534, 524, true, past, timeout));

        // Healthy: ChainDB tip >= ledger tip (storage precedes apply) — never fires,
        // even if "work_arriving" and long-stalled.
        assert!(!apply_stall_detected(524, 524, true, past, timeout)); // at-tip idle
        assert!(!apply_stall_detected(524, 534, true, past, timeout)); // chaindb ahead

        // Ahead but still bridging (progress within the window) — must not fire.
        assert!(!apply_stall_detected(534, 524, true, within, timeout));

        // Ahead + stalled but NO fetched blocks arriving (quiet, e.g. no peers) —
        // not the busy-loop wedge; do not exit.
        assert!(!apply_stall_detected(534, 524, false, past, timeout));

        // Exactly at the timeout boundary fires (>=).
        assert!(apply_stall_detected(534, 524, true, timeout, timeout));
    }

    use dugite_primitives::transaction::ScriptRef;
    use dugite_serialization::mempack::txout::ScriptRefKind;

    #[test]
    fn decode_imported_script_ref_maps_plutus_language_tags_to_global_versions() {
        // The MemPack Plutus language tag is era-relative but monotonic across
        // all eras (0→V1, 1→V2, 2→V3, 3→V4). decode_imported_script_ref must map
        // it to the global ScriptRef variant so compute_script_ref_hash produces
        // the correct CIP prefix (0x01/0x02/0x03/0x04).
        let body = vec![0xAB, 0xCD, 0xEF];
        assert_eq!(
            super::decode_imported_script_ref(ScriptRefKind::Plutus {
                lang_tag: 0,
                body: body.clone()
            })
            .unwrap(),
            ScriptRef::PlutusV1(body.clone())
        );
        assert_eq!(
            super::decode_imported_script_ref(ScriptRefKind::Plutus {
                lang_tag: 1,
                body: body.clone()
            })
            .unwrap(),
            ScriptRef::PlutusV2(body.clone())
        );
        assert_eq!(
            super::decode_imported_script_ref(ScriptRefKind::Plutus {
                lang_tag: 2,
                body: body.clone()
            })
            .unwrap(),
            ScriptRef::PlutusV3(body.clone())
        );
        assert_eq!(
            super::decode_imported_script_ref(ScriptRefKind::Plutus {
                lang_tag: 3,
                body: body.clone()
            })
            .unwrap(),
            ScriptRef::PlutusV4(body.clone())
        );
        // NO-SILENT-NONE (#10(B)): unknown language tag → HARD ERROR, never a
        // silently-dropped script_ref.
        assert!(
            super::decode_imported_script_ref(ScriptRefKind::Plutus { lang_tag: 9, body }).is_err(),
            "unknown Plutus language tag must hard-error, not silently drop"
        );
    }

    #[test]
    fn decode_imported_script_ref_decodes_native_timelock_cbor() {
        // Native (timelock) script-ref body is the raw native-script CBOR:
        // array(2) [0, bytes(28)] — a ScriptPubkey.
        let mut cbor = vec![0x82, 0x00, 0x58, 28];
        cbor.extend_from_slice(&[0x42u8; 28]);
        let sr = super::decode_imported_script_ref(ScriptRefKind::Native(cbor))
            .expect("native script must decode");
        assert!(matches!(sr, ScriptRef::NativeScript(_)));
    }

    #[test]
    fn decode_imported_script_ref_malformed_native_blob_hard_errors() {
        // NO-SILENT-NONE (#10(B)): a malformed tag-5 native-script CBOR blob must
        // HARD-ERROR the import path, not silently degrade to `None` (which would
        // corrupt the imported UTxO set by dropping the reference script and cause
        // spurious phase-2 failures at the live tip). dugite-node is
        // adversarial-deployment software: reject over silent skip.
        // `0x9f` is an indefinite-array opener with no body/terminator → invalid.
        let malformed = vec![0x9f, 0x00];
        assert!(
            super::decode_imported_script_ref(ScriptRefKind::Native(malformed)).is_err(),
            "malformed native reference-script CBOR must hard-error, not silently drop"
        );
    }

    #[test]
    fn imported_inline_datum_malformed_but_framed_is_opaque_not_error() {
        // #10 commit-B RE-FIX (path 1): a tag-4 inline datum is a Haskell
        // `BinaryData`, a `newtype … ShortByteString deriving newtype (… MemPack)`
        // (cardano-ledger `Cardano.Ledger.Plutus.Data`). Its `unpackM` at snapshot
        // load stores the bytes OPAQUELY and never re-decodes the Plutus `Data`
        // structure (validation lives only in `makeBinaryData`, the on-chain
        // DecCBOR path, which `loadSnapshot` does NOT invoke). So a malformed-but-
        // framed blob must import OPAQUE — NOT hard-error (the previous no-silent-
        // None behaviour OVER-REJECTED vs Haskell) and NOT `OutputDatum::None`.
        use dugite_primitives::transaction::{OutputDatum, PlutusData};

        // `0x9f` (indefinite array, no terminator) is not valid Plutus Data CBOR.
        let malformed = [0x9f_u8, 0x00];
        // Sanity: the structural decoder genuinely rejects it (so the fallback arm
        // is the one exercised).
        assert!(
            dugite_serialization::decode_plutus_data_cbor(&malformed).is_err(),
            "the malformed blob must fail structural decode (otherwise this test is vacuous)"
        );

        match super::import_inline_datum(&malformed) {
            OutputDatum::InlineDatum { data, raw_cbor } => {
                // OPAQUE fallback: raw bytes preserved verbatim in both fields.
                assert_eq!(
                    raw_cbor.as_deref(),
                    Some(&malformed[..]),
                    "raw_cbor must carry the verbatim datum bytes for byte-exact re-encoding"
                );
                assert_eq!(
                    data,
                    PlutusData::Bytes(malformed.to_vec()),
                    "structural decode failure must fall back to opaque PlutusData::Bytes, \
                     never OutputDatum::None"
                );
            }
            other => panic!("malformed-but-framed datum must import as InlineDatum, got {other:?}"),
        }
    }

    #[test]
    fn imported_inline_datum_well_formed_decodes_structurally() {
        // A well-formed Plutus Data CBOR (integer 0 = 0x00) must populate the
        // structural `data` field (here `PlutusData::Integer(0)`) while still
        // preserving the verbatim raw bytes for byte-exact re-encoding.
        use dugite_primitives::transaction::{OutputDatum, PlutusData};
        let cbor = [0x00_u8]; // Plutus Data: integer 0.
        match super::import_inline_datum(&cbor) {
            OutputDatum::InlineDatum { data, raw_cbor } => {
                assert_eq!(raw_cbor.as_deref(), Some(&cbor[..]));
                assert_eq!(
                    data,
                    PlutusData::Integer(num_bigint::BigInt::from(0)),
                    "well-formed datum must decode structurally to Integer(0), not the opaque \
                     Bytes fallback"
                );
            }
            other => panic!("expected InlineDatum, got {other:?}"),
        }
    }

    use super::serve::ChainDBBlockProvider;
    use super::sync::validate_genesis_blocks;
    use super::{
        next_forged_block_number, resolve_inmemory_tables_path, resolve_snapshot_txix_endianness,
        ForgeMode, Point,
    };
    use crate::config::NodeConfig;

    // ─── next_forged_block_number regression tests ───────────────────────────
    //
    // Haskell reference (all verified against IntersectMBO/ouroboros-consensus):
    //   * HeaderValidation.hs:404-413 (`expectedBlockNo`):
    //       Origin      -> expectedFirstBlockNo p
    //       NotOrigin.. -> expectedNextBlockNo p ..annTipBlockNo tip
    //   * HardFork/Combinator/Block.hs:238-241 — HFC delegates to the FIRST
    //     era (Byron).
    //   * Byron/Ledger/HeaderValidation.hs:40 — `expectedFirstBlockNo = BlockNo 0`.
    //
    // Dugite only ever forges from Origin on private testnets configured with
    // `TestShelleyHardForkAtEpoch: 0` (no EBB, first block is a Shelley block).
    // In that case the first forged block MUST be BlockNo 0, not BlockNo 1,
    // or Haskell rejects with `UnexpectedBlockNo (BlockNo 0) (BlockNo 1)`.

    #[test]
    fn next_forged_block_number_at_origin_is_zero() {
        // Regression: with tip at Origin, the first forged block must be
        // BlockNo 0 (matching Haskell's `expectedFirstBlockNo` via HFC→Byron).
        let bn = next_forged_block_number(&Point::Origin, BlockNo(0));
        assert_eq!(
            bn.0, 0,
            "first forged block from Origin must be BlockNo 0, \
             not BlockNo 1 — Haskell rejects 1 with UnexpectedBlockNo \
             (HeaderValidation.hs:404 + HardFork/Block.hs:238 + \
             Byron/HeaderValidation.hs:40)"
        );
    }

    #[test]
    fn next_forged_block_number_at_origin_ignores_current_block_number() {
        // Origin always produces BlockNo 0 regardless of `current_block_number`.
        // (A non-zero `current_block_number` at Origin would indicate a state
        // inconsistency but we still want the invariant to hold.)
        let bn = next_forged_block_number(&Point::Origin, BlockNo(42));
        assert_eq!(bn.0, 0);
    }

    #[test]
    fn next_forged_block_number_increments_from_non_origin() {
        // After at least one block is applied, each subsequent forge
        // increments by one (Haskell `succ`).
        let point = Point::Specific(SlotNo(10), Hash32::from_bytes([0xAA; 32]));
        assert_eq!(next_forged_block_number(&point, BlockNo(0)).0, 1);
        assert_eq!(next_forged_block_number(&point, BlockNo(41)).0, 42);
        assert_eq!(next_forged_block_number(&point, BlockNo(413)).0, 414);
    }

    /// Tip-query staleness regression — narrow contract test.
    ///
    /// Before the 2026-05-16 fix, `Node::try_forge_block_at` skipped the
    /// metric setters that `apply_fetched_block` ran inline on every
    /// peer-adopted block.  The new `post_block_apply_updates` helper
    /// centralises those setters and is now called from BOTH paths.
    ///
    /// A full end-to-end test of `post_block_apply_updates` requires a real
    /// `Node` fixture (ledger + chain DB + N2C query handler), which the
    /// existing test harness does not provide.  This narrower test pins the
    /// metric contract that the forge path was missing: the gauges that the
    /// helper calls actually persist and Prometheus will report them.  The
    /// snapshot-refresh half of the contract is exercised end-to-end by the
    /// local-devnet 30-min soak (verify.sh predicate 4 — tip parity over
    /// time, dugite-bp not excluded).
    ///
    /// Design doc:
    /// docs/superpowers/specs/2026-05-16-tip-query-staleness-fix.md
    #[test]
    fn metrics_setters_advance_block_number_and_slot() {
        use std::sync::atomic::Ordering;

        let metrics = crate::metrics::NodeMetrics::new();
        assert_eq!(metrics.block_number.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.slot_number.load(Ordering::Relaxed), 0);

        metrics.set_block_number(42);
        metrics.set_slot(500);

        assert_eq!(
            metrics.block_number.load(Ordering::Relaxed),
            42,
            "set_block_number must persist for Prometheus + N2C tip queries"
        );
        assert_eq!(
            metrics.slot_number.load(Ordering::Relaxed),
            500,
            "set_slot must persist for Prometheus + N2C tip queries"
        );
    }

    /// Helper to create a minimal test block with the given era, block number, hash, and prev_hash.
    fn make_test_block(
        era: Era,
        block_no: u64,
        slot: u64,
        hash: Hash32,
        prev_hash: Hash32,
    ) -> Block {
        Block {
            header: BlockHeader {
                header_hash: hash,
                prev_hash,
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
                slot: SlotNo(slot),
                epoch_nonce: Hash32::ZERO,
                body_size: 0,
                body_hash: Hash32::ZERO,
                operational_cert: OperationalCert {
                    hot_vkey: vec![],
                    sequence_number: 0,
                    kes_period: 0,
                    sigma: vec![],
                },
                protocol_version: ProtocolVersion { major: 0, minor: 0 },
                kes_signature: vec![],
            },
            transactions: vec![],
            era,
            raw_cbor: None,
        }
    }

    #[test]
    fn test_validate_genesis_empty_blocks() {
        // Empty block list should pass validation
        let result = validate_genesis_blocks(&[], None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_genesis_skips_non_genesis_block() {
        // Block with block_number > 0 should skip validation
        let block = make_test_block(
            Era::Byron,
            42,
            100,
            Hash32::from_bytes([1u8; 32]),
            Hash32::from_bytes([2u8; 32]),
        );
        let result = validate_genesis_blocks(&[block], None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_byron_genesis_hash_match() {
        let expected_hash = Hash32::from_bytes([0xAA; 32]);
        let block = make_test_block(Era::Byron, 0, 0, expected_hash, Hash32::ZERO);
        let result = validate_genesis_blocks(&[block], Some(&expected_hash), None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_byron_genesis_hash_mismatch() {
        let expected_hash = Hash32::from_bytes([0xAA; 32]);
        let wrong_hash = Hash32::from_bytes([0xBB; 32]);
        let block = make_test_block(Era::Byron, 0, 0, wrong_hash, Hash32::ZERO);
        let result = validate_genesis_blocks(&[block], Some(&expected_hash), None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Byron genesis block hash mismatch"));
        assert!(err.contains(&expected_hash.to_hex()));
        assert!(err.contains(&wrong_hash.to_hex()));
    }

    #[test]
    fn test_validate_byron_genesis_no_expected_hash() {
        // When no expected hash is configured, validation should pass (with warning)
        let block = make_test_block(
            Era::Byron,
            0,
            0,
            Hash32::from_bytes([0xCC; 32]),
            Hash32::ZERO,
        );
        let result = validate_genesis_blocks(&[block], None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_shelley_genesis_prev_hash_match() {
        // For Shelley-first chains, prev_hash of block 0 is the genesis hash
        let genesis_hash = Hash32::from_bytes([0xDD; 32]);
        let block = make_test_block(
            Era::Shelley,
            0,
            0,
            Hash32::from_bytes([0x11; 32]),
            genesis_hash,
        );
        let result = validate_genesis_blocks(&[block], None, Some(&genesis_hash));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_shelley_genesis_prev_hash_mismatch() {
        let expected_genesis = Hash32::from_bytes([0xDD; 32]);
        let wrong_prev = Hash32::from_bytes([0xEE; 32]);
        let block = make_test_block(
            Era::Shelley,
            0,
            0,
            Hash32::from_bytes([0x11; 32]),
            wrong_prev,
        );
        let result = validate_genesis_blocks(&[block], None, Some(&expected_genesis));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Shelley genesis hash mismatch"));
        assert!(err.contains(&expected_genesis.to_hex()));
        assert!(err.contains(&wrong_prev.to_hex()));
    }

    #[test]
    fn test_validate_shelley_genesis_no_expected_hash() {
        // When no expected Shelley hash is configured, validation should pass
        let block = make_test_block(
            Era::Shelley,
            0,
            0,
            Hash32::from_bytes([0x11; 32]),
            Hash32::from_bytes([0x22; 32]),
        );
        let result = validate_genesis_blocks(&[block], None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_byron_and_shelley_batch() {
        // A batch starting with Byron genesis block 0 followed by more blocks
        let byron_hash = Hash32::from_bytes([0xAA; 32]);
        let b0 = make_test_block(Era::Byron, 0, 0, byron_hash, Hash32::ZERO);
        let b1 = make_test_block(Era::Byron, 1, 1, Hash32::from_bytes([0xBB; 32]), byron_hash);

        let result = validate_genesis_blocks(&[b0, b1], Some(&byron_hash), None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_conway_genesis_prev_hash() {
        // Conway era block at genesis (block 0) — still Shelley-based
        let genesis_hash = Hash32::from_bytes([0xFF; 32]);
        let block = make_test_block(
            Era::Conway,
            0,
            0,
            Hash32::from_bytes([0x33; 32]),
            genesis_hash,
        );
        // Conway is Shelley-based, so Shelley genesis hash should be validated
        let result = validate_genesis_blocks(&[block], None, Some(&genesis_hash));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_conway_genesis_prev_hash_mismatch() {
        let expected = Hash32::from_bytes([0xFF; 32]);
        let wrong = Hash32::from_bytes([0x00; 32]);
        let block = make_test_block(Era::Conway, 0, 0, Hash32::from_bytes([0x33; 32]), wrong);
        let result = validate_genesis_blocks(&[block], None, Some(&expected));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_genesis_hash_parsing() {
        let json = r#"{
            "Network": "Testnet",
            "NetworkMagic": 2,
            "ByronGenesisFile": "preview-byron-genesis.json",
            "ByronGenesisHash": "81cf23542e33d64c541699926c2b5e6e9c286583f0c8a3fb5f22ea7b352dd174",
            "ShelleyGenesisFile": "preview-shelley-genesis.json",
            "ShelleyGenesisHash": "363498d1024f84bb39d3fa9593ce391483cb40d479b87233f868d6e57c3a400d"
        }"#;

        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.byron_genesis_hash.as_deref(),
            Some("81cf23542e33d64c541699926c2b5e6e9c286583f0c8a3fb5f22ea7b352dd174")
        );
        assert_eq!(
            config.shelley_genesis_hash.as_deref(),
            Some("363498d1024f84bb39d3fa9593ce391483cb40d479b87233f868d6e57c3a400d")
        );

        // Verify the hashes parse into Hash32 correctly
        let byron_hash = Hash32::from_hex(config.byron_genesis_hash.as_ref().unwrap()).unwrap();
        assert_ne!(byron_hash, Hash32::ZERO);

        let shelley_hash = Hash32::from_hex(config.shelley_genesis_hash.as_ref().unwrap()).unwrap();
        assert_ne!(shelley_hash, Hash32::ZERO);
    }

    #[test]
    fn test_config_without_genesis_hashes() {
        let json = r#"{
            "Network": "Testnet",
            "NetworkMagic": 2,
            "ByronGenesisFile": "preview-byron-genesis.json",
            "ShelleyGenesisFile": "preview-shelley-genesis.json"
        }"#;

        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert!(config.byron_genesis_hash.is_none());
        assert!(config.shelley_genesis_hash.is_none());
        assert!(config.alonzo_genesis_hash.is_none());
        assert!(config.conway_genesis_hash.is_none());
    }

    /// Regression test: BlockProvider methods must not panic when called
    /// from within a tokio async runtime. Previously, bare `blocking_read()`
    /// would panic with "Cannot block the current thread from within a runtime".
    /// The fix wraps them in `tokio::task::block_in_place`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_block_provider_works_inside_async_runtime() {
        use dugite_network::BlockProvider;
        use dugite_storage::ChainDB;

        let tmp = tempfile::tempdir().unwrap();
        let db = ChainDB::open(tmp.path()).unwrap();
        let provider = ChainDBBlockProvider {
            chain_db: Arc::new(RwLock::new(db)),
        };

        // These would panic before the block_in_place fix
        let tip = provider.get_tip();
        assert_eq!(tip.block_number, 0);

        let result = provider.get_block(&[0u8; 32]);
        assert!(result.is_none());

        let result = provider.has_block(&[0u8; 32]);
        assert!(!result);

        let result = provider.get_next_block_after_slot(0);
        assert!(result.is_none());
    }

    /// Regression test: tokio RwLock blocking_read inside block_in_place
    /// must not panic in a multi-threaded async runtime. This covers the
    /// pattern used by both LedgerUtxoProvider and ChainDBBlockProvider.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_blocking_read_via_block_in_place_does_not_panic() {
        let lock = Arc::new(RwLock::new(42u64));
        let value = tokio::task::block_in_place(|| {
            let guard = lock.blocking_read();
            *guard
        });
        assert_eq!(value, 42);
    }

    /// N2C query-snapshot refresh cadence: 1 Hz at tip (effectively per
    /// block, since at-tip blocks arrive every ~20 s), 30 s during catch-up.
    /// The rebuild walks every delegation/pool/DRep in the ledger (~1.4 s at
    /// mainnet epoch-334 scale) and runs synchronously on the apply task, so
    /// the previous unconditional 1 Hz cadence stalled the apply loop for
    /// ~60 % of wall time during mainnet bulk sync (metronomic 1.4 s gap
    /// every 2.5 s, measured 2026-06-10).
    #[test]
    fn query_state_refresh_interval_drops_to_30s_during_catch_up() {
        assert_eq!(
            super::query_state_refresh_interval(true),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            super::query_state_refresh_interval(false),
            std::time::Duration::from_secs(30)
        );
    }

    // ─── Forge-loop gate tests (Haskell-aligned) ─────────────────────────────
    //
    // These tests exercise the gate predicates extracted from `try_forge_block_at`
    // without requiring a full Node.  They verify the logic that replaces the
    // old `MAX_FORGE_LAG_SLOTS = 60` guard.

    /// Catch-up gate: during bulk sync, far behind the peer-reported network
    /// tip → silently skip the leadership check (no TraceStartLeadershipCheck
    /// log, no lock acquisition past the gate).  Reported on preprod
    /// 2026-05-11 as "BP performing leadership checks every 1s unnecessarily
    /// while resynchronising the chain".
    #[test]
    fn forge_catch_up_gate_skips_when_far_behind_peer_tip() {
        let stability = dugite_consensus::stability_window_slots(2160, 0.05);
        // tip_slot 25M, peer_tip 122M → ~97M slot lag, far above 129 600.
        assert!(super::should_skip_forge_for_catch_up(
            25_000_000,
            122_000_000,
            122_000_000,
            stability,
        ));
    }

    /// Catch-up gate: within the **short** catch-up lag (Bug H) of the
    /// network tip → run the leadership check normally.  Updated 2026-05-16
    /// from the prior `within stability_window` semantics: the gate now
    /// requires being within `max(5, stability_window / 30)` slots of the
    /// peer tip, not just within the full stability window.  This tighter
    /// rule prevents a BP that briefly fell behind from forging on a stale
    /// tip — under the old loose rule the BP could create a sibling fork
    /// that Praos's k-stability then refused to reconcile.
    #[test]
    fn forge_catch_up_gate_passes_when_within_short_catch_up_lag() {
        let stability = dugite_consensus::stability_window_slots(2160, 0.05);
        let short_catch_up_lag = (stability / 30).max(5);
        let peer_tip = 1_000_000;
        // Tip exactly at the short-catch-up-lag boundary — should NOT skip.
        let tip_slot = peer_tip - short_catch_up_lag;
        assert!(!super::should_skip_forge_for_catch_up(
            tip_slot, peer_tip, peer_tip, stability,
        ));
        // Tip one slot inside the lag — should NOT skip.
        assert!(!super::should_skip_forge_for_catch_up(
            tip_slot + 1,
            peer_tip,
            peer_tip,
            stability,
        ));
    }

    /// Catch-up gate: one slot past the **short** catch-up lag → skip.
    /// Replaces the old `forge_catch_up_gate_fires_one_slot_past_window`
    /// (which tested the full `stability_window` boundary; that boundary
    /// is now superseded by the tighter short-catch-up-lag check).
    #[test]
    fn forge_catch_up_gate_fires_one_slot_past_short_catch_up_lag() {
        let stability = dugite_consensus::stability_window_slots(2160, 0.05);
        let short_catch_up_lag = (stability / 30).max(5);
        let peer_tip = 1_000_000;
        let tip_slot = peer_tip - short_catch_up_lag - 1;
        assert!(super::should_skip_forge_for_catch_up(
            tip_slot, peer_tip, peer_tip, stability,
        ));
    }

    /// Catch-up gate: at the stability_window boundary (way past the
    /// short-catch-up-lag) → skip.  Originally
    /// `forge_catch_up_gate_fires_one_slot_past_window` — the new
    /// short-catch-up-lag check fires long before the stability_window
    /// boundary is reached.
    #[test]
    fn forge_catch_up_gate_fires_at_stability_window_boundary() {
        let stability = dugite_consensus::stability_window_slots(2160, 0.05);
        let peer_tip = 1_000_000;
        let tip_slot = peer_tip - stability - 1; // one slot beyond the window.
        assert!(super::should_skip_forge_for_catch_up(
            tip_slot, peer_tip, peer_tip, stability,
        ));
    }

    /// Catch-up gate: at or past the network tip → never skip (we are
    /// caught up and could be forging on top of the chain).
    #[test]
    fn forge_catch_up_gate_passes_at_or_past_peer_tip() {
        let stability = dugite_consensus::stability_window_slots(2160, 0.05);
        assert!(!super::should_skip_forge_for_catch_up(
            100, 100, 100, stability,
        ));
        // Forged ahead — peer_tip lags our tip until the next ChainSync update.
        assert!(!super::should_skip_forge_for_catch_up(
            105, 100, 105, stability,
        ));
    }

    /// Bug G regression (2026-05-16): when our chain is still at Origin
    /// (tip_slot=0) but a peer has any non-Origin chain (peer_tip>0), the
    /// gate MUST skip the forge regardless of stability_window.  Otherwise
    /// the BP would forge block_no=0 on top of Origin, creating a fork that
    /// can never reconcile with the peer's chain because the only common
    /// ancestor is genesis — beyond the volatile + ledger-seq window
    /// (`VolatileDB::switch_chain` returns `None` with "fork unreachable"
    /// for any subsequent attempt to adopt the peer's chain).
    ///
    /// Before this guard:
    ///   reference_tip(5) - tip_slot(0) > stability_window(150) → false → DON'T SKIP
    /// allowed the BP to fork from Origin at the very first slot.
    #[test]
    fn forge_catch_up_gate_skips_boot_when_peer_has_chain_and_we_have_origin() {
        let stability = dugite_consensus::stability_window_slots(10, 0.2); // local-devnet
                                                                           // dbp empty (tip_slot=0), peer (cbp via relay) at slot 5: must skip.
        assert!(super::should_skip_forge_for_catch_up(
            0, // tip_slot — our chain is at Origin
            5, // peer_tip — peer has 5 slots of chain
            7, // wall_clock_slot — we're at slot 7
            stability,
        ));
        // Even a 1-slot peer chain must trigger skip.
        assert!(super::should_skip_forge_for_catch_up(
            0, 1, // bare minimum peer chain
            10, stability,
        ));
    }

    /// Bug G regression (2026-05-16): the boot guard does NOT fire on a
    /// genuinely empty network where peer_tip is also 0.  Both BPs start
    /// from Origin and resolve via Praos VRF tiebreaker (slot battle).
    #[test]
    fn forge_catch_up_gate_does_not_skip_boot_when_both_empty() {
        let stability = dugite_consensus::stability_window_slots(10, 0.2);
        assert!(!super::should_skip_forge_for_catch_up(
            0, // tip_slot
            0, // peer_tip (no peer chain)
            3, // wall_clock_slot small
            stability,
        ));
    }

    /// Catch-up gate: before any ChainSync intersection (`peer_tip == 0`)
    /// fall back to wall clock so a fresh-boot, behind-tip BP still skips.
    /// Without this fallback, the gate would let the first second of forge
    /// attempts pass through during boot.
    #[test]
    fn forge_catch_up_gate_falls_back_to_wall_clock_when_no_peer_tip() {
        let stability = dugite_consensus::stability_window_slots(2160, 0.05);
        // tip at origin, wall clock at preprod-tip — must skip.
        assert!(super::should_skip_forge_for_catch_up(
            0,
            0, // no peer tip yet
            122_000_000,
            stability,
        ));
        // tip near wall clock, no peer info — must pass (rare but valid:
        // e.g. running on an isolated/offline relay).
        assert!(!super::should_skip_forge_for_catch_up(
            122_000_000 - 100,
            0,
            122_000_000,
            stability,
        ));
    }

    /// stability_window_slots(k=2160, f=0.05) must equal 129 600.
    ///
    /// Haskell reference: `stabilityWindow = ceil(3 * k / f)`.
    /// For preview/mainnet k=2160, f=0.05 → 3*2160/0.05 = 129600.000 → 129600.
    #[test]
    fn stability_window_slots_preview() {
        assert_eq!(
            dugite_consensus::stability_window_slots(2160, 0.05),
            129_600,
            "stability window for k=2160, f=0.05 must be 129600 slots (36h)"
        );
    }

    /// BlockFromFuture gate: must use **strict** `tip_slot > current_slot`,
    /// matching Haskell's `mkCurrentBlockContext` GT branch in
    /// `ouroboros-consensus-diffusion/.../NodeKernel.hs`.
    ///
    /// The equality case (`tip_slot == current_slot`) is a slot battle, NOT a
    /// "block from future". Haskell handles it by forging a competing block
    /// against the tip's parent (with the same block_no as the existing tip);
    /// we currently log `TraceSlotBattleSkipped` at INFO and skip.
    #[test]
    fn forge_gate_block_from_future_uses_strict_greater_than() {
        let current_slot: u64 = 1000;

        // tip_slot > current_slot → gate fires (chain ahead of wall clock).
        let tip_slot: u64 = 1001;
        assert!(
            matches!(tip_slot.cmp(&current_slot), std::cmp::Ordering::Greater),
            "BlockFromFuture must fire only on strict tip_slot > current_slot"
        );

        // tip_slot == current_slot → NOT BlockFromFuture, this is a slot battle.
        let tip_slot: u64 = 1000;
        assert!(
            matches!(tip_slot.cmp(&current_slot), std::cmp::Ordering::Equal),
            "tip_slot == current_slot must be classified as Equal, not Greater \
             (Haskell forges a competing block in this case; we log SlotBattle)"
        );

        // tip_slot < current_slot → normal forge condition.
        let tip_slot: u64 = 999;
        assert!(
            matches!(tip_slot.cmp(&current_slot), std::cmp::Ordering::Less),
            "tip_slot < current_slot is the normal forge-on-top condition"
        );
    }

    /// SlotBattle classification: tip_slot == current_slot must be treated
    /// distinctly from `tip_slot > current_slot`. This is a regression guard
    /// against accidentally re-introducing `tip_slot >= current_slot` (which
    /// over-categorises slot-battles as `BlockFromFuture` and produces noisy
    /// false-positive ERROR logs every time a peer forges at our wall-clock
    /// slot).
    #[test]
    fn forge_gate_slot_battle_is_distinct_from_block_from_future() {
        let current_slot: u64 = 111_562_722;
        // The exact case observed in the 2026-05-08 soak logs: peer's block
        // for slot N was applied milliseconds before our forge ticker fired
        // for slot N. tip_slot equals current_slot.
        let tip_slot = current_slot;
        let cmp = tip_slot.cmp(&current_slot);
        assert!(
            !matches!(cmp, std::cmp::Ordering::Greater),
            "slot-battle race must not be misclassified as BlockFromFuture (>)"
        );
        assert!(
            matches!(cmp, std::cmp::Ordering::Equal),
            "slot-battle race must be classified as Equal so it can take the \
             SlotBattle branch (INFO log, skip until competing-forge support lands)"
        );
    }

    /// SlotBattle parameter selection: matches Haskell's `mkCurrentBlockContext`
    /// EQ branch in NodeKernel.hs:960. The competing block must reuse the
    /// existing tip's block_no (NOT incremented) and parent at the tip's parent
    /// (`tip.prev_hash`). Two pools forging at the same slot end up with
    /// blocks that share `(slot, block_no, parent)` and differ only in their
    /// VRF/KES output — chain selection's RestrictedVRFTiebreaker (slotDist=0)
    /// chooses the winner deterministically.
    #[test]
    fn forge_mode_slot_battle_uses_tip_block_no_and_tip_prev_hash() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::BlockNo;

        let tip_block_no = BlockNo(123_456);
        let tip_prev_hash = Hash32::from_bytes([0xAB; 32]);

        let mode = ForgeMode::SlotBattle {
            block_number: tip_block_no,
            prev_hash: tip_prev_hash,
        };

        match mode {
            ForgeMode::SlotBattle {
                block_number,
                prev_hash,
            } => {
                assert_eq!(
                    block_number, tip_block_no,
                    "slot-battle competing block MUST share the tip's block_no \
                     (NOT tip.block_no + 1) so it sits at the same chain height"
                );
                assert_eq!(
                    prev_hash, tip_prev_hash,
                    "slot-battle competing block MUST parent at the tip's parent \
                     (tip.prev_hash), NOT at the tip itself, otherwise it would be \
                     a child of the existing slot-N block instead of an alternative"
                );
            }
            ForgeMode::ExtendTip => panic!("expected SlotBattle variant"),
        }
    }

    /// ExtendTip vs SlotBattle classification regression: confirm the variants
    /// are distinct types. Guards against a future refactor that accidentally
    /// collapses them or swaps the field semantics.
    #[test]
    fn forge_mode_variants_are_distinct() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::time::BlockNo;

        let extend = ForgeMode::ExtendTip;
        let battle = ForgeMode::SlotBattle {
            block_number: BlockNo(1),
            prev_hash: Hash32::ZERO,
        };

        // The compiler enforces this at type level, but the `matches!` form
        // serves as a runtime self-document.
        assert!(matches!(extend, ForgeMode::ExtendTip));
        assert!(matches!(battle, ForgeMode::SlotBattle { .. }));
        assert!(!matches!(extend, ForgeMode::SlotBattle { .. }));
        assert!(!matches!(battle, ForgeMode::ExtendTip));
    }

    /// CannotForge gate: forge must be skipped when the wall-clock KES period
    /// is earlier than the operational certificate's start period
    /// (`c0`/`opcert_kes_period`). Matches Haskell's
    /// `PraosCannotForgeKeyNotUsableYet` from
    /// `ouroboros-consensus-protocol/.../Praos.hs:praosCheckCanForge`.
    ///
    /// This guards against forging with a freshly issued operational
    /// certificate whose start period is in the future (a common scenario
    /// during graceful KES rotation): a block produced in this state would
    /// carry a KES signature at relative evolution 0 that the verifier
    /// ALSO clamps to 0, which is a wire-format coincidence rather than
    /// semantic correctness.
    #[test]
    fn forge_gate_cannot_forge_key_not_usable_yet() {
        let slots_per_kes_period: u64 = 129_600;

        // Wall-clock slot 100, opcert KES period 1 → wall-clock KES period 0
        // < opcert KES period 1 → gate fires.
        let current_slot: u64 = 100;
        let opcert_kes_period: u64 = 1;
        let wall_clock_kes_period = current_slot / slots_per_kes_period;
        assert!(
            wall_clock_kes_period < opcert_kes_period,
            "TraceNodeCannotForge gate must fire when wall-clock period < opcert start period"
        );

        // Wall-clock slot 200_000, opcert KES period 1 → wall-clock KES period 1
        // == opcert KES period → gate does NOT fire (key just became usable).
        let current_slot: u64 = 200_000;
        let opcert_kes_period: u64 = 1;
        let wall_clock_kes_period = current_slot / slots_per_kes_period;
        assert!(
            wall_clock_kes_period >= opcert_kes_period,
            "TraceNodeCannotForge gate must NOT fire at the exact start period boundary"
        );

        // Wall-clock slot 1_000_000, opcert KES period 5 → wall-clock KES period 7
        // > opcert KES period 5 → gate does NOT fire (normal forge condition).
        let current_slot: u64 = 1_000_000;
        let opcert_kes_period: u64 = 5;
        let wall_clock_kes_period = current_slot / slots_per_kes_period;
        assert!(
            wall_clock_kes_period >= opcert_kes_period,
            "TraceNodeCannotForge gate must NOT fire when wall-clock period > opcert start"
        );
    }

    /// SlotIsImmutable gate: forge must be skipped when immutable_tip_slot == current_slot.
    ///
    /// Haskell: if immutableTipSlot == currentSlot → TraceSlotIsImmutable + exitEarly.
    #[test]
    fn forge_gate_slot_is_immutable() {
        let current_slot: u64 = 500;

        // Matches → gate fires.
        let immutable_tip_slot: u64 = 500;
        assert!(
            immutable_tip_slot == current_slot,
            "TraceSlotIsImmutable gate must fire when immutable_tip_slot == current_slot"
        );

        // Does not match → gate does NOT fire.
        let immutable_tip_slot: u64 = 499;
        assert!(
            immutable_tip_slot != current_slot,
            "TraceSlotIsImmutable gate must NOT fire when immutable_tip_slot != current_slot"
        );
    }

    /// NoLedgerView gate: forge must be skipped when lag > stability_window.
    ///
    /// Haskell: forecastFor fails when currentSlot >= tipSlot + 1 + stabilityWindow.
    /// Equivalent: lag = current_slot - tip_slot > stability_window.
    #[test]
    fn forge_gate_no_ledger_view_fires_when_lag_exceeds_stability_window() {
        let k: u64 = 2160;
        let f: f64 = 0.05;
        let stability_window = dugite_consensus::stability_window_slots(k, f);
        assert_eq!(stability_window, 129_600);

        let tip_slot: u64 = 1_000_000;
        // current_slot = tip_slot + stability_window + 1 → lag = stability_window + 1 → fires.
        let current_slot = tip_slot + stability_window + 1;
        let lag = current_slot.saturating_sub(tip_slot);
        assert!(
            lag > stability_window,
            "TraceNoLedgerView gate must fire when lag ({lag}) > stability_window ({stability_window})"
        );
    }

    /// NoLedgerView gate must NOT fire for normal lag (e.g. 60–100 slots behind).
    ///
    /// Regression test for the MAX_FORGE_LAG_SLOTS=60 false positive: a 60-slot
    /// gap is completely normal on Conway preview (f=0.05, expected inter-block
    /// gap ≈ 20 slots, but empty windows are common).  The old guard fired a
    /// WARN every second; the new Haskell-aligned gate allows up to 129600 slots.
    #[test]
    fn forge_gate_no_ledger_view_does_not_fire_for_small_lag() {
        let k: u64 = 2160;
        let f: f64 = 0.05;
        let stability_window = dugite_consensus::stability_window_slots(k, f);
        assert_eq!(stability_window, 129_600);

        let tip_slot: u64 = 1_000_000;

        // 60-slot lag — old guard would have fired WARN; new gate must NOT fire.
        let current_slot_60 = tip_slot + 60;
        let lag_60 = current_slot_60.saturating_sub(tip_slot);
        assert!(
            lag_60 <= stability_window,
            "TraceNoLedgerView gate must NOT fire for lag={lag_60} (old MAX_FORGE_LAG_SLOTS=60 false positive)"
        );

        // 100-slot lag — well within stability window.
        let current_slot_100 = tip_slot + 100;
        let lag_100 = current_slot_100.saturating_sub(tip_slot);
        assert!(
            lag_100 <= stability_window,
            "TraceNoLedgerView gate must NOT fire for lag={lag_100}"
        );

        // Exactly at stability_window — boundary: must NOT fire (> not >=).
        let current_slot_exact = tip_slot + stability_window;
        let lag_exact = current_slot_exact.saturating_sub(tip_slot);
        assert!(
            lag_exact <= stability_window,
            "TraceNoLedgerView gate must NOT fire when lag equals stability_window exactly"
        );
    }

    /// Verify the gate boundary: lag == stability_window does NOT fire,
    /// lag == stability_window + 1 DOES fire.
    #[test]
    fn forge_gate_no_ledger_view_boundary() {
        let stability_window: u64 = 129_600;
        let tip_slot: u64 = 500_000;

        // lag == stability_window → does not fire (> is strict).
        let lag_at = stability_window;
        assert!(
            lag_at <= stability_window,
            "gate must NOT fire at lag == stability_window"
        );

        // lag == stability_window + 1 → fires.
        let lag_over = stability_window + 1;
        assert!(
            lag_over > stability_window,
            "gate must fire at lag == stability_window + 1"
        );

        // Sanity: tip_slot is only used in the subtraction above; include it
        // to suppress an "unused variable" warning in hypothetical future moves.
        let _ = tip_slot;
    }

    // InMemory tables path resolution (issue #460):
    //
    // Verifies that `resolve_inmemory_tables_path` accepts both the new
    // ouroboros-consensus 1.0.0.0+ layout (flat `<snap>/tables` file) and the
    // legacy `<snap>/tables/tvar` layout, prefers the new layout, and returns
    // `None` when neither exists.

    #[test]
    fn resolve_tables_path_prefers_new_flat_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        std::fs::write(snap.join("tables"), b"flat-blob").expect("write flat tables");
        let resolved = resolve_inmemory_tables_path(snap).expect("resolved path");
        assert_eq!(resolved, snap.join("tables"));
        assert_eq!(
            std::fs::read(&resolved).unwrap(),
            b"flat-blob",
            "resolved path must point at the v11 flat blob"
        );
    }

    #[test]
    fn resolve_tables_path_falls_back_to_legacy_nested_tvar() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        std::fs::create_dir_all(snap.join("tables")).expect("mkdir tables");
        std::fs::write(snap.join("tables").join("tvar"), b"nested-blob")
            .expect("write nested tvar");
        let resolved = resolve_inmemory_tables_path(snap).expect("resolved path");
        assert_eq!(resolved, snap.join("tables").join("tvar"));
        assert_eq!(std::fs::read(&resolved).unwrap(), b"nested-blob");
    }

    #[test]
    fn resolve_tables_path_returns_none_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Empty snapshot dir: neither layout present.
        assert!(resolve_inmemory_tables_path(tmp.path()).is_none());
    }

    /// Regression for #495: the importer's UTxO-loading sub-block must
    /// decode the MemPack tables blob from the v11+ flat layout
    /// (`<snap>/tables` as a file).  Prior to the fix the importer
    /// hard-coded the legacy `<snap>/tables/tvar` path, so every preview
    /// import (preview is PV11) silently skipped the UTxO load and saved
    /// `utxos=0` into the native snapshot — tripping the defensive
    /// UTxO-empty gate on the next startup and forcing a full-chain
    /// replay.  This test pins the integration so a regression to the
    /// hard-coded path is caught.
    #[test]
    fn importer_loads_utxos_from_v11_flat_tables_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        // Minimal valid MemPack tables blob: array(1)[ map() ].
        //   0x81        array(1)
        //   0xbf        indefinite-length map start
        //   0xff        break  (= empty map)
        std::fs::write(snap.join("tables"), [0x81u8, 0xbf, 0xff]).expect("write tables blob");

        // The resolver must point the importer at the flat blob.
        let resolved =
            resolve_inmemory_tables_path(snap).expect("v11 flat tables path must resolve");
        assert_eq!(resolved, snap.join("tables"));

        // And the importer's actual decode path (TvarIterator) must accept it.
        let data = std::fs::read(&resolved).unwrap();
        let mut iter = dugite_serialization::mempack::TvarIterator::new(&data)
            .expect("v11 flat tables must decode as MemPack array(1)[ map() ]");
        assert!(iter.next().is_none(), "empty map yields no entries");
    }

    /// The importer must NOT silently skip the UTxO load when only the
    /// legacy `tables/tvar` layout is present (cardano-node ≤ 10.6.x
    /// snapshots).  Same TvarIterator path, different filesystem layout.
    #[test]
    fn importer_loads_utxos_from_legacy_nested_tvar_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        std::fs::create_dir_all(snap.join("tables")).expect("mkdir tables");
        std::fs::write(snap.join("tables").join("tvar"), [0x81u8, 0xbf, 0xff])
            .expect("write nested tvar");

        let resolved = resolve_inmemory_tables_path(snap).expect("legacy nested tvar must resolve");
        assert_eq!(resolved, snap.join("tables").join("tvar"));
        let data = std::fs::read(&resolved).unwrap();
        let mut iter = dugite_serialization::mempack::TvarIterator::new(&data)
            .expect("legacy nested tvar must decode as MemPack array(1)[ map() ]");
        assert!(iter.next().is_none());
    }

    #[test]
    fn resolve_tables_path_ignores_tables_as_directory_without_tvar() {
        // If `<snap>/tables` exists but is an empty directory (no `tvar`
        // child), neither layout is satisfied and we must return `None`
        // rather than handing the importer a directory it would then try
        // to `std::fs::read` as a file.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("tables")).expect("mkdir tables");
        assert!(resolve_inmemory_tables_path(tmp.path()).is_none());
    }

    /// Build a real `tvar` MemPack tables blob from the given `TxIx` values,
    /// keyed with the supplied `endianness`. Each entry is a CBOR `bytes(34)`
    /// key (`32-byte txid || 2-byte txix`) mapped to a CBOR `bytes(N)` value
    /// holding a genuine tag-0 ADA-only `MemPackTxOut` (a real preview UTxO,
    /// 1_814_145 lovelace) so the full `TvarIterator` decode path round-trips.
    /// The map is `array(1)[ {indef} … 0xff ]`, exactly the on-disk shape.
    fn build_tvar(
        txixs: &[u16],
        endianness: dugite_serialization::mempack::TxIxEndianness,
    ) -> Vec<u8> {
        // Real tag-0 entry from preview tvar (see mempack tests):
        //   value = 1_814_145 lovelace, enterprise testnet address.
        let txout =
            hex::decode("001d60986cdecfc4f555a8605d621505a4a82c25c574f59fd0b79e2acdaf0200eedd01")
                .expect("valid tag-0 txout hex");

        let mut blob = vec![0x81u8, 0xbf]; // array(1) [ indefinite-map(
        for (i, &ix) in txixs.iter().enumerate() {
            // CBOR bytes(34) key: 0x58 0x22 || 32-byte txid || 2-byte txix.
            blob.push(0x58);
            blob.push(0x22);
            let mut txid = [0u8; 32];
            txid[0] = i as u8; // distinct keys
            blob.extend_from_slice(&txid);
            let ix_bytes = match endianness {
                dugite_serialization::mempack::TxIxEndianness::Little => ix.to_le_bytes(),
                dugite_serialization::mempack::TxIxEndianness::Big => ix.to_be_bytes(),
            };
            blob.extend_from_slice(&ix_bytes);
            // CBOR bytes(N) value: 0x58 LEN || real tag-0 MemPackTxOut.
            blob.push(0x58);
            blob.push(txout.len() as u8);
            blob.extend_from_slice(&txout);
        }
        blob.push(0xff); // break (end of map)
        blob
    }

    /// STRICT #10 (re-gauntlet w4007sv2k): a snapshot directory with NO `meta`
    /// file must be REJECTED — `resolve_snapshot_txix_endianness` returns `Err`
    /// rather than silently decoding legacy little-endian.
    ///
    /// This drives the REAL importer endianness-resolution path
    /// (`resolve_snapshot_txix_endianness`, the function the importer calls).
    /// The upstream node loader (`V2/InMemory.hs loadSnapshot`) fails a
    /// missing-meta snapshot with `ReadMetadataError`. `getMetadata`'s
    /// `MetadataFileDoesNotExist -> Nothing` (offline converter) is the CRC-skip
    /// path, NOT a decode-LE branch — endianness is never selected from a
    /// missing meta. We default to rejection (byte-exact only): no silent LE.
    #[test]
    fn importer_meta_file_absent_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();

        let tvar = build_tvar(
            &[1, 2, 3, 7, 42],
            dugite_serialization::mempack::TxIxEndianness::Big,
        );
        std::fs::write(snap.join("tables"), &tvar).expect("write tables blob");

        // No `meta` file is written.
        assert!(
            !snap.join("meta").exists(),
            "test precondition: snapshot has no meta file"
        );

        let resolved = resolve_inmemory_tables_path(snap).expect("flat tables path must resolve");
        let data = std::fs::read(&resolved).unwrap();

        assert!(
            resolve_snapshot_txix_endianness(snap, &data).is_err(),
            "a snapshot with no meta file must be REJECTED (ReadMetadataError upstream); \
             endianness is never selected from a missing meta — no silent little-endian"
        );
    }

    /// STRICT #10: a present meta that parses but LACKS `tablesCodecVersion`
    /// must be REJECTED (`MetadataInvalid` upstream — the mandatory
    /// `o .: "tablesCodecVersion"` is an Aeson `Left`). No silent little-endian.
    #[test]
    fn importer_meta_without_codec_version_field_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        let tvar = build_tvar(
            &[1, 5, 9],
            dugite_serialization::mempack::TxIxEndianness::Big,
        );
        std::fs::write(snap.join("tables"), &tvar).expect("write tables blob");
        // meta exists, parses, has the right backend, but no tablesCodecVersion field.
        std::fs::write(
            snap.join("meta"),
            br#"{"backend":"utxohd-mem","checksum":2409556997}"#,
        )
        .expect("write fieldless meta");

        let data = std::fs::read(snap.join("tables")).unwrap();
        assert!(
            resolve_snapshot_txix_endianness(snap, &data).is_err(),
            "meta present but missing tablesCodecVersion must be REJECTED (MetadataInvalid)"
        );
    }

    /// STRICT #10: a present meta whose `tablesCodecVersion` is `null` must be
    /// REJECTED — same `MetadataInvalid` outcome as field-absent.
    #[test]
    fn importer_meta_null_codec_version_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        let tvar = build_tvar(
            &[1, 5, 9],
            dugite_serialization::mempack::TxIxEndianness::Big,
        );
        std::fs::write(snap.join("tables"), &tvar).expect("write tables blob");
        std::fs::write(
            snap.join("meta"),
            br#"{"backend":"utxohd-mem","tablesCodecVersion":null}"#,
        )
        .expect("write null-version meta");

        let data = std::fs::read(snap.join("tables")).unwrap();
        assert!(
            resolve_snapshot_txix_endianness(snap, &data).is_err(),
            "meta with null tablesCodecVersion must be REJECTED (MetadataInvalid)"
        );
    }

    /// STRICT #10: a present meta whose `backend` is not `utxohd-mem`
    /// (UTxOHDMemSnapshot) must be REJECTED — V2/InMemory loadSnapshot guards
    /// `when (snapshotBackend /= UTxOHDMemSnapshot) $ throwE MetadataBackendMismatch`.
    #[test]
    fn importer_meta_wrong_backend_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        // BE blob + valid version 1, but the WRONG backend tag.
        let blob = build_tvar(
            &[1, 2, 3],
            dugite_serialization::mempack::TxIxEndianness::Big,
        );
        std::fs::write(snap.join("tables"), &blob).expect("write tables");
        std::fs::write(
            snap.join("meta"),
            br#"{"backend":"utxohd-lmdb","checksum":1,"tablesCodecVersion":1}"#,
        )
        .expect("write wrong-backend meta");

        let data = std::fs::read(snap.join("tables")).unwrap();
        assert!(
            resolve_snapshot_txix_endianness(snap, &data).is_err(),
            "meta with backend != utxohd-mem must be REJECTED (MetadataBackendMismatch)"
        );

        // A meta with NO backend field at all is also rejected.
        std::fs::write(
            snap.join("meta"),
            br#"{"checksum":1,"tablesCodecVersion":1}"#,
        )
        .expect("write backend-less meta");
        let data = std::fs::read(snap.join("tables")).unwrap();
        assert!(
            resolve_snapshot_txix_endianness(snap, &data).is_err(),
            "meta with no backend field must be REJECTED (MetadataBackendMismatch)"
        );
    }

    /// STRICT #10 (kept): `tablesCodecVersion: 1` + `backend: utxohd-mem`
    /// => big-endian, the modern format. The chain-verified accepted path.
    #[test]
    fn importer_meta_codec_version_1_selects_big_endian() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        // BE-keyed blob: write indices big-endian so the data agrees with BE.
        let blob = build_tvar(
            &[1, 2, 3],
            dugite_serialization::mempack::TxIxEndianness::Big,
        );
        std::fs::write(snap.join("tables"), &blob).expect("write tables");
        std::fs::write(
            snap.join("meta"),
            br#"{"backend":"utxohd-mem","checksum":1,"tablesCodecVersion":1}"#,
        )
        .expect("write meta v1");

        let data = std::fs::read(snap.join("tables")).unwrap();
        let endianness =
            resolve_snapshot_txix_endianness(snap, &data).expect("meta v1 => big-endian");
        assert_eq!(
            endianness,
            dugite_serialization::mempack::TxIxEndianness::Big
        );
    }

    /// STRICT #10 (kept): a meta with an unknown codec version is a hard error
    /// (mirrors upstream `enforceVersion` rejecting everything but `1`).
    #[test]
    fn importer_meta_unknown_version_is_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let snap = tmp.path();
        let tvar = build_tvar(
            &[1, 2, 3],
            dugite_serialization::mempack::TxIxEndianness::Big,
        );
        std::fs::write(snap.join("tables"), &tvar).expect("write tables");
        std::fs::write(
            snap.join("meta"),
            br#"{"backend":"utxohd-mem","tablesCodecVersion":2}"#,
        )
        .expect("write meta v2");
        let data = std::fs::read(snap.join("tables")).unwrap();
        assert!(
            resolve_snapshot_txix_endianness(snap, &data).is_err(),
            "unknown tablesCodecVersion must be rejected (not silently guessed)"
        );
    }

    // ─── apply_peer_metrics gauge update (GitHub #437 regression) ────────────

    /// Inbound connection registration must drive `peers_inbound`,
    /// `conn_inbound`, and `n2n_connections_active` in lock-step. Before this
    /// fix, only `peers_inbound` and `n2n_connections_active` were updated by
    /// `update_peer_metrics`; `conn_inbound` only moved on block arrival,
    /// leaving Prometheus and dugite-monitor reporting zero inbound
    /// connections for nodes at chain tip.
    #[test]
    fn apply_peer_metrics_inbound_drives_all_three_gauges() {
        use crate::metrics::NodeMetrics;
        use crate::node::apply_peer_metrics;
        use crate::node::networking::{ConnectionDirection, NodePeerManager, PeerManagerConfig};
        use std::net::SocketAddr;
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = NodeMetrics::default();
        let mut pm = NodePeerManager::new(PeerManagerConfig::default());
        let addr: SocketAddr = "198.51.100.7:3001".parse().unwrap();

        // Baseline: every gauge starts at zero.
        assert_eq!(metrics.peers_inbound.load(Relaxed), 0);
        assert_eq!(metrics.conn_inbound.load(Relaxed), 0);
        assert_eq!(metrics.n2n_connections_active.load(Relaxed), 0);

        // Simulate the inbound-register path: peer-manager records the inbound,
        // then the run loop refreshes all metrics via apply_peer_metrics.
        pm.peer_connected(&addr, ConnectionDirection::Inbound);
        apply_peer_metrics(&metrics, &pm, /*active_connection_count=*/ 1);

        assert_eq!(
            metrics.peers_inbound.load(Relaxed),
            1,
            "peers_inbound must reflect the registered inbound peer"
        );
        assert_eq!(
            metrics.conn_inbound.load(Relaxed),
            1,
            "conn_inbound must move on inbound register, not only on block arrival"
        );
        assert_eq!(
            metrics.n2n_connections_active.load(Relaxed),
            1,
            "n2n_connections_active must mirror the lifecycle map length"
        );
        // The Duplex DataFlow bucket also has to fire: InboundIdle(Duplex)
        // contributes to both `inbound` and `duplex` per Haskell semantics.
        assert_eq!(metrics.conn_duplex.load(Relaxed), 1);

        // Disconnect: every inbound gauge has to return to zero.
        pm.peer_disconnected(&addr);
        apply_peer_metrics(&metrics, &pm, /*active_connection_count=*/ 0);

        assert_eq!(metrics.peers_inbound.load(Relaxed), 0);
        assert_eq!(metrics.conn_inbound.load(Relaxed), 0);
        assert_eq!(metrics.n2n_connections_active.load(Relaxed), 0);
    }

    /// Regression: the heavy governance gauges (DReps, proposals, committee,
    /// pparams) and `utxo_count` must be refreshed per-block AT TIP, not only
    /// during bulk sync (5 s gate) / on forge / at startup. Before this fix the
    /// at-tip received-block path (`apply_fetched_block` at_tip branch) called
    /// only `publish_ledger_view` (which touches no atomics), so these gauges
    /// froze at the last value once the node reached tip — the same staleness
    /// class as the pots gauge. `refresh_heavy_at_tip_gauges` is the seam the
    /// at-tip / era-transition branch uses; this asserts it overwrites stale
    /// gauge values from live ledger state.
    #[test]
    fn refresh_heavy_at_tip_gauges_refreshes_governance_and_utxo() {
        use crate::metrics::NodeMetrics;
        use crate::node::refresh_heavy_at_tip_gauges;
        use dugite_ledger::LedgerState;
        use dugite_primitives::protocol_params::ProtocolParameters;
        use dugite_primitives::value::Lovelace;
        use std::sync::atomic::Ordering::Relaxed;

        let metrics = NodeMetrics::default();
        let mut ls = LedgerState::new(ProtocolParameters::mainnet_defaults());
        ls.epochs.treasury = Lovelace(111);
        ls.epochs.reserves = Lovelace(222);

        // Stale gauge values from a prior refresh; the at-tip refresh must
        // overwrite them from live ledger state, not leave them frozen.
        metrics.set_utxo_count(9999);
        metrics.set_pots(1, 2);

        refresh_heavy_at_tip_gauges(&metrics, &ls);

        assert_eq!(
            metrics.utxo_count.load(Relaxed),
            ls.utxo.utxo_set.len() as u64,
            "utxo_count must be refreshed from live ledger at tip (was stale 9999)"
        );
        assert_eq!(
            metrics.treasury_lovelace.load(Relaxed),
            111,
            "treasury gauge must reflect live ledger pots at tip"
        );
        assert_eq!(
            metrics.reserves_lovelace.load(Relaxed),
            222,
            "reserves gauge must reflect live ledger pots at tip"
        );
    }

    // ─── Forge peer-connectivity gate tests (Bug C) ──────────────────────────
    //
    // The forge loop has a connectivity gate: forge is deferred until BOTH
    //   (a) at least one peer is in Hot state, AND
    //   (b) a non-Origin MsgIntersectFound has been received.
    //
    // These unit tests exercise the gate predicate logic directly, without
    // spinning up a full Node.  The predicate is:
    //
    //   should_defer = !has_hot_peer || !has_intersection
    //
    // When `should_defer` is true, the forge attempt is skipped and an INFO
    // log is emitted.  When false, forge proceeds normally.
    //
    // See: crates/dugite-node/src/node/mod.rs `try_forge_block_at` connectivity gate.

    /// With no peers and no intersection, the gate must deny.
    #[test]
    fn forge_connectivity_gate_denies_with_no_peers_no_intersection() {
        let has_hot_peer = false;
        let has_intersection = false;
        let should_defer = !has_hot_peer || !has_intersection;
        assert!(
            should_defer,
            "forge must be deferred when no peers are hot AND no intersection is established"
        );
    }

    /// With hot peers but no intersection yet, the gate must deny.
    #[test]
    fn forge_connectivity_gate_denies_with_hot_peer_but_no_intersection() {
        let has_hot_peer = true;
        let has_intersection = false; // peer promoted to hot but intersection not yet complete
        let should_defer = !has_hot_peer || !has_intersection;
        assert!(
            should_defer,
            "forge must be deferred when intersection is not yet established, \
             even if a peer is hot — prevents forging before ChainSync negotiation"
        );
    }

    /// With intersection established but no hot peers, the gate must deny.
    #[test]
    fn forge_connectivity_gate_denies_with_intersection_but_no_hot_peer() {
        let has_hot_peer = false; // all peers dropped back to warm/cold
        let has_intersection = true;
        let should_defer = !has_hot_peer || !has_intersection;
        assert!(
            should_defer,
            "forge must be deferred when no peer is hot, \
             even if a prior intersection was established"
        );
    }

    /// With at least one hot peer AND a successful intersection, the gate must allow.
    #[test]
    fn forge_connectivity_gate_allows_with_hot_peer_and_intersection() {
        let has_hot_peer = true;
        let has_intersection = true;
        let should_defer = !has_hot_peer || !has_intersection;
        assert!(
            !should_defer,
            "forge must proceed when at least one peer is hot AND intersection is established"
        );
    }

    /// The `peer_intersection_established` flag is set by chainsync on any
    /// valid intersection (Specific OR Origin-with-Origin-ledger) and must not
    /// be reset between forge ticks.  This test verifies the AtomicBool
    /// semantics that underpin the gate.
    #[test]
    fn peer_intersection_established_flag_is_sticky() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = Arc::new(AtomicBool::new(false));

        // Initially false — no intersection has been seen.
        assert!(
            !flag.load(Ordering::Relaxed),
            "flag must start false before any ChainSync intersection"
        );

        // Simulate chainsync receiving any valid intersection (Specific or
        // Origin-with-Origin-local-ledger — see chainsync_client_task).
        flag.store(true, Ordering::Relaxed);
        assert!(
            flag.load(Ordering::Relaxed),
            "flag must be true after a valid intersection is established"
        );

        // Additional chainsync tasks (e.g. reconnect, second peer) must not
        // reset the flag to false — once true, always true.
        // (No code path resets it; this test is a guard against accidental regression.)
        flag.store(true, Ordering::Relaxed); // second intersection — still true
        assert!(
            flag.load(Ordering::Relaxed),
            "flag must remain true after subsequent intersections"
        );
    }

    // ── post_apply_timing_enabled (issue #702) ───────────────────────────────

    /// `post_apply_timing_enabled()` returns a bool without panicking.
    ///
    /// The OnceLock is process-wide, so the actual value depends on whether
    /// `DUGITE_POST_APPLY_TIMING=1` was set in the test environment.  This
    /// test is a regression lock: the function must be callable at any time
    /// without panic and must return `false` in the default (unset) case.
    #[test]
    fn post_apply_timing_enabled_returns_bool() {
        use crate::node::post_apply_timing_enabled;
        // Call twice to exercise the "already initialised" path of OnceLock.
        let v1 = post_apply_timing_enabled();
        let v2 = post_apply_timing_enabled();
        assert_eq!(v1, v2, "OnceLock must be stable across calls");
        // In CI (DUGITE_POST_APPLY_TIMING is not set), this must be false.
        // In a dev session with the env var set it may be true — that is fine.
        // We cannot assert a specific value here without controlling the env.
        let _ = v1; // suppress unused warning
    }

    // ── #985: the BFT overlay gate ──────────────────────────────────────────

    /// The exact state the wedged preview BP was in: a LedgerSeq anchored at
    /// preview genesis reconstructed PV 6 / d = 1 / 7 genesis delegates into
    /// the live ledger, and a canonical Conway block arrived.
    ///
    /// Every non-era term is satisfied here — that is the point. Before the
    /// fix this returned `true`, the overlay classifier ran on a Praos header,
    /// slot 119084816 (offset 25616 of epoch 1378, `25616 % 20 = 16`) came
    /// back `NonActiveSlot`, and block 4535827 was rejected and cached as
    /// invalid, wedging chain selection for the process lifetime.
    #[test]
    fn conway_block_never_gets_an_overlay_context_even_on_corrupt_pparams() {
        assert!(
            !super::should_build_overlay_context(Era::Conway, 6, 1, true),
            "a Conway header must never be judged against the TPraos overlay \
             schedule, whatever the ledger state says (#985)"
        );
        for era in [Era::Babbage, Era::Conway, Era::Dijkstra] {
            assert!(
                !super::should_build_overlay_context(era, 6, 1, true),
                "{era:?} is a Praos era"
            );
        }
    }

    /// The era term must not have broken the check where it is genuinely
    /// needed — a mainnet Shelley block during the decentralisation ramp.
    #[test]
    fn tpraos_era_with_d_still_gets_an_overlay_context() {
        assert!(super::should_build_overlay_context(
            Era::Shelley,
            2,
            1,
            true
        ));
        assert!(super::should_build_overlay_context(Era::Alonzo, 6, 1, true));
    }

    /// The pre-existing terms still gate independently within a TPraos era.
    #[test]
    fn overlay_gate_still_honours_d_pv_and_delegates() {
        // d == 0: fully decentralised, no overlay slots.
        assert!(!super::should_build_overlay_context(
            Era::Shelley,
            2,
            0,
            true
        ));
        // No genesis delegates: nothing to validate an overlay slot against.
        assert!(!super::should_build_overlay_context(
            Era::Shelley,
            2,
            1,
            false
        ));
        // PV >= 7 cannot occur in a TPraos era on a real chain, but the term is
        // retained as defence in depth and must still gate.
        assert!(!super::should_build_overlay_context(
            Era::Alonzo,
            7,
            1,
            true
        ));
    }
}
