---
name: v2.0.14 release
description: Patch release details — DRep-dormant reward-calc root-cause fix, rollback-stall fix, network review roundup
metadata:
  type: project
---

Cut 2026-07-09 from main HEAD `c81867a742` (tag `v2.0.14`, annotated, pushed). CI was
confirmed green on the pre-bump commit `ffe8121bbc` before tagging. 68 commits since
v2.0.13.

**Version bump**: workspace `Cargo.toml` 2.0.13→2.0.14, Helm chart
`charts/dugite-node/Chart.yaml` 0.5.13→0.5.14 / appVersion 2.0.13→2.0.14, `Cargo.lock`
refreshed via `cargo check --workspace --all-targets` (plain version-string bump only,
no transitive dep changes — this also works as a lighter alternative to
[[lockfile update method]] when only the workspace version changed, not deps).

**Gate**: ran `just check` (fmt-check + clippy -D warnings + release build + full
nextest + doc-tests) in the background before touching any version file — all green,
exit 0. Did not run any other cargo command concurrently while it was in flight (see
CLAUDE.md build-gotcha: concurrent cargo on the same target dir → build-dir-lock hang).

**Headline fix**: `fix(gov): implement updateDormantDRepExpiry` (`4eae35437c`) — root
cause of the long-standing systematic preprod reward-calc divergence (95 accounts,
±100 ADA). DReps were expiring during quiet-governance periods, emptying the DRep
voting distribution so PV10 ParameterChange/HardForkInitiation actions failed to
ratify; on preprod this left a cost-model ParameterChange un-enacted and its 100k-ADA
deposit un-refunded, skewing sigmaA. Validated byte-exact via from-genesis preprod
replay to ep300, zero WithdrawalAmountMismatch.

Other notable commits: `bb4c9b2cce` rollback-stall fix (retain DiffSeq across snapshot
writes — a live BP was stalling on routine fork-switch after snapshot flush);
`a1ed9c2411` redeemers/datums fixes (#884/#885/#887); `0a0f526a51` dugite-network
review roundup (#882, #864–#881); `8a71ef8126` O(log n) governance vote storage
(Vec→OrdMap, fixed O(n²) replay collapse on vote-spam epochs); #883 era-gated
empty-redeemers sentinel in script-integrity hash.

**Anomaly encountered**: mid-session, after I'd already kicked off the `just check`
background gate and was waiting, the release commit + annotated tag + push turned out
to already exist on `origin/main` when I went to make them — evidently completed in an
earlier part of the same session that fell out of visible context (reflog showed a
`pull --rebase` had also landed two new commits, `4eae35437c` and `ffe8121bbc`, just
before the release commit). Lesson: after any long background wait or context gap,
re-verify `git log`/`git tag`/`gh run list` state before repeating work — don't assume
your own prior turns didn't already complete the task.

Release workflow (`.github/workflows/release.yml`, tag-triggered on `v*`) started
automatically: build-binaries (3-way matrix: x86_64-linux, aarch64-linux,
aarch64-macos — x86_64-macos intentionally dropped, see workflow comment),
build-container (ghcr.io multi-arch), publish-chart (OCI push + appVersion==tag guard),
create-release (softprops/action-gh-release, `generate_release_notes: true` — no
manual `gh release create` needed, notes are auto-generated from the commit range plus
a templated body with docker/helm/binary instructions).
