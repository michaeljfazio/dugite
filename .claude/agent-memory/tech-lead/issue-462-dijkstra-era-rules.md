---
name: Dijkstra era rules dispatch (#462)
description: Conway alias removed in eras/mod.rs; DijkstraRules delegates to Conway plus identity translateEraDijkstra; remaining Dijkstra-only features tracked as ignored placeholder tests
type: project
---

Issue #462: `EraRulesImpl::for_era(Era::Dijkstra)` now returns `Dijkstra(DijkstraRules)` instead of `Conway(ConwayRules)`. The Conway alias is gone.

**Why:** The byte-patch + Conway alias was safe through preview Dijkstra activation 2026-05-07 because preview's PV12 blocks are structurally Conway-compatible at CBOR level, but it would silently mis-validate any block that actually uses Dijkstra-only features.

**How to apply:**
- `DijkstraRules` delegates every `EraRules` method to `ConwayRules` except `on_era_transition`, which implements `translateEraDijkstra` as an explicit identity (`from_era != Conway` returns `LedgerError::EpochTransition`).
- The Conway-side defensive guard (Babbage-only from_era check) is now belt-and-braces — kept in place but documented as such.
- Deferred Dijkstra-only features (sub-transactions, isValid removal, account_balance_intervals, direct_deposits, credential guards, PlutusV4, PParams 34-37, minFeeA wire-type change, peras_certificate, prevNonce header field, dijkstra-genesis.json) are catalogued as `#[ignore]` placeholder tests in `dijkstra_unimplemented` module — each pinned to a Phase in #462 and dependent on pallas Dijkstra support (#466).
- The multi_era.rs byte-patch shim (#466) is unchanged — it's the wire-decoder layer, not the rule dispatch layer.

When picking up a deferred feature: strip `#[ignore]`, replace `self.conway()` delegate in the relevant `EraRules` method with Dijkstra-specific logic, and re-verify against current `cardano-ledger@HEAD` (the spec was still in flux as of 2026-05-12).
