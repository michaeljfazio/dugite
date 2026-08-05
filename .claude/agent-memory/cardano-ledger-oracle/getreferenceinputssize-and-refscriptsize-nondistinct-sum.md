---
name: getreferenceinputssize-and-refscriptsize-nondistinct-sum
description: cardano-cli `query ref-script-size` and Conway's CIP-0112 tiered ref-script fee use the SAME underlying primitive — sum of originalBytesSize (raw captured script bytes, incl. any CBOR-bstr wrap) per matching TxIn, non-deduplicated by ScriptHash.
metadata:
  type: reference
---

Live-verified 2026-08-05 against `IntersectMBO/cardano-cli` + `IntersectMBO/cardano-api` + `IntersectMBO/cardano-ledger` (`master`).

## cardano-cli `query ref-script-size --tx-in ...`

`cardano-cli/src/Cardano/CLI/EraBased/Query/Run.hs:722-763` (`runQueryRefScriptSizeCmd`) queries UTxO for the given tx-ins, then calls `getReferenceInputsSizeForTxIds` (`cardano-api/src/Cardano/Api/Tx/Internal/Body.hs:2584-2592`):

```haskell
getReferenceInputsSizeForTxIds beo utxo txIds = babbageEraOnwardsConstraints beo $ do
  let refScripts = L.getReferenceScriptsNonDistinct utxo (Set.map toShelleyTxIn txIds)
  getSum $ foldMap (Sum . SafeHash.originalBytesSize . snd) refScripts
```

`getReferenceScriptsNonDistinct` (`eras/babbage/impl/src/Cardano/Ledger/Babbage/UTxO.hs:157-166`):
```haskell
getReferenceScriptsNonDistinct (UTxO mp) inputs =
  [ (hashScript script, script)
  | txOut <- Map.elems (Map.restrictKeys mp inputs)
  , SJust script <- [txOut ^. referenceScriptTxOutL]
  ]
```
— one `(ScriptHash, Script era)` pair per matching TxOut that HAS a reference script; TxOuts without one contribute nothing (not zero-length, just absent).

`originalBytesSize` (`libs/cardano-ledger-core/src/Cardano/Ledger/Hashes.hs:379-380`): `originalBytesSize = BS.length . originalBytes` (default method on `SafeToHash`). `originalBytes` on `Script era` is the SAME "never re-encoded, raw captured wire bytes" mechanism as `hashScript` — see [[native-script-hash-memobytes-safetohash]] for native scripts and [[plutus-script-hash-retains-one-cbor-bstr-wrapper]] for Plutus (the byte count for a Plutus reference script therefore INCLUDES its one retained CBOR-bstr wrapper header, exactly matching Koios' reported `size` field for a script).

**Answer to "(a) raw bytes vs (b) something else": it's (a), but "raw bytes" specifically means `originalBytes` — the exact bytes fed to `hashScript` minus the 1-byte tag, NOT a fresh re-encode of the decoded script, and for Plutus scripts specifically includes the retained CBOR-bstr framing.**

**Non-distinct**: if two different resolved TxIns each carry an identical (byte-for-byte, same ScriptHash) reference script, both bytes are counted — no dedup by ScriptHash. Confirmed by the function's own name and behavior (`Map.fromList` in the deduplicating sibling `getReferenceScripts` vs the plain list in `...NonDistinct`).

## Same primitive powers CIP-0112 tiered ref-script fees

`eras/conway/impl/src/Cardano/Ledger/Conway/UTxO.hs:160-170`, `txNonDistinctRefScriptsSize`:
```haskell
txNonDistinctRefScriptsSize utxo tx = getSum $ foldMap (Sum . originalBytesSize . snd) refScripts
  where
    inputs = (tx ^. bodyTxL . referenceInputsTxBodyL) `Set.union` (tx ^. bodyTxL . inputsTxBodyL)
    refScripts = getReferenceScriptsNonDistinct utxo inputs
```
Used by `getConwayMinFeeTxUtxo` (line 149-158) for the actual min-fee calculation (tiered per CIP-0112). Doc comment explicitly: "Duplicate scripts will be counted as many times as they occur, since there is never a reason to include an input with the same reference script." So the CLI query and the real fee-relevant size are the SAME formula — the only difference is which TxIn set feeds it: the CLI takes caller-supplied `--tx-in` values directly (need not be an actual transaction's real inputs+reference-inputs), the fee calc uses `referenceInputsTxBodyL ∪ inputsTxBodyL` of the real tx being fee'd.

## Rust Translation Notes (Dugite)

`crates/dugite-ledger` — any ref-script-size or tiered-fee implementation must sum `len(raw_captured_script_bytes)` (the same bytes used for `script_ref_hash`, per CLAUDE.md's Key Patterns note) per **input occurrence**, not per distinct ScriptHash, and must include reference_inputs UNION regular inputs (an input appearing in both counts once — Haskell explicitly unions the two sets before the lookup). A dugite `query ref-script-size` CLI command should mirror the exact tx-in-list semantics (arbitrary caller-supplied UTxO set, no real-tx requirement).
