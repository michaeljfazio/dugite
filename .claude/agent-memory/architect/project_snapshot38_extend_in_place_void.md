---
name: snapshot38-extend-in-place-void
description: SNAPSHOT_VERSION 38's extend-in-place plan died when v2.8.0 was tagged; its guard is structurally vacuous in CI because checkout fetches no tags.
metadata:
  type: project
---

`SNAPSHOT_VERSION` 38 was deliberately **extended in place** rather than bumped,
so operators would replay once instead of twice. That plan was valid only while
no released artefact carried SNAPSHOT 38. **v2.8.0 is now tagged, so the plan is
void**: any further layout change MUST bump 38 -> 39.

`xtask/tests/snapshot_one_bump_invariant.rs::snapshot_38_is_not_shared_between_two_layouts`
enforces this and **fails on main as of 2026-08-20** — verified by running it,
not inferred:

```
SNAPSHOT_VERSION is 38 and ["v2.8.0"] exists, but `LedgerStateSnapshot`
carries none of ["pulsing_reward_update", "reward_snapshot"].
Fix by bumping SNAPSHOT_VERSION to 39 and deleting this test.
```

Its own remedy is the fix: bump to 39, delete the test. Do NOT relax the
assertion — two on-disk layouts both calling themselves 38 means an upgrading
node **mis-decodes instead of being rejected**, because the version check passes.

**Why nobody noticed — the guard is vacuous in CI.** It reads tags via
`git tag --list v2.8*` (test line ~59). Every `.github/workflows/ci.yml` job uses
`actions/checkout@v7` with no `fetch-depth`/`fetch-tags`, so the default shallow
clone carries **no tags**; the list is empty, the test returns early, and CI has
been green on this guard since the day it was written. It only bites on a
developer machine that has the tags. A tag-dependent guard under a shallow
checkout tests nothing — see [[a-field-with-no-writer-compares-vacuously]].

**Why:** discovered while sequencing #1071/#1084/#1088, all three of which want a
format change; the bump question had to be settled before any of them started.

**How to apply:** treat "bump 38 -> 39 once, at the end of the wave, in its own
commit" as mandatory, not a judgement call. Bundle every format-affecting change
(#1088 ordering, #1071 pulser fields, #1084 Byron state) behind that single bump.
Also fix the CI blindness — either fetch tags in the workflow or derive the
release fact from something a shallow clone can see. Related:
[[1088-snapshot-ordering-sort-at-boundary]].
