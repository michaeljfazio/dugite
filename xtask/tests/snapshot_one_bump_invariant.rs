//! Guards the "operators replay exactly once" plan for SNAPSHOT_VERSION 38.
//!
//! # The seam this watches
//!
//! v2.8.0 (#1067: `NonMyopic` per-pool `Likelihood` + `rewardPotNM`) bumped
//! `SNAPSHOT_VERSION` 37 -> 38 and was committed, gate-validated, and then
//! deliberately NOT tagged. The pulser-alignment program
//! (`docs/superpowers/specs/2026-08-08-pulser-alignment-design.md`) adds more
//! persisted ledger state — the frozen `RewardSnapShot` and the
//! `PulsingRewUpdate` state machine — and rather than bump 38 -> 39 and make
//! operators replay a second time, it EXTENDS the layout of 38 in place.
//!
//! That is only sound while **no released artefact carries SNAPSHOT 38 with the
//! #1067-only layout**. If v2.8.0 is ever tagged before the pulser fields land,
//! two different on-disk layouts both call themselves 38, and a node that
//! upgrades between them mis-decodes rather than being rejected.
//!
//! # Why this is a test and not a note in a document
//!
//! Revision 1 of the spec treated "v2.8.0 is unreleased" as a checkable
//! precondition. It is not: it is a standing obligation on every future
//! session, and CLAUDE.md's own hard requirements ("Commit regularly — Push
//! changes to remote after each successful iteration") actively push against
//! it. An invariant that depends on nobody doing the normal thing is exactly
//! the class of implicit assumption the pulser spec exists to eliminate — so
//! it would be incoherent to protect it with another one.
//!
//! # What to do when this fails
//!
//! Two honest options, and re-fitting the test is neither:
//!
//! 1. The pulser work has landed — add its marker field to
//!    `PULSER_LAYOUT_MARKERS` below. That is the expected resolution.
//! 2. v2.8.0 genuinely shipped first — then the one-bump plan is void. Bump
//!    `SNAPSHOT_VERSION` to 39 and delete this test, because the seam it
//!    watches no longer exists.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fields whose presence in `LedgerStateSnapshot` means the pulser layout has
/// landed. Any one is sufficient — they arrive together, and requiring all of
/// them would make this test fail spuriously mid-program.
const PULSER_LAYOUT_MARKERS: &[&str] = &["pulsing_reward_update", "reward_snapshot"];

fn repo_root() -> PathBuf {
    // xtask/tests/ -> xtask/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent")
        .to_path_buf()
}

/// Tags matching the release this invariant is about.
///
/// Returns an empty vec when git is unavailable or this is not a checkout —
/// absence of evidence is not evidence of a tag, and a packaging build with no
/// `.git` must not fail here.
fn v28_tags(root: &Path) -> Vec<String> {
    let out = match Command::new("git")
        .args(["tag", "--list", "v2.8*"])
        .current_dir(root)
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn snapshot_format_src(root: &Path) -> String {
    let p = root.join("crates/dugite-ledger/src/state/snapshot_format.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn snapshot_version(root: &Path) -> u8 {
    let p = root.join("crates/dugite-ledger/src/state/snapshot.rs");
    let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let line = src
        .lines()
        .find(|l| l.contains("const SNAPSHOT_VERSION"))
        .expect("SNAPSHOT_VERSION declaration");
    line.rsplit('=')
        .next()
        .and_then(|s| s.trim().trim_end_matches(';').parse().ok())
        .unwrap_or_else(|| panic!("could not parse SNAPSHOT_VERSION from: {line}"))
}

#[test]
fn snapshot_38_is_not_shared_between_two_layouts() {
    let root = repo_root();
    let version = snapshot_version(&root);

    // The invariant is specific to 38. Once the version moves on, the two
    // layouts can no longer collide and this guard is inert by construction.
    if version != 38 {
        return;
    }

    let tags = v28_tags(&root);
    if tags.is_empty() {
        return; // nothing released under this version — the plan holds
    }

    let src = snapshot_format_src(&root);
    let landed = PULSER_LAYOUT_MARKERS.iter().any(|m| src.contains(m));

    assert!(
        landed,
        "SNAPSHOT_VERSION is 38 and {tags:?} exists, but `LedgerStateSnapshot` \
         carries none of {PULSER_LAYOUT_MARKERS:?}.\n\n\
         Two different on-disk layouts would both call themselves 38: the one \
         that tag shipped (#1067 only) and the one this tree builds (with the \
         pulser state). A node upgrading between them MIS-DECODES rather than \
         being rejected, because the version check passes.\n\n\
         Fix by bumping SNAPSHOT_VERSION to 39 and deleting this test — the \
         one-bump plan is void once v2.8.0 is tagged. Do NOT relax this \
         assertion; the seam is real."
    );
}

/// The markers must stay in sync with the field names the program actually
/// introduces, or the guard above silently never fires.
#[test]
fn pulser_layout_markers_are_not_empty() {
    assert!(
        !PULSER_LAYOUT_MARKERS.is_empty(),
        "emptying PULSER_LAYOUT_MARKERS disables the 38-collision guard entirely"
    );
}
