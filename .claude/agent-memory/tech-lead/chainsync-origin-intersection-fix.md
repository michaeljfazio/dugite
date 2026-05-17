---
name: chainsync-origin-intersection-fix
description: Origin-intersection bug — disconnect when intersection=Origin but local ledger is non-Origin; VolatileDB switch_chain cannot operate across Origin anchor
type: project
---

## Bug: Stale ChainSync Intersection at Origin (Bug A)

When dugite starts with non-Origin ledger tip but connects to a peer that is behind it (peer also at genesis or behind), `known_points = [Origin]` so intersection lands at Origin. VolatileDB `switch_chain` requires a shared volatile block (`isReachable` invariant); Origin is not a volatile block, so every competing-fork block returns `StoreButDontChange` — chain selection is permanently blocked for that connection.

**Why:** `use_chain_tip = chain_slot > ledger_slot` (both 0) → false; `chain_diverged = false`; goes to `else` branch: ledger_tip=Origin not pushed; chain_points empty; only `[Origin]` in known_points.

**Fix location:** `crates/dugite-node/src/node/sync.rs`, after `try_find_intersect` returns `Some(CodecPoint::Origin)`. Check: `if matches!(intersection, Some(CodecPoint::Origin)) && ledger_tip != Point::Origin { return Err(...); }`. This is ~5 lines at the call site, no signature changes.

**Why:** Triggers reconnect → chainsync_client_task reruns with fresh known_points once the relay has advanced. Matches Haskell `terminateAfterDrain / NoLongerIntersects` semantics.

**Non-issue cases:** Both at genesis (guard doesn't fire, ledger_tip==Origin). Mithril import (guard fires correctly — peer should reconnect and offer a real intersection point).

**Design doc:** `docs/superpowers/specs/2026-05-16-bug-a-stale-intersection-fix.md`

**How to apply:** When investigating "node stuck on self-forged fork" or "chain selection not switching" in local-testnet or low-peer environments.
