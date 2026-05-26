//! Byte-exact verification harness for issue #670.
//!
//! Compares two `LedgerState` snapshots field-by-field for semantic
//! equality. The harness is the acceptance gate for the Mithril ancillary
//! import path: an operator runs the same `dugite-node mithril-import`
//! twice — once with `--include-ancillary` and once with
//! `--no-include-ancillary` — and the resulting `ledger-snapshot.bin`
//! files must be byte-exact equal at the same chain tip.
//!
//! ## Why semantic comparison (not byte-for-byte)
//!
//! `LedgerStateSnapshot` contains many `HashMap` and `HashSet` fields.
//! Bincode serialises hash maps in their internal iteration order, which
//! is non-deterministic across processes (and across hash-DOS-mitigation
//! seed values). A raw byte comparison would flag spurious differences
//! that are not actual ledger-state mismatches.
//!
//! Instead the harness walks each field of `LedgerStateSnapshot`
//! independently and compares using value semantics: maps are compared
//! by key set + per-key value equality, sets by element equality, etc.
//!
//! ## Output
//!
//! On success the harness prints a one-line summary and exits 0.
//! On any difference it prints a structured per-field diff and exits 1
//! so the harness can be used as a CI gate.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;
use std::path::Path;

use anyhow::{Context, Result};
use dugite_ledger::state::snapshot_format::LedgerStateSnapshot;
use dugite_ledger::state::LedgerState;

/// One detected difference between two snapshots.
#[derive(Debug)]
pub struct Diff {
    /// Dotted path identifying the field (`utxo_set`, `governance.dreps`, …).
    pub field: String,
    /// Human-readable description of the mismatch.
    pub detail: String,
}

/// Result of comparing two snapshots.
#[derive(Debug)]
pub struct DiffReport {
    pub diffs: Vec<Diff>,
}

impl DiffReport {
    pub fn is_empty(&self) -> bool {
        self.diffs.is_empty()
    }

    /// Pretty-print the diffs to stdout. Returns the diff count.
    pub fn print(&self) -> usize {
        if self.is_empty() {
            println!("PASS — snapshots are semantically equal");
        } else {
            println!("FAIL — {} field(s) differ:", self.diffs.len());
            for d in &self.diffs {
                println!("  • {}: {}", d.field, d.detail);
            }
        }
        self.diffs.len()
    }
}

/// Print the first N pool_params entries that differ between the two
/// snapshots. Useful for triaging the structural shape of the divergence
/// when `pool_params: value_mismatches=605` alone doesn't show what field
/// inside `PoolRegistration` actually differs.
pub fn print_first_pool_param_diffs(left_path: &Path, right_path: &Path, limit: usize) -> Result<()> {
    let l = LedgerState::load_snapshot(&resolve_snapshot_path(left_path)?)
        .with_context(|| format!("loading left {}", left_path.display()))?;
    let r = LedgerState::load_snapshot(&resolve_snapshot_path(right_path)?)
        .with_context(|| format!("loading right {}", right_path.display()))?;
    println!("\n=== first {limit} pool_params diffs ===");
    let mut emitted = 0;
    let mut common_keys: Vec<_> = l.certs.pool_params.keys().collect();
    common_keys.sort();
    for k in common_keys {
        if emitted >= limit {
            break;
        }
        let lv = l.certs.pool_params.get(k);
        let rv = r.certs.pool_params.get(k);
        if lv != rv {
            println!("  pool {}", k.to_hex());
            println!("    LEFT  = {:#?}", lv);
            println!("    RIGHT = {:#?}", rv);
            emitted += 1;
        }
    }
    Ok(())
}

