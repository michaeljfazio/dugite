---
name: Issue #433 CC member count investigation
description: Preview epoch 1269 shows 1 CC vs Koios 8 — re-investigation 2026-05-12
type: project
---

Status: NOT FIXED. Investigation incomplete pending live-node observation.

Ground truth (preview Koios):
- 8 CC members, all `cc_cold_has_script: true`. Genesis founder `ff9babf23...` expiration 1000.
- 3 enacted NewCommittee (CDDL UpdateCommittee) proposals: enacted epochs 994, 998, 1013.
- Epoch-1013 enactment carries `members_to_add={}` + 5 removals (cleanup). The 7 additions came from the 994 + 998 actions.

Code audit (clean):
- `enact_gov_action_impl` UpdateCommittee at governance.rs:2242-2296 is correctly additive. Script tracking added in 04de3c1fb.
- CBOR decode multi_era.rs:1804-1818 preserves credential type.
- Conway genesis seed eras/conway.rs:781-787 hydrates founder with script flag via `hash[28]=0x01`.

Key reframe: issue evidence shows `status=Active expiration=1000`. query.rs:444 emits Active only when `ls.epoch.0 <= expiry.0`. So the queried snapshot was at epoch ≤ 1000 — pre-enactment. The 7-member gap may be a sync-lag artifact, not an enactment bug.

Independent real divergence found: `expire_committee_members` (governance.rs:1485-1508) physically removes expired entries. Haskell keeps them and computes `MemberStatus=Expired` dynamically. This will manifest post-epoch-1000 when founder expires. Separate issue.

Why: Did not commit a fix because (a) the additive enactment path is already correct; (b) no synced node available to query at epoch ≥ 1013 to localize the layer (proposal admission vs ratification vs enactment).
How to apply: Next agent must reproduce on a node synced past epoch 1013 before assuming any bug location. Don't speculatively patch enact_gov_action_impl — it's correct.
