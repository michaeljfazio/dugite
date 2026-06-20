//! Block-apply performance benchmark — iteration 0 (data gathering).
//!
//! Loads a real mainnet ledger snapshot, reads a fixed slice of real
//! mainnet blocks from the ImmutableDB chunks OFFLINE (read-only), applies
//! them with full validation (`ValidateAll`), measures wall-time per block,
//! and emits a ledger-state fingerprint as a byte-exact regression check.
//!
//! ## Usage
//!
//! ```bash
//! # Build with profiling symbols (release-prof = release + line tables, no strip):
//! cargo build --profile release-prof --bin apply_bench -p dugite-node
//!
//! # Run (defaults use db-mainnet-pre-alonzo at ep287, Mary-era blocks, 3000 blocks):
//! ./target/release-prof/apply_bench
//!
//! # Run against a custom DB slice:
//! ./target/release-prof/apply_bench \
//!     --snapshot /path/to/db/ledger-snapshot-epochXXX-slotYYY.bin \
//!     --utxo-store /path/to/db/utxo-store \
//!     --immutable-dir /path/to/db/immutable \
//!     --start-slot <slot_after_snapshot> \
//!     --block-count 3000
//!
//! # Profile with samply (macOS):
//! samply record ./target/release-prof/apply_bench \
//!     --snapshot ... --utxo-store ... --immutable-dir ... \
//!     --start-slot ... --block-count 3000
//! ```
//!
//! ## Regression check
//!
//! The final line of stdout is:
//! ```
//! FINGERPRINT: <hex> blocks=<N> slot=<tip_slot>
//! ```
//! This is a Blake2b-256 hash of aggregate ledger accounting invariants
//! (UTxO count, reserves, treasury, epoch, tip slot, pool count, delegation count).
//! Any optimisation that changes this value has broken byte-exact correctness.
//!
//! ## Design notes
//!
//! - Reads ImmutableDB files OFFLINE (no writes, no ChainDB, no network).
//! - After each apply, replicates the per-block `publish_ledger_view` cost:
//!   structural clone of imbl maps (delegations, reward_accounts), Arc-clones
//!   of pool_params / governance / opcert_counters, ProtocolParameters clone.
//!   This ensures the benchmark measures the FULL per-block cost the live node
//!   incurs, not just `apply_block` in isolation.
//! - `DUGITE_BLOCK_APPLY_TIMING=1` activates per-phase breakdown logging.
//! - `RUST_LOG=warn` suppresses verbose ledger trace during profiling runs.

use dugite_ledger::state::{BlockValidationMode, LedgerState};
use dugite_primitives::hash::blake2b_256;
use dugite_serialization::decode_block_with_byron_epoch_length;
use dugite_storage::ImmutableDB;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

// Needed to replicate publish_ledger_view cost (structural clones, Arc-clones)
use std::sync::Arc;

// ── CLI argument parsing ──────────────────────────────────────────────────

struct Args {
    snapshot: PathBuf,
    utxo_store: PathBuf,
    immutable_dir: PathBuf,
    /// Restore the on-disk LSM UTxO store (`--utxo-store`) and attach it to the
    /// loaded ledger before applying blocks.  Required for v23+ (LSM/UTxO-HD)
    /// snapshots whose `.bin` no longer embeds the UTxO set (`utxo_count: 0`) —
    /// without it every input misses and the run falls through the
    /// "trusting on-chain consensus" divergence path, making the timing
    /// unrepresentative.  NOTE: applying MUTATES the store, so point
    /// `--utxo-store` at a COW clone, never the live store.
    restore_lsm: bool,
    /// First slot to include in the benchmark slice.
    start_slot: u64,
    /// Number of blocks to apply (0 = apply all available).
    block_count: usize,
    /// Print per-block timings to stderr.
    verbose: bool,
    /// Human-readable name for the benchmark preset (for diagnostics).
    preset_name: &'static str,
    /// If set, save the final ledger state to this path after applying
    /// all blocks (useful for generating snapshot checkpoints without
    /// running a full node).
    save_snapshot: Option<PathBuf>,
}

