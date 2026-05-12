---
name: N2C Conway PParams protocolVersion position
description: protocolVersion is the LAST field (index 30) in Conway PParams array(31), not index 12
type: reference
---

# Conway PParams Positional Encoding (issue #336)

## Invariant
Conway PParams CBOR is a **positional `array(31)`** with `protocolVersion`
at index **30 (LAST)**, encoded as `array(2)[major, minor]`.

The previous dugite encoder placed `protocolVersion` at index 12, shifting
every subsequent field by one slot. cardano-cli 10.15 (`transaction build`)
then read the `protocolVersion[major,minor]` array(2) as if it were
`minPoolCost`, parsed the cost-model map as a flat coin, and surfaced as a
CBOR element-count mismatch deep in the response stream
(`Final number of elements: 41315 does not match the total count that was decoded: 41319`).

## Why protocolVersion moved to the end
Conway moved `protocolVersion` out of the updatable `PParamsUpdate` map
(no ppuTag, no proposal can change it). The Haskell `eraPParams @ConwayEra`
lens list appends it via `ppGovProtocolVersion`, which becomes the final
positional entry.

Source of truth:
- `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs` (`cppHKDLensMap`, `ppGovProtocolVersion`)
- Oracle: `.claude/agent-memory/cardano-haskell-oracle/cardano-ledger-types-wire-format.md` (table at line ~410)

## Where this lives in dugite
- Encoder: `crates/dugite-node/src/node/n2c_query/encoding.rs::encode_protocol_params_cbor`
- Callers: GetCurrentPParams (tag 3), GovState (tag 24) cur/prev pparams, future pparams
- Golden test: `test_pparams_v21_positional_order_issue_336`

## Other PParams callers to check when fields shift again
`encode_protocol_params_cbor` is called from at least 6 sites (gov state,
ratify state, future pparams, etc.). If you re-order again, every consumer
that overlays a positional decoder on top will silently misread. Always
ship a golden test that asserts the value at each index, not just the count.
