---
name: 1088-snapshot-ordering-sort-at-boundary
description: Snapshot map nondeterminism (#1088) is fixed by sorting at the serialization boundary, not by converting live containers to OrdMap.
metadata:
  type: project
---

The on-disk ledger snapshot is nondeterministic: `bincode` writes every
`HashMap`/`HashSet` in native iteration order, and both `std::HashMap` and
`imbl::HashMap` default to `RandomState`, so **two nodes with identical ledger
state write different bytes** whenever any map holds >=2 entries. About 40 fields
are affected across `LedgerStateSnapshot`, `GovernanceState`, `CertSubState`,
`StakeSnapshot` (x3 mark/set/go), `EpochSnapshots`, `NonMyopic`,
`PulsingSnapshot`, `EnactedGovTerms`, `PGraph`.

**Decision: sort at the serialization boundary (option a), NOT convert live
containers to `OrdMap` (option b).**

**Why:**
- Blast radius. `dugite-ledger` has ~133 `imbl::HashMap` uses against 4 `OrdMap`;
  option (b) reaches ~300+ call sites across `state/`, every `eras/` module, and
  parts of `dugite-node` (several live fields are read directly by N2C encoders).
  Option (a) is confined to `snapshot_format.rs` plus custom `Serialize` impls
  (or small wire mirrors) for the nested types that are `.clone()`d wholesale.
- Performance. The hottest maps document HAMT lookup/clone as load-bearing
  (`reward_accounts` is ~784K entries on mainnet). `OrdMap` keeps O(1) clone but
  is a real B-tree: depth ~log2(N)~=20 vs the HAMT's ~log32(N)~=4. Option (a)
  costs nothing per block — the conversion runs once per snapshot write, which is
  already an O(N) full clone.
- Precedent. The codebase has already used option (a) twice: `vrf_key_hashes`
  keeps a live `imbl::HashMap` and converts to `BTreeMap` only in the `From` impl;
  `drep_expiry` is the one field converted live (cheap — frozen state, mutated at
  most once per epoch).

Key types are NOT the blocker either way — `Hash<N>`, `GovActionId`, `Voter`,
`Credential`, `Pointer`, `TransactionInput` all already derive `Ord`.

**Not in scope:** the UTxO set. `UtxoSet::attach_store` clears the in-memory map
when the LSM/UTxO-HD store is attached, which production always does, so
`utxos` is empty at serialization time and lives outside the snapshot entirely.

**How to apply:** when adding any map field reachable from the snapshot root,
route it through the ordering boundary rather than hand-picking `OrdMap` per
field — a per-field choice is what let ~40 fields drift. The fix must land with
a multi-entry fixture: `snapshot_format_hash_stability` was only ever green
because every map in its fixture held <=1 entry. Pairs with the mandatory
38 -> 39 bump in [[snapshot38-extend-in-place-void]].