impl Args {
    fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut snapshot: Option<PathBuf> = None;
        let mut utxo_store: Option<PathBuf> = None;
        let mut immutable_dir: Option<PathBuf> = None;
        let mut start_slot: Option<u64> = None;
        let mut block_count = 3_000usize;
        let mut verbose = false;
        // `--alonzo` switches the default preset to the Alonzo/Plutus slice
        // (ep290 snapshot + /tmp/alonzo-bench-immutable CoW clone).
        let mut alonzo_preset = false;
        let mut alonzo_ep300_preset = false;
        let mut save_snapshot: Option<PathBuf> = None;
        let mut restore_lsm = false;

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--snapshot" => {
                    i += 1;
                    snapshot = Some(PathBuf::from(&args[i]));
                }
                "--utxo-store" => {
                    i += 1;
                    utxo_store = Some(PathBuf::from(&args[i]));
                }
                "--immutable-dir" => {
                    i += 1;
                    immutable_dir = Some(PathBuf::from(&args[i]));
                }
                "--start-slot" => {
                    i += 1;
                    start_slot = Some(args[i].parse().expect("--start-slot must be a u64"));
                }
                "--block-count" => {
                    i += 1;
                    block_count = args[i].parse().expect("--block-count must be a usize");
                }
                "--timing" | "-v" => {
                    verbose = true;
                }
                "--alonzo" => {
                    alonzo_preset = true;
                }
                "--alonzo-ep300" => {
                    alonzo_ep300_preset = true;
                }
                "--save-snapshot" => {
                    i += 1;
                    save_snapshot = Some(PathBuf::from(&args[i]));
                }
                "--restore-lsm" => {
                    restore_lsm = true;
                }
                "--help" | "-h" => {
                    eprintln!("{USAGE}");
                    std::process::exit(0);
                }
                other => {
                    eprintln!("Unknown argument: {other}");
                    eprintln!("{USAGE}");
                    std::process::exit(1);
                }
            }
            i += 1;
        }

        if alonzo_preset {
            // ── Alonzo/Plutus preset ────────────────────────────────────────
            // Snapshot: /tmp/alonzo-bench-snapshot-ep290.bin  (epoch 290, tip
            //           slot 39923024 — first block of Alonzo era on mainnet).
            // ImmutableDB: /tmp/alonzo-bench-immutable  (CoW clone of live
            //              db-mainnet immutable covering ep286-305).
            // Start slot: 39923025  (block immediately after snapshot tip).
            // Expected fingerprint (3000 blocks):
            //   fc788c7a071d65af46df1dd5674e412883763a73a41c6e1b8fde89865f30f5bf
            //
            // To run the Alonzo benchmark:
            //   ./target/release-prof/apply_bench --alonzo [--block-count N]
            let alonzo_immutable = PathBuf::from("/tmp/alonzo-bench-immutable");
            Args {
                snapshot: snapshot
                    .unwrap_or_else(|| PathBuf::from("/tmp/alonzo-bench-snapshot-ep290.bin")),
                utxo_store: utxo_store.unwrap_or_else(|| alonzo_immutable.clone()),
                immutable_dir: immutable_dir.unwrap_or_else(|| alonzo_immutable.clone()),
                start_slot: start_slot.unwrap_or(39_923_025),
                block_count,
                verbose,
                preset_name: "Alonzo-ep290",
                save_snapshot,
                restore_lsm,
            }
        } else if alonzo_ep300_preset {
            // ── Alonzo/Plutus ep300 preset ──────────────────────────────────
            // Snapshot: /tmp/alonzo-bench-snapshot-ep293.bin (actually epoch 300
            //           — named for history; generated by running 216K blocks from
            //           the ep290 snapshot). Tip slot 44345776.
            // ImmutableDB: /tmp/alonzo-bench-immutable (same as --alonzo)
            // Start slot: 44345777
            // Expected fingerprint (3000 blocks):
            //   c8a571cff2e6254fbc5ecb10b022b97d04048f2348ab21c3ad1928095dbbeefe
            // Plutus density: ~12% (346 evals in 3000 blocks)
            let alonzo_immutable = PathBuf::from("/tmp/alonzo-bench-immutable");
            Args {
                snapshot: snapshot
                    .unwrap_or_else(|| PathBuf::from("/tmp/alonzo-bench-snapshot-ep293.bin")),
                utxo_store: utxo_store.unwrap_or_else(|| alonzo_immutable.clone()),
                immutable_dir: immutable_dir.unwrap_or_else(|| alonzo_immutable.clone()),
                start_slot: start_slot.unwrap_or(44_345_777),
                block_count,
                verbose,
                preset_name: "Alonzo-ep300",
                save_snapshot,
                restore_lsm,
            }
        } else {
            // ── Mary-era default preset ─────────────────────────────────────
            // Blocks start immediately after the snapshot slot 38472727 (Mary era, epoch 286).
            // Expected fingerprint: 53d8e6dc015a95f36ccd76d38b300a0b44c52dd48251732e916c67b64d6da389
            let base = PathBuf::from("./db-mainnet-pre-alonzo");
            Args {
                snapshot: snapshot
                    .unwrap_or_else(|| base.join("ledger-snapshot-epoch286-slot38472727.bin")),
                utxo_store: utxo_store.unwrap_or_else(|| base.join("utxo-store")),
                immutable_dir: immutable_dir.unwrap_or_else(|| base.join("immutable")),
                start_slot: start_slot.unwrap_or(38_472_728),
                block_count,
                verbose,
                preset_name: "Mary-ep286",
                save_snapshot,
                restore_lsm,
            }
        }
    }
}