/// Print a side-by-side overview of the two snapshots' scalar fields and
/// collection sizes. Useful for triage when the diff report alone lacks
/// the context to interpret what is happening (e.g. tip alignment,
/// state-coverage counts, governance/snapshot rollover position).
pub fn print_scalar_overview(left_path: &Path, right_path: &Path) -> Result<()> {
    let l = LedgerState::load_snapshot(&resolve_snapshot_path(left_path)?)
        .with_context(|| format!("loading left {}", left_path.display()))?;
    let r = LedgerState::load_snapshot(&resolve_snapshot_path(right_path)?)
        .with_context(|| format!("loading right {}", right_path.display()))?;
    let row = |k: &str, lv: String, rv: String| {
        let marker = if lv == rv { "✓" } else { "≠" };
        println!("  {marker} {k:30} L={lv:>30} R={rv:>30}");
    };
    println!("\n=== snapshot scalar overview ===");
    row("era", format!("{:?}", l.era), format!("{:?}", r.era));
    row("epoch", l.epoch.0.to_string(), r.epoch.0.to_string());
    row(
        "tip_slot",
        l.tip.point.slot().map(|s| s.0.to_string()).unwrap_or_default(),
        r.tip.point.slot().map(|s| s.0.to_string()).unwrap_or_default(),
    );
    row(
        "tip_block",
        l.tip.block_number.0.to_string(),
        r.tip.block_number.0.to_string(),
    );
    row(
        "tip_hash",
        l.tip.point.hash().map(|h| h.to_hex()).unwrap_or_default(),
        r.tip.point.hash().map(|h| h.to_hex()).unwrap_or_default(),
    );
    row(
        "treasury",
        l.epochs.treasury.0.to_string(),
        r.epochs.treasury.0.to_string(),
    );
    row(
        "reserves",
        l.epochs.reserves.0.to_string(),
        r.epochs.reserves.0.to_string(),
    );
    row(
        "epoch_fees",
        l.utxo.epoch_fees.0.to_string(),
        r.utxo.epoch_fees.0.to_string(),
    );
    row(
        "pending_donations",
        l.utxo.pending_donations.0.to_string(),
        r.utxo.pending_donations.0.to_string(),
    );
    row(
        "pool_params.len",
        l.certs.pool_params.len().to_string(),
        r.certs.pool_params.len().to_string(),
    );
    row(
        "dreps.len",
        l.gov.governance.dreps.len().to_string(),
        r.gov.governance.dreps.len().to_string(),
    );
    row(
        "proposals.len",
        l.gov.governance.proposals.len().to_string(),
        r.gov.governance.proposals.len().to_string(),
    );
    row(
        "delegations.len",
        l.certs.delegations.len().to_string(),
        r.certs.delegations.len().to_string(),
    );
    row(
        "reward_accounts.len",
        l.certs.reward_accounts.len().to_string(),
        r.certs.reward_accounts.len().to_string(),
    );
    row(
        "stake_map.len",
        l.certs.stake_distribution.stake_map.len().to_string(),
        r.certs.stake_distribution.stake_map.len().to_string(),
    );
    row(
        "pointer_map.len",
        l.certs.pointer_map.len().to_string(),
        r.certs.pointer_map.len().to_string(),
    );
    row(
        "ptr_stake.len",
        l.epochs.ptr_stake.len().to_string(),
        r.epochs.ptr_stake.len().to_string(),
    );
    row(
        "ptr_stake_excluded",
        l.epochs.ptr_stake_excluded.to_string(),
        r.epochs.ptr_stake_excluded.to_string(),
    );
    row(
        "opcert_counters.len",
        l.consensus.opcert_counters.len().to_string(),
        r.consensus.opcert_counters.len().to_string(),
    );
    row(
        "epoch_blocks_by_pool.len",
        l.consensus.epoch_blocks_by_pool.len().to_string(),
        r.consensus.epoch_blocks_by_pool.len().to_string(),
    );
    row(
        "epoch_block_count",
        l.consensus.epoch_block_count.to_string(),
        r.consensus.epoch_block_count.to_string(),
    );
    row(
        "pv",
        format!(
            "{}.{}",
            l.epochs.protocol_params.protocol_version_major,
            l.epochs.protocol_params.protocol_version_minor
        ),
        format!(
            "{}.{}",
            r.epochs.protocol_params.protocol_version_major,
            r.epochs.protocol_params.protocol_version_minor
        ),
    );
    row(
        "prev_pv_major",
        l.epochs.prev_protocol_version_major.to_string(),
        r.epochs.prev_protocol_version_major.to_string(),
    );
    row(
        "utxo_set.len",
        l.utxo.utxo_set.len().to_string(),
        r.utxo.utxo_set.len().to_string(),
    );
    println!();
    Ok(())
}

