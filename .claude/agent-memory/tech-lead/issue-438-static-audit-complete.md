---
name: Issue 438 static-audit phase complete
description: Both static suspects (withdrawal-rollback asymmetry, dual-RUPD double-credit) eliminated; remaining work is live-replay capture
type: project
---

Issue #438 (preview epoch 1268 leader reward +3505 lovelace overshoot) static-audit phase closed by commit 648d72484 on 2026-05-13.

**Suspect 1 (withdrawal-on-rollback restoration)** EXONERATED by `ledger_seq.rs` architecture: rollback is pure delta truncation (`self.deltas.truncate(new_len)` at line 613), no inverse-apply path exists. Comment at line 725 makes this explicit. Forward replay applies `RewardChange::Withdraw` uniformly for every accepted withdrawal delta. No asymmetric restoration possible.

**Suspect 2 (pending-RUPD + fresh-RUPD double-credit)** EXONERATED by source scan: `pending_reward_update` is **never written to `Some(_)`** anywhere in the crate. All era init sites (byron/shelley/alonzo/babbage/conway/dijkstra) set `None`. The Conway boundary at conway.rs:342 always takes `None`, making line 361's fresh RUPD the sole crediting path.

Why: How to apply: When investigating reward divergences in dugite, do NOT re-investigate these two theories — they are structurally eliminated. Pinned by two source-scanning tests in `state::rewards::tests::`:
- `test_issue_438_rollback_is_pure_delta_truncation_not_inverse_apply`
- `test_issue_438_no_writer_for_pending_reward_update`

Future patches that add `pending_reward_update = Some(...)` will fail-loud at test #2.

**Path forward**: live-replay capture (#471) — diff `reward_accounts[owner_cred]`, `delegations[owner_cred]`, `ss_stake[owner_cred]` per-block across preview epoch 1266 vs Haskell oracle. Memory note `issue-438-koios-stake-vs-ssstake.md` already identified stale `reward_accounts` balance carried into SET→GO rotation as the most likely cause.
