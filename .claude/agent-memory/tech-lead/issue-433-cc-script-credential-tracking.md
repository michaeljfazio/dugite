---
name: Issue #433 partial fix — script committee credential tracking
description: enact_gov_action_impl + genesis loader weren't populating script_committee_credentials, causing all on-chain script CC members to be reported as KeyHash by N2C committee-state query
type: project
---

Issue #433 reports 1 vs 8 CC members at preview epoch 1269. Investigation (2026-05-12, commit 04de3c1fb):

**Found and fixed (partial):**
- `enact_gov_action_impl::UpdateCommittee` only inserted into `committee_expiration`; never tracked script type in `script_committee_credentials`. Same gap on remove and in `NoConfidence`.
- Conway genesis loader (`main.rs`) also skipped seeding `script_committee_credentials` for `scriptHash-` prefixed genesis entries (byte 28 = 0x01 convention).
- Query path `node/query.rs:413-417` reads `script_committee_credentials.contains(cold)` for cold_credential_type → mislabels script members as KeyHash without these fixes.
- Added regression test `test_enact_update_committee_tracks_script_credentials`.

**Why:** N2C committee-state query semantics require accurate cold_credential_type. Without it cardano-cli reports wrong type for every script-typed CC member.

**How to apply:** Whenever adding to `committee_expiration` (any path), also maintain `script_committee_credentials` for script credentials. Whenever clearing committee state (NoConfidence, member removal), clear the script tracking too.

**Still open in #433:** the headline "missing 7 members" symptom. Preview's 7 added members came from on-chain UpdateCommittee proposals chained via prev_action_id. Need replay logs with `RUST_LOG=dugite_ledger::state::governance=debug` over epochs ~1011-1013 to bisect between: (a) prev_action_id chain mismatch, (b) bootstrap-phase gating, (c) ratification threshold failure.