const USAGE: &str = r#"apply_bench — offline block-apply performance benchmark

USAGE:
  apply_bench [OPTIONS]

OPTIONS:
  --snapshot <path>       Ledger snapshot (.bin) to start from
  --utxo-store <path>     LSM UTxO store directory
  --immutable-dir <path>  ImmutableDB directory (immutable/*.chunk files)
  --start-slot <n>        First slot to include (default: 38472728)
  --block-count <n>       Number of blocks to apply (default: 3000, 0=all)
  --timing / -v           Print per-block timings to stderr
  --alonzo                Use the Alonzo/Plutus preset (ep290 snapshot, /tmp/alonzo-bench-*)
  --alonzo-ep300          Use the Alonzo ep300 preset (ep300 snapshot, /tmp/alonzo-bench-*)
  --save-snapshot <path>  Save the final ledger state to this path (for checkpoint generation)
  --restore-lsm           Open --utxo-store (LSM) and attach it before applying. Required for
                          v23+ LSM snapshots (utxo_count:0). MUTATES the store — use a COW clone.
  --help / -h             This message

PRESETS:
  (default)      Mary-era: ./db-mainnet-pre-alonzo  (ep286, Mary, no Plutus)
                 Expected fingerprint: 53d8e6dc015a95f36ccd76d38b300a0b44c52dd48251732e916c67b64d6da389
  --alonzo       Alonzo-ep290: /tmp/alonzo-bench-snapshot-ep290.bin  (ep290, early Alonzo, ~3% Plutus)
                 Expected fingerprint: fc788c7a071d65af46df1dd5674e412883763a73a41c6e1b8fde89865f30f5bf
  --alonzo-ep300 Alonzo-ep300: /tmp/alonzo-bench-snapshot-ep293.bin  (ep300, ~12% Plutus density)
                 Expected fingerprint: c8a571cff2e6254fbc5ecb10b022b97d04048f2348ab21c3ad1928095dbbeefe

PROFILING:
  Build: cargo build --profile release-prof --bin apply_bench -p dugite-node
  Run Mary:   samply record ./target/release-prof/apply_bench
  Run Alonzo: samply record ./target/release-prof/apply_bench --alonzo

REGRESSION:
  The FINGERPRINT on stdout must be identical across optimization iterations.
"#;

// ── publish_ledger_view cost simulation ──────────────────────────────────
//
// Replicates the per-block work that `publish_ledger_view` does in the live
// node.  Called after every `apply_block` in the benchmark loop so the
// reported timing includes the FULL per-block cost, not just apply.
//
// The live node calls `LedgerView::from_state` which does:
//   1. ProtocolParameters::clone (~few hundred bytes)
//   2. Arc::clone for pool_params, governance, epoch_blocks_by_pool,
//      opcert_counters (after fix: opcert_counters remains std HashMap)
//   3. imbl::HashMap::clone for delegations + reward_accounts (O(1) after fix;
//      was O(784K) iterate+collect in iteration-2)
//   4. EpochSnapshots::clone (Arc-clone chain)
//   5. Scalar copies
//
// We cannot call the actual `LedgerView::from_state` here because the
// `node::ledger_view` module is only compiled into `dugite-node` binary, not
// the lib.  We replicate the identical operations directly on `LedgerState`
// fields (all pub) and `std::hint::black_box` the results to prevent the
// optimizer from eliding the work.
#[inline(never)]
fn simulate_publish_ledger_view(state: &LedgerState) {
    // 1. ProtocolParameters clone (curPParams + prevPParams)
    let _pp = std::hint::black_box(state.epochs.protocol_params.clone());
    let _pp_prev = std::hint::black_box(state.epochs.prev_protocol_params.clone());

    // 2. Arc::clone for Arc-shared maps (O(1) reference-count bump)
    let _pool_params = std::hint::black_box(Arc::clone(&state.certs.pool_params));
    let _governance = std::hint::black_box(Arc::clone(&state.gov.governance));

    // 3. imbl::HashMap::clone for the two hot maps — O(1) structural clone
    //    (was the O(N) iterate+collect bottleneck in iteration-2)
    let _delegations = std::hint::black_box(state.certs.delegations.clone());
    let _reward_accounts = std::hint::black_box(state.certs.reward_accounts.clone());

    // 4. EpochSnapshots::clone — internally a chain of Arc-clones
    let _snapshots = std::hint::black_box(state.epochs.snapshots.clone());

    // 5. Scalar copies (nonces, tip, slot config — a few cache lines)
    let _epoch_nonce = std::hint::black_box(state.consensus.epoch_nonce);
    let _candidate_nonce = std::hint::black_box(state.consensus.candidate_nonce);
    let _evolving_nonce = std::hint::black_box(state.consensus.evolving_nonce);
}

// ── Ledger fingerprint ────────────────────────────────────────────────────

/// Compute a deterministic 32-byte fingerprint of the ledger state
/// covering aggregate accounting invariants (not the full UTxO set).
///
/// Covers: UTxO count, reserves, treasury, epoch, tip slot + block number,
/// pool count, delegation count.  Any optimisation that changes this value
/// has broken byte-exact correctness.
fn fingerprint(state: &LedgerState) -> [u8; 32] {
    let mut buf = Vec::with_capacity(128);

    let utxo_count = state.utxo.utxo_set.len() as u64;
    buf.extend_from_slice(&utxo_count.to_le_bytes());

    // epochs.reserves and epochs.treasury are Lovelace(u64) on EpochSubState
    let reserves = state.epochs.reserves.0;
    let treasury = state.epochs.treasury.0;
    buf.extend_from_slice(&reserves.to_le_bytes());
    buf.extend_from_slice(&treasury.to_le_bytes());

    // epoch is EpochNo(u64)
    buf.extend_from_slice(&state.epoch.0.to_le_bytes());

    // tip.point.slot() returns Option<SlotNo>, SlotNo(u64)
    let tip_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0u64);
    buf.extend_from_slice(&tip_slot.to_le_bytes());
    // block_number is BlockNo(u64)
    buf.extend_from_slice(&state.tip.block_number.0.to_le_bytes());

    let delegation_count = state.certs.delegations.len() as u64;
    buf.extend_from_slice(&delegation_count.to_le_bytes());

    let pool_count = state.certs.pool_params.len() as u64;
    buf.extend_from_slice(&pool_count.to_le_bytes());

    // blake2b_256 returns Hash32; use .as_bytes() to get &[u8;32]
    *blake2b_256(&buf).as_bytes()
}

// ── Percentile helper ─────────────────────────────────────────────────────

fn percentile_us(sorted: &[u64], pct: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * pct / 100.0) as usize;
    sorted[idx.min(sorted.len() - 1)]
}

// ── Main ─────────────────────────────────────────────────────────────────

fn main() {
    // Minimal tracing: warn level by default so profiling output is clean.
    // Override with RUST_LOG=info to see ledger detail.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "warn,dugite_ledger=warn,dugite_uplc=warn,dugite_serialization=warn",
                )
            }),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    // ── Step 1: Load ledger snapshot ─────────────────────────────────────
    eprintln!(
        "[apply_bench] Loading snapshot: {}",
        args.snapshot.display()
    );
    let t0 = Instant::now();
    let mut ledger = match LedgerState::load_snapshot(&args.snapshot) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[apply_bench] ERROR loading snapshot: {e}");
            eprintln!("             Ensure --snapshot points to a valid .bin file.");
            std::process::exit(1);
        }
    };
    eprintln!(
        "[apply_bench] Snapshot loaded in {:.1}s  (epoch={} tip_slot={})",
        t0.elapsed().as_secs_f64(),
        ledger.epoch.0,
        ledger.tip.point.slot().map(|s| s.0).unwrap_or(0),
    );
    eprintln!(
        "[apply_bench] UTxO set: {} entries (in-memory UtxoStore)",
        ledger.utxo.utxo_set.len()
    );

    // ── Optional: restore the on-disk LSM UTxO store ─────────────────────
    // v23+ (LSM/UTxO-HD) snapshots no longer embed the UTxO set in the `.bin`
    // (`utxo_count: 0`); the live set lives in `utxo-store/`.  Without
    // restoring it, every tx input misses → the apply path short-circuits
    // through the "trusting on-chain consensus" divergence branch and the
    // timing is NOT representative of real validation.  Mirrors the live
    // node's `UtxoStore::open` + `attach_utxo_store` wiring (mod.rs:1784).
    //
    // WARNING: applying blocks MUTATES the store (memtable inserts/deletes).
    // Always point `--utxo-store` at a COW clone (e.g. `cp -c -R`), never the
    // live store, or the benchmark will corrupt it.
    if args.restore_lsm {
        eprintln!(
            "[apply_bench] Restoring LSM UTxO store: {}",
            args.utxo_store.display()
        );
        let t_store = Instant::now();
        match dugite_ledger::utxo_store::UtxoStore::open(&args.utxo_store) {
            Ok(mut store) => {
                // Disable the WAL so per-UTxO inserts/deletes don't fsync to the
                // (clone) store during the run — we only care about the in-memory
                // apply cost, not LSM durability. Mirrors the catch-up path.
                store.set_wal_enabled(false);
                ledger.attach_utxo_store(store);
                eprintln!(
                    "[apply_bench] LSM store attached in {:.1}s — UTxO set: {} entries",
                    t_store.elapsed().as_secs_f64(),
                    ledger.utxo.utxo_set.len()
                );
            }
            Err(e) => {
                eprintln!("[apply_bench] ERROR opening LSM UTxO store: {e}");
                eprintln!("             Point --utxo-store at a COW clone of db-*/utxo-store.");
                std::process::exit(1);
            }
        }
    }

    // Diagnostic probe (issue: epoch-boundary reward-pots divergence).
    // When DUGITE_DUMP_LEDGER_PROBE is set, print the reward-update inputs and
    // pots straight from the loaded snapshot, then exit — used to confirm
    // whether `bprev_blocks_by_pool` (eta numerator) is empty at the boundary.
    if std::env::var("DUGITE_DUMP_LEDGER_PROBE").is_ok() {
        let e = &ledger.epochs;
        let bprev_sum: u64 = e.snapshots.bprev_blocks_by_pool.values().sum();
        eprintln!("[probe] epoch={}", ledger.epoch.0);
        eprintln!("[probe] treasury={}", e.treasury.0);
        eprintln!("[probe] reserves={}", e.reserves.0);
        eprintln!(
            "[probe] bprev_block_count={} bprev_blocks_by_pool.len={} bprev_sum={}",
            e.snapshots.bprev_block_count,
            e.snapshots.bprev_blocks_by_pool.len(),
            bprev_sum
        );
        eprintln!("[probe] ss_fee={}", e.snapshots.ss_fee.0);
        match e.snapshots.go.as_ref() {
            Some(g) => eprintln!(
                "[probe] go: epoch={} pools={} stake_creds={} go_block_count={} go_blocks_by_pool.len={}",
                g.epoch.0,
                g.pool_stake.len(),
                g.stake_distribution.len(),
                g.epoch_block_count,
                g.epoch_blocks_by_pool.len()
            ),
            None => eprintln!("[probe] go: NONE"),
        }
        eprintln!(
            "[probe] set_present={} mark_present={}",
            e.snapshots.set.is_some(),
            e.snapshots.mark.is_some()
        );
        eprintln!(
            "[probe] prev_d={}/{} prev_pv_major={}",
            e.prev_d.numerator, e.prev_d.denominator, e.prev_protocol_version_major
        );
        eprintln!(
            "[probe] reward_accounts={} rupd_addrs_rew_present={}",
            ledger.certs.reward_accounts.len(),
            e.rupd_addrs_rew.is_some()
        );
        std::process::exit(0);
    }

    // Note: We deliberately do NOT restore the LSM UTxO store from disk.
    // The in-memory UtxoStore populated from the snapshot is sufficient for
    // measuring the apply-path bottleneck (CBOR decode + validation + UTxO ops).
    // The LSM on-disk path adds I/O that is orthogonal to the CPU-bound
    // apply path we want to profile. For LSM-specific profiling, restore here.

    // ── Step 2: Open ImmutableDB (read-only) ────────────────────────────
    eprintln!(
        "[apply_bench] Opening ImmutableDB: {}",
        args.immutable_dir.display()
    );
    let immutable = match ImmutableDB::open(&args.immutable_dir) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[apply_bench] ERROR opening ImmutableDB: {e}");
            eprintln!("             Ensure --immutable-dir points to an immutable/ directory.");
            std::process::exit(1);
        }
    };
    eprintln!(
        "[apply_bench] ImmutableDB: {} blocks, tip slot={}",
        immutable.total_blocks(),
        immutable.tip_slot()
    );

    // ── Step 3: Collect block CBOR bytes ────────────────────────────────
    // Expand the slot range generously: Cardano mainnet averages ~1 block/slot,
    // so block_count * 2000 slots covers even sparse eras with room to spare.
    let end_slot = if args.block_count == 0 {
        immutable.tip_slot()
    } else {
        args.start_slot + (args.block_count as u64) * 2000
    };

    eprintln!(
        "[apply_bench] Fetching blocks: slot {}..{} (target {} blocks)",
        args.start_slot, end_slot, args.block_count
    );
    let t_collect = Instant::now();
    let raw_blocks: Vec<Vec<u8>> = immutable.get_blocks_in_slot_range(args.start_slot, end_slot);
    eprintln!(
        "[apply_bench] Fetched {} blocks in {:.1}s",
        raw_blocks.len(),
        t_collect.elapsed().as_secs_f64()
    );

    if raw_blocks.is_empty() {
        eprintln!(
            "[apply_bench] ERROR: no blocks in slot range {}..{}",
            args.start_slot, end_slot
        );
        eprintln!("             Check --start-slot and --immutable-dir.");
        std::process::exit(1);
    }

    // ── Step 4: Apply blocks ─────────────────────────────────────────────
    let apply_count = if args.block_count == 0 {
        raw_blocks.len()
    } else {
        args.block_count.min(raw_blocks.len())
    };

    eprintln!(
        "[apply_bench] Applying {apply_count} blocks (ValidateAll). \
         Set DUGITE_BLOCK_APPLY_TIMING=1 for per-phase breakdown."
    );

    let mut timings_us: Vec<u64> = Vec::with_capacity(apply_count);
    let mut decode_errors = 0usize;
    let mut apply_errors = 0usize;
    let mut total_txs = 0usize;

    // Mainnet Byron epoch length (slots per epoch in the Byron era)
    const BYRON_EPOCH_LENGTH: u64 = 21600;

    let t_apply_start = Instant::now();

    for raw in raw_blocks.iter().take(apply_count) {
        // Decode block CBOR → in-memory Block
        let block = match decode_block_with_byron_epoch_length(raw, BYRON_EPOCH_LENGTH) {
            Ok(b) => b,
            Err(e) => {
                if args.verbose {
                    eprintln!("[apply_bench] WARN decode error: {e}");
                }
                decode_errors += 1;
                continue;
            }
        };

        let slot = block.slot().0;
        let tx_count = block.transactions.len();

        // ── THE HOT PATH ──────────────────────────────────────────────
        // Includes apply_block + publish_ledger_view simulation so we measure
        // the full per-block cost the live node incurs at tip.
        let t_block = Instant::now();
        match ledger.apply_block(&block, BlockValidationMode::ValidateAll) {
            Ok(()) => {
                // Replicate publish_ledger_view cost INSIDE the timing window.
                // This is the fix: iteration-2 had an O(784K) iterate+collect
                // here that the original apply_bench never measured.
                simulate_publish_ledger_view(&ledger);
                let elapsed_us = t_block.elapsed().as_micros() as u64;
                timings_us.push(elapsed_us);
                total_txs += tx_count;
                if args.verbose {
                    eprintln!(
                        "[apply_bench] slot={slot} era={:?} txs={tx_count} {:.1}ms",
                        block.era,
                        elapsed_us as f64 / 1000.0,
                    );
                }
            }
            Err(e) => {
                if args.verbose {
                    eprintln!("[apply_bench] WARN slot={slot}: {e}");
                }
                apply_errors += 1;
            }
        }
    }

    let total_wall = t_apply_start.elapsed();
    let applied = timings_us.len();

    // ── Step 5: Compute statistics ───────────────────────────────────────
    timings_us.sort_unstable();
    let total_us: u64 = timings_us.iter().sum();
    let mean_us = if applied > 0 {
        total_us / applied as u64
    } else {
        0
    };
    let p50 = percentile_us(&timings_us, 50.0);
    let p90 = percentile_us(&timings_us, 90.0);
    let p99 = percentile_us(&timings_us, 99.0);
    let max_us = timings_us.last().copied().unwrap_or(0);
    let blk_per_sec = if total_wall.as_secs_f64() > 0.0 {
        applied as f64 / total_wall.as_secs_f64()
    } else {
        0.0
    };
    let ms_per_blk = if applied > 0 {
        total_wall.as_secs_f64() * 1000.0 / applied as f64
    } else {
        0.0
    };

    // ── Step 6: Print results ─────────────────────────────────────────────
    let stderr = std::io::stderr();
    let mut err = stderr.lock();

    writeln!(err).unwrap();
    writeln!(err, "╔══════════════════════════════════════════════════╗").unwrap();
    writeln!(err, "║   apply_bench — {} preset", args.preset_name).unwrap();
    writeln!(err, "╚══════════════════════════════════════════════════╝").unwrap();
    writeln!(err, "  blocks applied : {applied}").unwrap();
    writeln!(
        err,
        "  publish_view   : included per block (simulate_publish_ledger_view)"
    )
    .unwrap();
    writeln!(err, "  decode errors  : {decode_errors}").unwrap();
    writeln!(err, "  apply errors   : {apply_errors}").unwrap();
    writeln!(err, "  total txs      : {total_txs}").unwrap();
    writeln!(err, "  total wall     : {:.3}s", total_wall.as_secs_f64()).unwrap();
    writeln!(err, "  throughput     : {blk_per_sec:.1} blk/s").unwrap();
    writeln!(
        err,
        "  mean           : {ms_per_blk:.2}ms/blk  ({mean_us}µs)"
    )
    .unwrap();
    writeln!(
        err,
        "  p50            : {:.2}ms  ({p50}µs)",
        p50 as f64 / 1000.0
    )
    .unwrap();
    writeln!(
        err,
        "  p90            : {:.2}ms  ({p90}µs)",
        p90 as f64 / 1000.0
    )
    .unwrap();
    writeln!(
        err,
        "  p99            : {:.2}ms  ({p99}µs)",
        p99 as f64 / 1000.0
    )
    .unwrap();
    writeln!(
        err,
        "  max            : {:.2}ms  ({max_us}µs)",
        max_us as f64 / 1000.0
    )
    .unwrap();
    writeln!(err).unwrap();
    writeln!(err, "  final ledger state:").unwrap();
    writeln!(err, "    epoch    = {}", ledger.epoch.0).unwrap();
    writeln!(
        err,
        "    tip slot = {}",
        ledger.tip.point.slot().map(|s| s.0).unwrap_or(0)
    )
    .unwrap();
    writeln!(err, "    utxo cnt = {}", ledger.utxo.utxo_set.len()).unwrap();
    writeln!(err, "    reserves = {}", ledger.epochs.reserves.0).unwrap();
    writeln!(err, "    treasury = {}", ledger.epochs.treasury.0).unwrap();
    writeln!(err, "    pools    = {}", ledger.certs.pool_params.len()).unwrap();
    writeln!(err, "    delegs   = {}", ledger.certs.delegations.len()).unwrap();
    writeln!(err).unwrap();

    let fp = fingerprint(&ledger);
    let fp_hex: String = fp.iter().map(|b| format!("{:02x}", b)).collect();
    writeln!(err, "╔══════════════════════════════════════════════════╗").unwrap();
    writeln!(err, "║  REGRESSION FINGERPRINT (must be stable)         ║").unwrap();
    writeln!(err, "╚══════════════════════════════════════════════════╝").unwrap();
    writeln!(err, "  {fp_hex}").unwrap();
    writeln!(
        err,
        "  (blocks={applied} slot={})",
        ledger.tip.point.slot().map(|s| s.0).unwrap_or(0)
    )
    .unwrap();
    writeln!(err).unwrap();
    writeln!(
        err,
        "  Any change = correctness regression. Fix before merging."
    )
    .unwrap();

    // Machine-parseable fingerprint to stdout (for scripted comparison)
    println!(
        "FINGERPRINT: {fp_hex} blocks={applied} slot={}",
        ledger.tip.point.slot().map(|s| s.0).unwrap_or(0)
    );

    // ── Optional snapshot save ───────────────────────────────────────────
    // Useful for generating checkpoints at a specific epoch without
    // running a full node. Example:
    //   ./target/release-prof/apply_bench --alonzo --block-count 200000 \
    //     --save-snapshot /tmp/alonzo-bench-snapshot-ep293.bin
    if let Some(ref snap_path) = args.save_snapshot {
        eprintln!(
            "[apply_bench] Saving ledger snapshot to: {}",
            snap_path.display()
        );
        let t_save = Instant::now();
        match ledger.save_snapshot(snap_path) {
            Ok(()) => eprintln!(
                "[apply_bench] Snapshot saved in {:.1}s",
                t_save.elapsed().as_secs_f64()
            ),
            Err(e) => eprintln!("[apply_bench] ERROR saving snapshot: {e}"),
        }
    }
}
