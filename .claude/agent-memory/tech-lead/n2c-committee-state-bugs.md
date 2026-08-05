---
name: committee-state-encoding-bugs
description: STALE as of 2026-08-05 — bugs 1-3 below are all FIXED (see issue-1018-1027-lsq-mempool-audit-2026-08-05.md for the current, real gap: NextEpochChange hardcoded)
type: project
---

**STALE — verify before use.** All three bugs below were confirmed FIXED by
direct code read + a passing regression test
(`test_committee_state_script_hot_credential_type_propagated`,
`crates/dugite-node/src/node/n2c_query/governance.rs`) during the 2026-08-05
LSQ audit. `committee_expiration` IS the outer iteration map, hot credential
type IS tracked via `script_committee_hot_credentials`, and the wire encoder
IS field-correct. The REAL remaining gap in this query as of 2026-08-05 is
different: `CommitteeMemberState`'s `NextEpochChange` field is hardcoded to
`NoChangeExpected` for every member — see
[issue-1018-1027-lsq-mempool-audit-2026-08-05.md](issue-1018-1027-lsq-mempool-audit-2026-08-05.md)
(filed as #1020). Kept below for historical context only.

After the e8b58c9 fix (Hash32-padded credential hashes truncated to 28 bytes), the following issues remain in governance query responses:

## 1. CommitteeSnapshot members built from wrong map

**Why:** `update_query_state()` builds `CommitteeSnapshot.members` by iterating `committee_hot_keys` only. But `committee_expiration` is the authoritative membership map — it contains ALL committee members regardless of whether they have authorized a hot key.

**Effect:** Members who haven't run `CommitteeHotAuth` are invisible in `query committee-state` responses. cardano-cli transaction build may fail if it expects specific CC quorum.

**Location:** `crates/dugite-node/src/node/query.rs`, lines 277-313

**How to apply:** Should iterate `committee_expiration` as the outer loop, then look up hot_keys and resigned status per member. Members absent from hot_keys and resigned should have hot_status=1 (MemberNotAuthorized).

## 2. Hot credential type hardcoded to KeyHashObj (0)

**Why:** `CommitteeMemberSnapshot` struct has no `hot_credential_type` field. The encoding at `encoding.rs:1055` always encodes `enc.u8(0)` (KeyHashObj) regardless of the actual hot credential type.

**Effect:** Script hot credentials appear as key-hash type — cardano-cli may reject this if it validates the credential type.

**Location:** `crates/dugite-network/src/n2c/query/encoding.rs`, line 1055
Fix: add `hot_credential_type: u8` to `CommitteeMemberSnapshot` and populate it from the certificate data.

## 3. committee_hot_keys stores hot credential as Hash32 but doesn't track hot cred type

**Why:** `committee_hot_keys: HashMap<Hash32, Hash32>` only stores the hash — it loses whether the hot credential is key or script. The `script_committee_credentials` set only tracks cold credentials, not hot.

**Effect:** Combined with Bug 2 — we can never correctly encode the hot credential type.

**Location:** `crates/dugite-ledger/src/state/mod.rs`, line 204; `certificates.rs` CommitteeHotAuth handler.

## Notes on what IS fixed (e8b58c9)

- DRep credential_hash: hash32_padded_to_28_bytes ✓
- DRep delegator_hashes: hash32_padded_to_28_bytes ✓
- Committee cold_credential: hash32_padded_to_28_bytes ✓
- Committee hot_credential: hash32_padded_to_28_bytes ✓
- StakeAddress credential_hash: hash32_padded_to_28_bytes ✓
- VoteDelegatee credential_hash: hash32_padded_to_28_bytes ✓
- VoteDelegatee drep_hash (KeyHash): [..28] truncation ✓
- build_vote_maps committee/drep voters: use Credential(Hash28) directly ✓
- build_vote_maps spo voters: [..28] truncation ✓
