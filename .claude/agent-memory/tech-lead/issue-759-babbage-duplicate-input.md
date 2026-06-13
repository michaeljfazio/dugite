---
name: issue-759-babbage-duplicate-input
description: Phase-1 DuplicateInput false positive for Babbage PV<9 txs — Haskell uses Set.fromList (silent dedup), not an error
metadata:
  type: project
---

Phase-1 Rule 1b (`DuplicateInput`) must be gated on `protocol_version_major >= 9`.

**Why:** Haskell `cardano-ledger-binary` `decodeSet` at PV < 9 routes through `Set.fromList` — silent dedup, no predicate failure. `BabbageUtxoPredFailure` has NO DuplicateInput constructor. Real mainnet tx 5ca83e21... (epoch 484, PV8) has wire-duplicate spend inputs and is accepted by cardano-node.

**Fix:** `phase1.rs` Rule 1b: `if !seen.insert(input) && params.protocol_version_major >= 9` — single && guard.

**How to apply:** Any Phase-1 check rooted in the CBOR `Set` type must check whether it applies at PV<9 vs PV>=9. Haskell only enforces set-uniqueness at PV >= 9 (`decodeSetEnforceNoDuplicates`). Conway (PV9+) rejects; Alonzo/Babbage (PV<9) silently dedups.

Fixture pinned at: `crates/dugite-ledger/src/validation/fixtures/tx-5ca83e21.hex`
