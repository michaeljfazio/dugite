---
name: project_1074_first_pulse_prefilter_hole
description: "#1074 ROOT CAUSE (2026-08-10): mainnet 233->236 treasury-high/reserves-low divergence = the RUPD member fold's FIRST pulse runs BEFORE the rupd_addrs_rew capture on the trigger block, with a permissive is_none_or default — one deregistered credential in the queue head, numerically proven (70,698 / 163,916 exact)."
metadata:
  type: project
---

Mainnet treasury/reserves divergence at boundaries 233->234, 234->235, 235->236
(70,698 / 163,916 / 62,277 lovelace, treasury HIGH, reserves LOW by same;
#1074, evidence in `docs/superpowers/specs/2026-08-09-mainnet-treasury-divergence.md`
and `reports/mainnet-exactness/`).

**Root cause — an ORDERING hole on the pulser trigger block**
(`crates/dugite-ledger/src/state/apply.rs`, checked @ branch worktree-nonmyopic-1067):

1. The 4k/f trigger block sets `rupd_monetary` (~line 585), then
2. `pulse_rupd_member_fold` runs (~line 610) — taking the FIRST pulse — and its
   predicate is `addrs.as_ref().is_none_or(|set| set.contains(c))`
   (`rewards.rs` ~line 213): with `rupd_addrs_rew` still `None`, EVERY
   credential passes the pv<=6 member prefilter, and
3. only THEN is `rupd_addrs_rew` captured (~line 617-629).

So the first `pulse_size = ceil(N/8640)` credentials (ascending Hash32 =
lex order of the 28-byte hash; `to_hash32_padded` is front-copy/zero-suffix)
of EVERY pv<=6 epoch are folded WITHOUT the fvAddrsRew prefilter. Upstream
(`rewardOnePoolMember`, Rewards.hs:315) never creates those rewards.

**The proof**: credential `keyHash-000327e8689d04c56de4cd65a3cc104bf0cf6abfd6cac922a84bf2e9`
sits at queue index 3 (pulse_size 10/10/11), deregistered before epoch 233's
mark, GO stake 211,098,212 -> 211,549,805 delegated to pool `e8e00995c7f3820f…`.
Exact member-reward recomputation from the cstreamer dumps:
70,698 (EXACT), 163,916 (EXACT), 62,209 (obs 62,277; +68 = second-order floor
dust from the already-diverged reserves feeding deltaR1/deltaT1/R — same origin
as the -1 at 236->237). At apply the cred is unregistered -> dugite routes the
phantom reward to TREASURY; Haskell never created it -> stays in deltaR2 ->
RESERVES. Stops at 236->237 because the cred left GO (dereg during 233 =>
absent from the 234-start snapshot).

**Why every gate missed it**: pv>=7 bypasses the prefilter
(`hardforkBabbageForgoRewardPrefilter`), so permissive == correct on
devnet/preview/preprod — only a pv<=6 (mainnet 208-365) replay can see it.
And the differential proptest (`reward_pulser.rs::fold_differential`) runs with
`registered: |_| true` and `pv_major: 11` — the exact blind spot.

**Fix direction**: capture `rupd_addrs_rew` BEFORE calling
`pulse_rupd_member_fold` on the trigger block (both already run pre-cert, so
the captured set is unchanged by reordering). Secondary: the pulse-path
`build_pool_reward_table` also uses the permissive closure for the LEADER gate
— currently harmless (the boundary rebuilds the table and reads leaders only
from the fresh one) but wrong if the pulse table is ever consumed (Pulsing wire
arm / rewLeaders). Extend the differential gate to pv<=6 with a non-trivial
registered set spanning the capture boundary.

**Refuted premise**: "unregistered credentials holding MULTIPLE rewards" was
NOT the mechanism — the offender holds ONE member reward; multi-reward
credentials existed (registered) from epoch 212 with extras up to ~2.5e11/epoch
(223-227) and were handled byte-exactly, which also PROVES dugite's pv<=2
min-collapse and Leader<Member ordering in production. dugite's filter-at-compute
vs upstream's filter-at-apply is observationally EQUIVALENT (see
[[shelley-filter-rewards-apply-rupd-verbatim]]); `undistributed` == deltaR2.