/// Load and compare two ledger snapshots at the given paths.
///
/// Each `path` must point either to a `ledger-snapshot.bin` file or to a
/// database directory containing one.
pub fn verify_snapshots(left_path: &Path, right_path: &Path) -> Result<DiffReport> {
    let left_snapshot_path = resolve_snapshot_path(left_path)?;
    let right_snapshot_path = resolve_snapshot_path(right_path)?;

    let left = LedgerState::load_snapshot(&left_snapshot_path)
        .with_context(|| format!("loading left snapshot {}", left_snapshot_path.display()))?;
    let right = LedgerState::load_snapshot(&right_snapshot_path)
        .with_context(|| format!("loading right snapshot {}", right_snapshot_path.display()))?;

    let left_view = LedgerStateSnapshot::from(&left);
    let right_view = LedgerStateSnapshot::from(&right);

    Ok(diff_snapshots(&left_view, &right_view))
}

fn resolve_snapshot_path(path: &Path) -> Result<std::path::PathBuf> {
    if path.is_file() {
        Ok(path.to_path_buf())
    } else if path.is_dir() {
        let snapshot = path.join("ledger-snapshot.bin");
        if !snapshot.exists() {
            anyhow::bail!(
                "{} is a directory but contains no ledger-snapshot.bin",
                path.display()
            );
        }
        Ok(snapshot)
    } else {
        anyhow::bail!(
            "{} does not exist or is neither a file nor directory",
            path.display()
        )
    }
}

