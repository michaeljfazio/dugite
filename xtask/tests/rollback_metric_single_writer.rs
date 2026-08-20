//! Guards that the two rollback-family metrics each have exactly ONE
//! increment site (#1098).
//!
//! `dugite_rollback_count_total` used to conflate two different signals under
//! one name: per-peer ChainSync `MsgRollBackward` protocol chatter (routine,
//! ~15-20 per peer resync) and actual VolatileDB/ledger-level chain-switch
//! (reorg) events. A 3h preprod soak recorded 82 on the metric while only 2
//! real ledger reorgs occurred.
//!
//! The fix split it into `dugite_chainsync_rollback_messages_total` (the
//! per-peer protocol counter) and `dugite_ledger_reorg_total` (the real
//! reorg counter, incremented from exactly one place —
//! `Node::handle_rollback_inner`, which is reachable ONLY via
//! `Node::handle_ledger_rollback`, which in turn is reachable ONLY from a
//! genuine `TriggeredFork` chain switch or a failed-fork revert — never from
//! a peer's ChainSync message).
//!
//! While auditing the original metric's call sites, three of them turned out
//! to ALSO increment `rollback_count` directly, immediately before calling
//! `handle_ledger_rollback` (which increments it again internally via
//! `handle_rollback_inner`) — so every real reorg reaching those three sites
//! was counted TWICE. Simply renaming the metric without removing those
//! duplicate direct increments would have produced a new metric with the
//! same "misleading number" defect this issue is about, just at a smaller
//! scale. This test is the guard against that duplicate-increment pattern
//! recurring, at either field, in the future — the same "N-copies" shape
//! documented across this repo's other guard tests (#1082, #1088, #1057's
//! two-spelling-of-Origin fix).
//!
//! Prove it RED: reintroduce a
//! `self.metrics.ledger_reorg_total.fetch_add(1, ...)` immediately before any
//! `self.handle_ledger_rollback(...)` call (matching the pre-fix shape at
//! `node/mod.rs`'s `apply_fork_switch_plan` and forge-triggered-fork arm, or
//! `node/sync.rs`'s live-tip `TriggeredFork` arm) and this test fails with a
//! count > 1.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

/// Every `.rs` file under `crates/dugite-node/src/`, recursively.
fn dugite_node_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.join("crates/dugite-node/src")];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files
}

/// Strip `//`-style line comments (naive but sufficient: this codebase does
/// not put `.fetch_add(` calls inside string or char literals containing
/// `//`), then collapse all whitespace runs to a single space. This lets a
/// substring search find `<field>.fetch_add(` regardless of how the call is
/// wrapped across lines — the actual style used throughout this crate is
/// `self.metrics\n    .field_name\n    .fetch_add(...)`.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let code = match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        };
        out.push(' ');
        out.push_str(code.trim());
    }
    // Collapse repeated spaces so `.field .fetch_add(` and `.field\n.fetch_add(`
    // normalize identically, then strip spaces immediately around `.` so
    // `.field` and `.fetch_add(` glue into `.field.fetch_add(` regardless of
    // original formatting.
    let collapsed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.replace(" .", ".").replace(". ", ".")
}

/// Count non-overlapping occurrences of `.{field}.fetch_add(` across every
/// `.rs` file in `crates/dugite-node/src/`, returning `(total, per_file)`.
fn count_increment_sites(root: &Path, field: &str) -> (usize, Vec<(PathBuf, usize)>) {
    let pattern = format!(".{field}.fetch_add(");
    let mut total = 0;
    let mut per_file = Vec::new();
    for path in dugite_node_source_files(root) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let normalized = normalize(&text);
        let count = normalized.matches(&pattern).count();
        if count > 0 {
            per_file.push((path, count));
            total += count;
        }
    }
    (total, per_file)
}

#[test]
fn ledger_reorg_total_has_exactly_one_increment_site() {
    let root = repo_root();
    let (total, per_file) = count_increment_sites(&root, "ledger_reorg_total");
    assert_eq!(
        total,
        1,
        "dugite_ledger_reorg_total must be incremented from exactly ONE place \
         (Node::handle_rollback_inner) — every other real-reorg call site \
         (apply_fork_switch_plan, the forge-triggered-fork arm, the live-tip \
         TriggeredFork arm) reaches it only via handle_ledger_rollback, never \
         directly. Found {total} site(s):\n{}",
        per_file
            .iter()
            .map(|(p, c)| format!("  {} x{c}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn chainsync_rollback_messages_has_exactly_one_increment_site() {
    let root = repo_root();
    let (total, per_file) = count_increment_sites(&root, "chainsync_rollback_messages");
    assert_eq!(
        total,
        1,
        "dugite_chainsync_rollback_messages_total must be incremented from \
         exactly ONE place (the ChainSync MsgRollBackward handler in \
         node/sync.rs) — a second site would reintroduce the #1098 \
         conflation this metric split apart from dugite_ledger_reorg_total. \
         Found {total} site(s):\n{}",
        per_file
            .iter()
            .map(|(p, c)| format!("  {} x{c}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The old, conflated metric name must not be EXPORTED any more — i.e. it
/// must never appear as a Rust string literal (the form
/// `to_prometheus`'s counters table registers names in). A prose mention in
/// a doc comment explaining the #1098 rename (no surrounding quotes) is
/// fine and expected; this only guards against the name silently coming
/// back on the wire.
#[test]
fn old_conflated_metric_name_is_no_longer_exported() {
    let root = repo_root();
    let needle = "\"dugite_rollback_count_total\"";
    for path in dugite_node_source_files(&root) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            !text.contains(needle),
            "{} still registers the retired dugite_rollback_count_total \
             metric name as a string literal (#1098 split it into \
             dugite_chainsync_rollback_messages_total and \
             dugite_ledger_reorg_total)",
            path.display()
        );
    }
}
