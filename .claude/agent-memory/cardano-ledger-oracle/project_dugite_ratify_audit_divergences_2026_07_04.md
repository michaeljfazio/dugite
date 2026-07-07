---
name: project-dugite-ratify-audit-divergences-2026-07-04
description: 3 concrete, verified divergences found in crates/dugite-ledger/src/state/governance.rs vs cardano-ledger while precision-auditing Conway RATIFY on 2026-07-04 — NOT yet fixed
metadata:
  type: project
---

While answering a 10-question precision audit of Conway governance ratification (user comparing dugite-ledger against IntersectMBO/cardano-ledger), I cross-checked the live-verified Haskell facts in [[conway-ratify-precision-facts]] against dugite's actual code in `crates/dugite-ledger/src/state/governance.rs` and found 3 real, currently-unfixed divergences. None were fixed as part of this audit — this was a research/audit task only.

## Divergence 1 — `pv_can_follow` accepts any minor > cur, not exactly cur+1

Two duplicated call sites: `governance.rs:92-93` and `governance.rs:413-414`:
```rust
let can_follow = (*tgt_major == cur_major + 1 && *tgt_minor == 0)
    || (*tgt_major == cur_major && *tgt_minor > cur_minor);
```
Haskell `pvCanFollow` (see [[conway-ratify-precision-facts]] #1) requires `newMinor == curMinor + 1` exactly. Dugite's `*tgt_minor > cur_minor` accepts e.g. current (10,0) -> proposed (10,5), which Haskell would reject with `ProposalCantFollow`. Fix: change to `*tgt_minor == cur_minor + 1`.

## Divergence 2 — HardFork chaining never consults the in-flight preceding proposal's own target version

**Concrete example CORRECTED 2026-07-06 — see [[hardfork-pvcanfollow-exact-mechanics]] for byte-exact verified mechanics; the original example below was wrong and has been replaced.**

Same two call sites: `cur_major`/`cur_minor` are read unconditionally from `self.epochs.protocol_params` (current on-chain PParams). There is no dugite equivalent of Haskell's `preceedingHardFork`, which looks up the prev_action_id'd in-flight HardForkInitiation proposal in the live Proposals set and chains `pvCanFollow` against THAT proposal's target version — but ONLY when the proposed major version does not already exceed `succVersion(current major)`; if it does, Haskell forces the comparison back to live current PParams anyway (a short-circuit that prevents compounding two major bumps in one epoch).

Corrected concrete divergence: proposal A (in-flight, not yet enacted), `prevGovActionId=root(SNothing)`, targets `(10,0)` from current `(9,0)` — valid (major-bump-from-root case). Proposal B chains `prevGovActionId=A`, targets `(10,1)` (a MINOR bump on top of A's still-unenacted target — NOT a further major bump, so Haskell's short-circuit does NOT fire since `10 > succVersion(9)=10` is false). Haskell resolves B's base via the chain lookup to A's target `(10,0)`, then `pvCanFollow (10,0) (10,1)` = True (minor+1 rule) → **Haskell ACCEPTS B**. Dugite instead compares B directly against live current PParams `(9,0)`: `pv_can_follow((9,0),(10,1))` is false under BOTH of dugite's disjuncts (major-bump branch needs `tgt_minor==0`, same-major branch needs `tgt_major==cur_major`) → **dugite INCORRECTLY REJECTS B**. (The previous write-up's example — B targeting a second major bump, (11,0) — was wrong: that scenario is rejected by Haskell too, via the same short-circuit, so it is NOT actually a divergence.) Fix: needs a lookup into the live proposals/OMap keyed by prev_action_id when the action is a HardForkInitiation whose prev doesn't match the enacted root AND the target major doesn't already exceed current-major+1.

## Divergence 3 — committee zero-threshold auto-approve short-circuits BEFORE the minSize gate

`check_cc_approval`, `governance.rs:3358-3459`. The zero-threshold shortcut is at line 3379 (`if threshold.is_zero() { return true; }`), which executes before `active_size` is even computed (loop starts at 3397) and before the minSize gate at line 3437 (`if !bootstrap && active_size < committee_min_size { return false; }`). Per Haskell (see [[conway-ratify-precision-facts]] #7), the minSize-or-bootstrap gate determines whether a `VotingThreshold` is constructed AT ALL — the zero-threshold auto-pass inside `committeeAccepted` is only reachable after that gate already passed. So: a real committee configured with a literal 0% threshold, with `active_size < committee_min_size` and not in bootstrap, should be REJECTED (False) per Haskell, but dugite returns `true` (auto-approved) because the zero-check fires first. Existing test `test_cc_min_size_enforcement` (governance.rs:5111) does NOT catch this because its fixture (`gov_test_state`) always sets `committee_threshold = Some(1/2)` (non-zero) — the zero-threshold + insufficient-active-members combination is untested. Fix: move the minSize-or-bootstrap check before the `threshold.is_zero()` shortcut (or compute `active_size` first and gate on it before reading threshold at all).

## Why this matters
All 3 are silent correctness bugs (no panics, no test failures under current fixtures) that would only surface on-chain in specific governance-action combinations — hard-fork proposals with minor-version gaps or in-flight chaining, and low-membership/zero-threshold committee configurations. Byte-exact ledger-state divergence would show up in a future epoch-diff cross-validation only if these exact scenarios occur on preview/preprod/mainnet, which may be rare — worth a proactive fix rather than waiting for a live divergence report.

**How to apply:** If asked to fix Conway governance ratification bugs, or if a future epoch-diff shows a HardFork/committee-related divergence, check these three spots first before re-deriving from scratch.