/// Semantic diff of two `LedgerStateSnapshot` instances. Each detected
/// mismatch is captured as a `Diff` with a dotted-path field name.
pub fn diff_snapshots(left: &LedgerStateSnapshot, right: &LedgerStateSnapshot) -> DiffReport {
    let mut diffs = Vec::new();

    // ── Tip + era + epoch ────────────────────────────────────────────
    cmp_eq(&mut diffs, "tip", &left.tip, &right.tip);
    cmp_eq(&mut diffs, "era", &left.era, &right.era);
    cmp_eq(&mut diffs, "epoch", &left.epoch, &right.epoch);
    cmp_eq(
        &mut diffs,
        "epoch_length",
        &left.epoch_length,
        &right.epoch_length,
    );
    cmp_eq(
        &mut diffs,
        "shelley_transition_epoch",
        &left.shelley_transition_epoch,
        &right.shelley_transition_epoch,
    );
    cmp_eq(
        &mut diffs,
        "byron_epoch_length",
        &left.byron_epoch_length,
        &right.byron_epoch_length,
    );

    // ── Protocol parameters ──────────────────────────────────────────
    cmp_pretty(
        &mut diffs,
        "protocol_params",
        &left.protocol_params,
        &right.protocol_params,
    );
    cmp_pretty(
        &mut diffs,
        "prev_protocol_params",
        &left.prev_protocol_params,
        &right.prev_protocol_params,
    );
    cmp_eq(&mut diffs, "prev_d", &left.prev_d, &right.prev_d);
    cmp_eq(
        &mut diffs,
        "prev_protocol_version_major",
        &left.prev_protocol_version_major,
        &right.prev_protocol_version_major,
    );

    // ── Pots ─────────────────────────────────────────────────────────
    cmp_eq(&mut diffs, "treasury", &left.treasury, &right.treasury);
    cmp_eq(
        &mut diffs,
        "pending_donations",
        &left.pending_donations,
        &right.pending_donations,
    );
    cmp_eq(&mut diffs, "reserves", &left.reserves, &right.reserves);
    cmp_eq(
        &mut diffs,
        "epoch_fees",
        &left.epoch_fees,
        &right.epoch_fees,
    );

    // ── UTxO set ─────────────────────────────────────────────────────
    // UtxoSet's underlying HashMap is private; use the public API.
    // The single largest piece of state — compare via key set + per-key
    // value semantics.
    {
        let l = left.utxo_set.iter();
        let r = right.utxo_set.iter();
        if l.len() != r.len() {
            diffs.push(Diff {
                field: "utxo_set.len".into(),
                detail: format!("left={} right={}", l.len(), r.len()),
            });
        }
        let r_map: HashMap<_, _> = r.iter().cloned().collect();
        let mut missing_in_right = 0usize;
        let mut value_mismatches = 0usize;
        for (k, v) in &l {
            match r_map.get(k) {
                None => missing_in_right += 1,
                Some(rv) if rv != v => value_mismatches += 1,
                _ => {}
            }
        }
        let l_map: HashMap<_, _> = l.into_iter().collect();
        let missing_in_left = r_map.keys().filter(|k| !l_map.contains_key(k)).count();
        if missing_in_right + missing_in_left + value_mismatches > 0 {
            diffs.push(Diff {
                field: "utxo_set".into(),
                detail: format!(
                    "missing_in_right={missing_in_right} missing_in_left={missing_in_left} \
                     value_mismatches={value_mismatches}"
                ),
            });
        }
    }

    // ── Delegations / pools / rewards ────────────────────────────────
    diff_map(
        &mut diffs,
        "delegations",
        &left.delegations,
        &right.delegations,
    );
    diff_map(
        &mut diffs,
        "pool_params",
        &left.pool_params,
        &right.pool_params,
    );
    diff_map(
        &mut diffs,
        "future_pool_params",
        &left.future_pool_params,
        &right.future_pool_params,
    );
    diff_map(
        &mut diffs,
        "pending_retirements",
        &left.pending_retirements,
        &right.pending_retirements,
    );
    diff_map(
        &mut diffs,
        "reward_accounts",
        &left.reward_accounts,
        &right.reward_accounts,
    );
    diff_map(
        &mut diffs,
        "pointer_map",
        &left.pointer_map,
        &right.pointer_map,
    );
    diff_map(
        &mut diffs,
        "genesis_delegates",
        &left.genesis_delegates,
        &right.genesis_delegates,
    );
    diff_map(
        &mut diffs,
        "stake_key_deposits",
        &left.stake_key_deposits,
        &right.stake_key_deposits,
    );
    diff_map(
        &mut diffs,
        "pool_deposits",
        &left.pool_deposits,
        &right.pool_deposits,
    );
    cmp_eq(
        &mut diffs,
        "total_stake_key_deposits",
        &left.total_stake_key_deposits,
        &right.total_stake_key_deposits,
    );
    diff_set(
        &mut diffs,
        "script_stake_credentials",
        &left.script_stake_credentials,
        &right.script_stake_credentials,
    );

    // ── MIR pending ──────────────────────────────────────────────────
    diff_map(
        &mut diffs,
        "pending_mir_reserves",
        &left.pending_mir_reserves,
        &right.pending_mir_reserves,
    );
    diff_map(
        &mut diffs,
        "pending_mir_treasury",
        &left.pending_mir_treasury,
        &right.pending_mir_treasury,
    );
    cmp_eq(
        &mut diffs,
        "pending_mir_delta_reserves",
        &left.pending_mir_delta_reserves,
        &right.pending_mir_delta_reserves,
    );
    cmp_eq(
        &mut diffs,
        "pending_mir_delta_treasury",
        &left.pending_mir_delta_treasury,
        &right.pending_mir_delta_treasury,
    );

    // ── Epoch snapshots (mark/set/go) ────────────────────────────────
    cmp_pretty(&mut diffs, "snapshots", &left.snapshots, &right.snapshots);

    // ── Per-pool block counts ────────────────────────────────────────
    diff_map(
        &mut diffs,
        "epoch_blocks_by_pool",
        &left.epoch_blocks_by_pool,
        &right.epoch_blocks_by_pool,
    );
    cmp_eq(
        &mut diffs,
        "epoch_block_count",
        &left.epoch_block_count,
        &right.epoch_block_count,
    );

    // ── Praos nonces ─────────────────────────────────────────────────
    cmp_eq(
        &mut diffs,
        "evolving_nonce",
        &left.evolving_nonce,
        &right.evolving_nonce,
    );
    cmp_eq(
        &mut diffs,
        "candidate_nonce",
        &left.candidate_nonce,
        &right.candidate_nonce,
    );
    cmp_eq(
        &mut diffs,
        "epoch_nonce",
        &left.epoch_nonce,
        &right.epoch_nonce,
    );
    cmp_eq(&mut diffs, "lab_nonce", &left.lab_nonce, &right.lab_nonce);
    cmp_eq(
        &mut diffs,
        "last_epoch_block_nonce",
        &left.last_epoch_block_nonce,
        &right.last_epoch_block_nonce,
    );
    cmp_eq(
        &mut diffs,
        "randomness_stabilisation_window",
        &left.randomness_stabilisation_window,
        &right.randomness_stabilisation_window,
    );
    cmp_eq(
        &mut diffs,
        "stability_window_3kf",
        &left.stability_window_3kf,
        &right.stability_window_3kf,
    );
    cmp_eq(
        &mut diffs,
        "genesis_hash",
        &left.genesis_hash,
        &right.genesis_hash,
    );

    // ── Pre-Conway update mechanism ──────────────────────────────────
    diff_btree(
        &mut diffs,
        "pending_pp_updates",
        &left.pending_pp_updates,
        &right.pending_pp_updates,
    );
    diff_btree(
        &mut diffs,
        "future_pp_updates",
        &left.future_pp_updates,
        &right.future_pp_updates,
    );
    cmp_eq(
        &mut diffs,
        "update_quorum",
        &left.update_quorum,
        &right.update_quorum,
    );

    // ── Conway governance ────────────────────────────────────────────
    cmp_pretty(
        &mut diffs,
        "governance",
        &*left.governance,
        &*right.governance,
    );

    // ── Stake distribution + pointer stake ──────────────────────────
    diff_map(
        &mut diffs,
        "stake_distribution.stake_map",
        &left.stake_distribution.stake_map,
        &right.stake_distribution.stake_map,
    );
    diff_map(&mut diffs, "ptr_stake", &left.ptr_stake, &right.ptr_stake);

    // ── Reward update + opcert counters ─────────────────────────────
    cmp_pretty(
        &mut diffs,
        "pending_reward_update",
        &left.pending_reward_update,
        &right.pending_reward_update,
    );
    diff_map(
        &mut diffs,
        "opcert_counters",
        &left.opcert_counters,
        &right.opcert_counters,
    );

    DiffReport { diffs }
}

