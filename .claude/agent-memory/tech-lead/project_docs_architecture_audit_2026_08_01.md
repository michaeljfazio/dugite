---
name: project-docs-architecture-audit-2026-08-01
description: Full audit + fix pass on docs/src/architecture/*.md (mdBook) against current crates/ code, ahead of the v2.4.3 release
metadata:
  type: project
---

Ran a full accuracy audit of the 8 architecture pages under `docs/src/architecture/` (overview,
sync-pipeline, storage, ledger, consensus, networking, p2p-governor, genesis-support) against
`crates/` as of the v2.4.3 cut (2026-08-01). All 8 pages were edited in place; nothing committed
(per instructions).

**Why**: the docs had accumulated significant drift, mostly in the P2P/networking layer, which
had clearly been substantially rewritten since these pages were last touched — governor tick
interval, peer target defaults, the entire `PeerCategory`/circuit-breaker/subnet-diversity
feature set (never existed or was removed), and the BlockFetch fetch architecture (docs described
a 4-peer concurrent pool; actual code is single-fetcher/GSV-preferred, see
[[reference_block_fetch_logic_dead_code_trap]]) were all wrong. See
[[reference_claude_md_architecture_staleness_2026_08_01]] for the specific stale facts also baked
into CLAUDE.md.

**How to apply**: if asked to review/update these docs again, don't assume the current content is
a reliable starting point for delta-checking — cross-reference every specific number (target
counts, intervals, buffer sizes) against the actual constant/default in code, not just the
narrative structure. `docs-review` skill's own `references/doc-map.md` "Key facts" section is
itself stale (references pallas/aiken removal, "14 vs 15 crates," v1.7.0 — all pre-v2.x); don't
trust it either without checking dates.

Storage page now documents the v2.4.0 durability work (#926-#929: per-append secondary-index
writes, dual-open-path reconciliation, `.chunk.orphaned` quarantine, `tip.meta` clamping,
`immutable/clean` marker, `<db>/lock` flock) that was previously entirely undocumented. Ledger
page now documents the #919 per-era `min_coin_for_output` dispatch. Genesis-support page now
documents the exact 3-branch `haa_satisfied()` case split (#933) and the #757 dugite-only
"absent marker + recent tip → Syncing" startup extension, neither of which were previously
explained beyond "HAA satisfied (enough BLPs)".
