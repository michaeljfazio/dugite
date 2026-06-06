---
name: phase2-script-context-bugs
description: Phase-2 ScriptContext + flat-decoder bugs A-H fixed — TxId wrapping, fee as Value, V1/V2 schema differences, flat depth limit
type: project
---

8 bugs fixed in `crates/dugite-uplc/src/script_context.rs`, `flat/mod.rs`, `flat/term.rs`, `eval_redeemer.rs`:

**Bug A** — ADA policy key was 28 zero-bytes, must be `b""` (empty).
**Bug B** — PosixTimeRange missing 3-layer nesting: `Interval(LowerBound(Extended,Bool),UpperBound(Extended,Bool))`.
**Bug C** — flat decoder only handled atoms 0-4; atoms 5/6/7/8 (List/Pair/Apply/Data) rejected. Full `parse_type_from_atoms` added.
**Bug D** — V1/V2 `txInfoWdrl` schema difference: V1=`List[Constr 0[cred,amt]]` (AssocList), V2=`Map[(cred,amt)]` (AssocMap).
**Bug E (DOMINANT)** — TxId NOT wrapped in Constr 0: `TxOutRef.tx_id` must be `Constr 0[B bytes32]` NOT bare `B bytes32`. Breaks ALL spending scripts (`unConstrData on non-Constr Data`). Also applies to `txInfoId` field and V3 `GovActionId.tx_id`.
**Bug F** — fee encoded as bare `I lovelace` instead of `Value = Map[(b"",Map[(b"",I lovelace)])]`.
**Bug G** — V1 `txInfoData` was Map; must be `List[Constr 0[B32,datum]]` (AssocList). V2 correctly uses Map.
**Bug H** — flat decoder depth limit 256 too low for large DeFi scripts (10KB+ exceed 256 levels). Raised to 4096 + added `stacker::maybe_grow` for stack safety. Also fixed `decode_script_bytes` fallback: if outer bytes look like CBOR (major-type 2), don't fall through to raw flat.

**Key schema rules:**
- `TxId = Constr 0 [B bytes32]` (newtype index 0) — NOT bare bytes
- `TxOutRef = Constr 0 [TxId, Integer]` (NOT `Constr 0 [B bytes32, I idx]`)
- `txInfoFee :: Value = Map[(b"",Map[(b"",I lovelace)])]`
- V1 TxOut: 3 fields `[Address, Value, Maybe DatumHash]`; V2 TxOut: 4 fields `[Address, Value, OutputDatum, Maybe ScriptHash]`
- V1 wdrl/data: AssocList (`List[Constr 0[key,val]]`); V2 wdrl/data: AssocMap (`Map[(key,val)]`)

**Files changed:**
- `crates/dugite-uplc/src/script_context.rs` — `TxOutRef::to_data`, `GovActionId::to_data`, `TxOut::to_data_v1()`, `TxInInfo::to_data_v1()`, `TxInfoV1::to_data`, `TxInfoV2::to_data`, `TxInfoV3::to_data`, helper `data_txid()`, `data_ada_value()`
- `crates/dugite-uplc/src/flat/mod.rs` — `FLAT_MAX_DEPTH: 256 → 4096`
- `crates/dugite-uplc/src/flat/term.rs` — `decode_term_depth` split with `stacker::maybe_grow`
- `crates/dugite-uplc/src/eval_redeemer.rs` — `decode_script_bytes` CBOR-major-type gate
- `crates/dugite-uplc/Cargo.toml` — added `stacker = "0.1"`
- `crates/dugite-uplc/tests/phase2_script_context_regression.rs` — 20+ regression tests

**E2E gate passed:**
```
VERIFY 3d305521...: OK (1 redeemers passed)
VERIFY c8b4cf48...: OK (4 redeemers passed)
VERIFY b556be38...: OK (1 redeemers passed)
```
Plus 10/10 sampled from /tmp/phase2dump/ all pass (0 structural errors).
387/387 unit tests pass, clippy clean, fmt clean.

**Why TxId-wrapping alone is dominant:** Bug E causes `unConstrData on non-Constr Data` on every spend redeemer (every validator navigates its own input's outref). Bugs F/G are secondary (only scripts that explicitly inspect fee/data fields hit them).