// ── Comparison helpers ─────────────────────────────────────────────────

fn cmp_eq<T: PartialEq + std::fmt::Debug>(diffs: &mut Vec<Diff>, field: &str, l: &T, r: &T) {
    if l != r {
        diffs.push(Diff {
            field: field.into(),
            detail: format!("{l:?} != {r:?}"),
        });
    }
}

/// Same as `cmp_eq` but truncates Debug output for large values.
fn cmp_pretty<T: PartialEq + std::fmt::Debug>(diffs: &mut Vec<Diff>, field: &str, l: &T, r: &T) {
    if l != r {
        let ld = format!("{l:?}");
        let rd = format!("{r:?}");
        let max = 160;
        let l_short = if ld.len() > max {
            format!("{}…(truncated, {} bytes)", &ld[..max], ld.len())
        } else {
            ld
        };
        let r_short = if rd.len() > max {
            format!("{}…(truncated, {} bytes)", &rd[..max], rd.len())
        } else {
            rd
        };
        diffs.push(Diff {
            field: field.into(),
            detail: format!("differ — left={l_short} right={r_short}"),
        });
    }
}

fn diff_map<K, V>(diffs: &mut Vec<Diff>, field: &str, l: &HashMap<K, V>, r: &HashMap<K, V>)
where
    K: Eq + Hash + std::fmt::Debug,
    V: PartialEq + std::fmt::Debug,
{
    let mut detail_parts = Vec::new();
    if l.len() != r.len() {
        detail_parts.push(format!("len {} vs {}", l.len(), r.len()));
    }
    let mut missing_right = 0usize;
    let mut missing_left = 0usize;
    let mut value_mismatches = 0usize;
    for (k, v) in l {
        match r.get(k) {
            None => missing_right += 1,
            Some(rv) if rv != v => value_mismatches += 1,
            _ => {}
        }
    }
    for k in r.keys() {
        if !l.contains_key(k) {
            missing_left += 1;
        }
    }
    if missing_right > 0 {
        detail_parts.push(format!("missing_in_right={missing_right}"));
    }
    if missing_left > 0 {
        detail_parts.push(format!("missing_in_left={missing_left}"));
    }
    if value_mismatches > 0 {
        detail_parts.push(format!("value_mismatches={value_mismatches}"));
    }
    if !detail_parts.is_empty() {
        diffs.push(Diff {
            field: field.into(),
            detail: detail_parts.join(", "),
        });
    }
}

