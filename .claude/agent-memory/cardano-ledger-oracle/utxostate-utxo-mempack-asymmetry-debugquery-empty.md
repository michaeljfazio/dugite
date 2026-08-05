---
name: utxostate-utxo-mempack-asymmetry-debugquery-empty
description: UTxOState.utxosUtxo has an asymmetric EncCBOR (MemPack-wrapped bstr)/DecCBOR (plain TxIn/TxOut) pair — reconciled because GetDebugNewEpochState/GetDebugEpochState are QFNoTables queries whose UTxO field is ALWAYS EMPTY in real cardano-node replies
metadata:
  type: reference
---

# `UTxOState.utxosUtxo` MemPack encoding + why DebugNewEpochState/DebugEpochState never hit it

Verified verbatim @ cardano-ledger SHA `a88b60bdcf3248dfe5a2f9372c188c399233f479` (2026-07-24) +
ouroboros-consensus (`ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Query.hs`,
default branch, fetched 2026-08-05 — no SHA pin available/needed, semantics are stable).

## The asymmetry

`UTxOState`'s own `EncCBOR` instance (`eras/shelley/impl/.../LedgerState/Types.hs`) does NOT use
`UTxO era`'s generic `EncCBOR` instance for the `utxosUtxo` field. It manually re-encodes:

```haskell
!> E (encodeMap encodeMemPack encodeMemPack . unUTxO) utxosUtxo
```
comment in source: "We need to define encoder with MemPack manually here instead of changing the
`EncCBOR` instance for `UTxO` in order to not affect some of the ledger state queries."

`encodeMemPack :: MP.MemPack a => a -> Encoding` = "Encode as bytes using `MP.MemPack` and then
encode those bytes as CBOR" — i.e. each `TxIn` key and each `TxOut` value is individually wrapped
as a CBOR **byte string** containing its `Data.MemPack` binary serialization, NOT its normal CDDL
CBOR shape. This is confirmed DIFFERENT from `TxIn`'s own standalone `EncCBOR` instance
(`libs/cardano-ledger-core/.../TxIn.hs`): `encCBOR (TxIn txId index) = encodeListLen 2 <> encCBOR
txId <> encCBOR index` — the ordinary `array(2)[txid,index]` CDDL shape.

But `UTxOState`'s `DecShareCBOR` instance does NOT mirror this:
```haskell
utxosUtxo <- decShareCBOR cs   -- delegates to UTxO era's OWN DecShareCBOR instance
```
and `UTxO era`'s own instance is `decodeMap decNoShareCBOR (decShareCBOR credsInterns)` — i.e. it
reads TxIn/TxOut in their **plain, non-MemPack** CBOR shape. So `decode(encode(x)) /= x` for a
non-empty `utxosUtxo` — encoder writes MemPack-bstr-wrapped entries, decoder expects plain-CBOR
entries. This looks like a genuine asymmetry in this exact EncCBOR/DecCBOR pair.

## Why this never bites in practice: the field is ALWAYS EMPTY when it's queried

`DebugNewEpochState :: BlockQuery (ShelleyBlock proto era) QFNoTables (SL.NewEpochState era)` and
`DebugEpochState` are both tagged `QFNoTables` and answered via `answerPureBlockQuery`:

```haskell
answerPureBlockQuery cfg query ext = case query of
  DebugEpochState    -> getEpochState st   -- = SL.nesEs st
  DebugNewEpochState -> st
  ...
 where
  lst = ledgerState ext
  st  = shelleyLedgerState lst
```

In the UTxO-HD split, `LedgerState` is parameterized by a "MapKind" that tracks whether the ledger
tables (the actual on-disk/backing-store UTxO map) are loaded. `QFNoTables` queries are, by
construction, answered from an `ExtLedgerState` whose tables were never fetched — `ext` here never
touched the backing store. So the `NewEpochState`/`EpochState` value handed back by
`DebugNewEpochState`/`DebugEpochState` has its embedded `utxosUtxo` field equal to `mempty` (an
EMPTY map) **regardless of how large the real live UTxO set is**. An empty map encodes/decodes
identically no matter which encoder nominally "owns" it (`0xa0`, zero entries — no per-entry
MemPack wrapping ever executes), which is exactly why the encode/decode mismatch above is latent
and has apparently never been hit by real cardano-node/cardano-cli traffic.

## Practical implication for a from-scratch Rust encoder

If you are implementing `GetDebugNewEpochState`/`GetDebugEpochState` LSQ encoders from scratch and
you populate `utxosUtxo` with the real, non-empty live UTxO set, you will diverge from real
cardano-node's behavior in TWO independent ways at once — you'll be using the wrong CONTENT
(should be empty) as well as risking the wrong per-entry BYTE SHAPE (MemPack-bstr vs plain CBOR) if
you ever do try to encode real entries there. The fix that matches real cardano-node exactly is
simpler than replicating MemPack: **always emit `utxosUtxo` as an empty CBOR map (`0xa0`) for these
two debug queries**, independent of the actual UTxO set size. This sidesteps the MemPack question
entirely and is what a real Haskell peer's wire bytes will show.

Also note per `Query.hs`'s own doc comment on `GetCBOR`/`DebugEpochState`: "Only for debugging
purposes, we make no effort to ensure binary compatibility... it is huge" — Haskell itself
acknowledges these two queries are not meant to be stable/round-trippable across node versions, and
recommends wrapping with `GetCBOR` (CBOR-in-CBOR, tag 24) so a mismatched client at least doesn't
disconnect on decode failure. Worth checking whether dugite's `GetDebugNewEpochState`/
`GetDebugEpochState` LSQ handlers are being invoked directly or via a `GetCBOR`-wrapped path,
since that changes the outer framing (tag 24 + bstr) independent of the inner-payload arity bug.

See also [[unit-strictmaybe-maybe-enccbor-wire-shapes]] and [[newepochstate-complete-encoding]].