fn diff_btree<K, V>(diffs: &mut Vec<Diff>, field: &str, l: &BTreeMap<K, V>, r: &BTreeMap<K, V>)
where
    K: Ord + std::fmt::Debug,
    V: PartialEq + std::fmt::Debug,
{
    let mut detail_parts = Vec::new();
    if l.len() != r.len() {
        detail_parts.push(format!("len {} vs {}", l.len(), r.len()));
    }
    let l_keys: BTreeSet<_> = l.keys().collect();
    let r_keys: BTreeSet<_> = r.keys().collect();
    let missing_right: Vec<_> = l_keys.difference(&r_keys).collect();
    let missing_left: Vec<_> = r_keys.difference(&l_keys).collect();
    let value_mismatches = l_keys
        .intersection(&r_keys)
        .filter(|k| l.get(*k) != r.get(*k))
        .count();
    if !missing_right.is_empty() {
        detail_parts.push(format!("missing_in_right={}", missing_right.len()));
    }
    if !missing_left.is_empty() {
        detail_parts.push(format!("missing_in_left={}", missing_left.len()));
    }
    if value_mismatches > 0 {
        detail_parts.push(format!("value_mismatches={value_mismatches}"));
    }
    if !detail_parts.is_empty() {
        diffs.push(Diff {
            field: field.into(),
            detail: detail_parts.join(", "),
        });
    }
}

fn diff_set<T: Eq + Hash + std::fmt::Debug>(
    diffs: &mut Vec<Diff>,
    field: &str,
    l: &HashSet<T>,
    r: &HashSet<T>,
) {
    if l.len() != r.len() {
        diffs.push(Diff {
            field: field.into(),
            detail: format!("set size {} vs {}", l.len(), r.len()),
        });
        return;
    }
    let missing_right = l.iter().filter(|x| !r.contains(x)).count();
    let missing_left = r.iter().filter(|x| !l.contains(x)).count();
    if missing_right + missing_left > 0 {
        diffs.push(Diff {
            field: field.into(),
            detail: format!("missing_in_right={missing_right} missing_in_left={missing_left}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::protocol_params::ProtocolParameters;
    use dugite_primitives::value::Lovelace;

    fn fresh_state() -> LedgerState {
        LedgerState::new(ProtocolParameters::mainnet_defaults())
    }

    /// Identity diff: a snapshot vs itself yields zero differences.
    #[test]
    fn diff_identical_states_is_empty() {
        let state = fresh_state();
        let view = LedgerStateSnapshot::from(&state);
        let report = diff_snapshots(&view, &view);
        assert!(
            report.is_empty(),
            "self-diff produced {} differences: {:?}",
            report.diffs.len(),
            report.diffs
        );
    }

    /// Cloning produces an identical snapshot — must diff to empty.
    #[test]
    fn diff_clone_is_empty() {
        let state = fresh_state();
        let v1 = LedgerStateSnapshot::from(&state);
        let v2 = v1.clone();
        let report = diff_snapshots(&v1, &v2);
        assert!(
            report.is_empty(),
            "clone diff non-empty: {:?}",
            report.diffs
        );
    }

    /// Mutating treasury produces exactly one diff entry on `treasury`.
    #[test]
    fn diff_treasury_mismatch_detected() {
        let state = fresh_state();
        let mut v1 = LedgerStateSnapshot::from(&state);
        let v2 = v1.clone();
        v1.treasury = Lovelace(123456789);
        let report = diff_snapshots(&v1, &v2);
        assert_eq!(
            report.diffs.len(),
            1,
            "expected 1 diff, got {:?}",
            report.diffs
        );
        assert_eq!(report.diffs[0].field, "treasury");
    }

    /// HashMap order-insensitivity: same key/value contents in different
    /// insertion orders MUST diff to empty. Without this guarantee the
    /// harness would produce false positives — see module doc.
    #[test]
    fn diff_hashmap_is_order_insensitive() {
        use dugite_primitives::hash::{Hash, Hash32};
        use std::sync::Arc;

        let mut state_a = fresh_state();
        let mut state_b = fresh_state();

        let mut accounts_a: HashMap<Hash32, Lovelace> = HashMap::new();
        accounts_a.insert(Hash::<32>::from_bytes([1u8; 32]), Lovelace(100));
        accounts_a.insert(Hash::<32>::from_bytes([2u8; 32]), Lovelace(200));
        accounts_a.insert(Hash::<32>::from_bytes([3u8; 32]), Lovelace(300));

        let mut accounts_b: HashMap<Hash32, Lovelace> = HashMap::new();
        // Different insertion order, same contents
        accounts_b.insert(Hash::<32>::from_bytes([3u8; 32]), Lovelace(300));
        accounts_b.insert(Hash::<32>::from_bytes([1u8; 32]), Lovelace(100));
        accounts_b.insert(Hash::<32>::from_bytes([2u8; 32]), Lovelace(200));

        state_a.certs.reward_accounts = Arc::new(accounts_a);
        state_b.certs.reward_accounts = Arc::new(accounts_b);

        let va = LedgerStateSnapshot::from(&state_a);
        let vb = LedgerStateSnapshot::from(&state_b);

        let report = diff_snapshots(&va, &vb);
        assert!(
            report.is_empty(),
            "hashmap with same content but different insertion order diffed: {:?}",
            report.diffs
        );
    }

    /// Differing values for the same key produce a `value_mismatches`
    /// entry rather than missing-key entries.
    #[test]
    fn diff_hashmap_value_mismatch() {
        use dugite_primitives::hash::{Hash, Hash32};
        use std::sync::Arc;

        let mut state_a = fresh_state();
        let mut state_b = fresh_state();

        let mut a: HashMap<Hash32, Lovelace> = HashMap::new();
        a.insert(Hash::<32>::from_bytes([1u8; 32]), Lovelace(100));
        let mut b: HashMap<Hash32, Lovelace> = HashMap::new();
        b.insert(Hash::<32>::from_bytes([1u8; 32]), Lovelace(999));

        state_a.certs.reward_accounts = Arc::new(a);
        state_b.certs.reward_accounts = Arc::new(b);

        let va = LedgerStateSnapshot::from(&state_a);
        let vb = LedgerStateSnapshot::from(&state_b);
        let report = diff_snapshots(&va, &vb);

        assert_eq!(report.diffs.len(), 1);
        assert_eq!(report.diffs[0].field, "reward_accounts");
        assert!(
            report.diffs[0].detail.contains("value_mismatches=1"),
            "expected value_mismatches=1 in detail, got: {}",
            report.diffs[0].detail
        );
    }

    /// Missing keys on either side are counted in the right direction.
    #[test]
    fn diff_hashmap_missing_key_directions() {
        use dugite_primitives::hash::{Hash, Hash32};
        use std::sync::Arc;

        let mut state_a = fresh_state();
        let mut state_b = fresh_state();

        let mut a: HashMap<Hash32, Lovelace> = HashMap::new();
        a.insert(Hash::<32>::from_bytes([1u8; 32]), Lovelace(1));
        a.insert(Hash::<32>::from_bytes([2u8; 32]), Lovelace(2));
        let mut b: HashMap<Hash32, Lovelace> = HashMap::new();
        b.insert(Hash::<32>::from_bytes([2u8; 32]), Lovelace(2));
        b.insert(Hash::<32>::from_bytes([3u8; 32]), Lovelace(3));

        state_a.certs.reward_accounts = Arc::new(a);
        state_b.certs.reward_accounts = Arc::new(b);

        let va = LedgerStateSnapshot::from(&state_a);
        let vb = LedgerStateSnapshot::from(&state_b);
        let report = diff_snapshots(&va, &vb);

        assert_eq!(report.diffs.len(), 1);
        let detail = &report.diffs[0].detail;
        assert!(
            detail.contains("missing_in_right=1") && detail.contains("missing_in_left=1"),
            "detail should report both directions, got: {detail}"
        );
    }

    /// Two completely independent states are not equal — sanity check
    /// that the harness doesn't accidentally treat everything as equal.
    /// Currently a fresh `LedgerState::new` produces zero meaningful
    /// state so we test by mutating multiple fields and expecting all
    /// to be reported.
    #[test]
    fn diff_multiple_field_mismatches() {
        let state = fresh_state();
        let mut v1 = LedgerStateSnapshot::from(&state);
        let mut v2 = v1.clone();
        v1.treasury = Lovelace(1);
        v2.treasury = Lovelace(2);
        v1.reserves = Lovelace(10);
        v2.reserves = Lovelace(20);
        v1.epoch_fees = Lovelace(100);
        v2.epoch_fees = Lovelace(200);
        let report = diff_snapshots(&v1, &v2);
        let fields: Vec<&str> = report.diffs.iter().map(|d| d.field.as_str()).collect();
        assert!(fields.contains(&"treasury"));
        assert!(fields.contains(&"reserves"));
        assert!(fields.contains(&"epoch_fees"));
    }
}
