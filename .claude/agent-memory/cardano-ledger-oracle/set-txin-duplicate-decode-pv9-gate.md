---
name: set-txin-duplicate-decode-pv9-gate
description: Whether a duplicate TxIn in a tx's CBOR `inputs` Set is rejected at decode time or silently deduplicated — exact PV9 gate, function, and wire-level behavior (live-verified 2026-07-29)
metadata:
  type: reference
---

## The question
Does cardano-ledger reject a CBOR-encoded transaction whose `inputs` (or
`collateral`/`referenceInputs`) field contains the same `TxIn` twice?

## Answer: it's a decoder-level gate keyed on protocol version, not a Phase-1 predicate

- Module: `Cardano.Ledger.Binary.Decoding.Decoder`, file
  `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/Decoder.hs`.
- The generic instance used for every `Set TxIn` field in every era's `TxBody` is:
  ```haskell
  instance (Ord a, DecCBOR a) => DecCBOR (Set.Set a) where
    decCBOR = decodeSet decCBOR
  ```
  (confirmed same generic instance backs `stbrInputs :: !(Set TxIn)` in Shelley
  TxBody.hs, `atbrInputs`/`atbrCollateral :: !(Set TxIn)` in Alonzo TxBody.hs,
  and `ctbrSpendInputs :: !(Set TxIn)` field-0 in Conway TxBody.hs — all three
  fetched and grepped directly, all route through `field ... From`, i.e. the
  plain non-Annotator `decCBOR` path, not `decodeAnnSet`).
- `decodeSet` (lines ~952-962) branches on the **decoder `Version`**, which is a
  plain `Word32` equal to the **protocol major version** (`MaxVersion = 13` as
  of 2026-07; `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Version.hs`):
  - **PV >= 9** (Conway onward, i.e. PV9, PV10, PV11...): `decodeSetEnforceNoDuplicates`
    -> `decodeSetLikeEnforceNoDuplicates` -> decodes a list (tag 258 permitted
    but NOT required), builds `Set.fromList`, and does
    `when (len /= count) $ fail "Final number of elements: ... does not match
    the total count that was decoded: ..."` where `count` = elements read off
    the wire and `len` = `Set.size` of the result. A literal duplicate makes
    `len < count` => **decode FAILS**.
  - **2 <= PV < 9** (Shelley through Babbage, PV2-PV8): plain
    `Set.fromList <$> decodeCollection decodeListLenOrIndef valueDecoder` — no
    duplicate check, tag 258 not permitted. `Set.fromList` silently collapses
    duplicates. **Decode SUCCEEDS**, producing a Set that is now indistinguishable
    from a normal single/fewer-input tx.
  - **PV < 2**: old exact-tag/strict-order/no-dup Shelley-launch format (tag 258
    enforced, later abandoned starting PV2).
- CHANGELOG confirmation: `libs/cardano-ledger-binary/CHANGELOG.md`, version
  1.1.0.0 entry: "Changed: Starting in version 9, duplicate keys in CBOR sets
  are not longer allowed. Additionally, the CBOR set tag 258 is permitted but
  not enforced." This is the FIRST released version of this policy — it's been
  true throughout Conway's entire life, not something later hardened.
- **Do not confuse with `decodeAnnSet`** (Annotator-based Set decode, used for
  fields needing original captured bytes elsewhere in the ledger) — that got
  the same "fail on duplicate" behavior but gated at **PV12**, a full era later,
  per the same changelog (1.8.0.0: "Make `decodeAnnSet` fail when there are
  duplicates, starting with protocol version 12"). TxIn fields do NOT use this
  path (confirmed: plain `From`/`decCBOR`), so the PV12 threshold is irrelevant
  to `inputs`/`collateral`/`referenceInputs` specifically, but relevant if a
  test touches some other Annotator-backed Set field.

## Practical consequences

1. **No Phase-1 UTXOW/UTXO predicate failure for "duplicate inputs" exists in
   any era.** There never has been one, because by the time a decoded `TxBody`
   reaches Phase-1 validation, `Set TxIn` has ALREADY either collapsed the
   duplicate (pre-PV9) or the tx never finished decoding at all (PV9+). It is
   structurally impossible for a `TxBody`'s `inputs :: Set TxIn` to reach a
   Phase-1 rule with a literal duplicate present.
2. **Historical preprod txs with duplicate inputs on the wire** (e.g. around
   epoch 35, deep in Alonzo/Babbage's PV5-PV8 range) are explained exactly by
   case 2 above: decode succeeded, `Set.fromList` deduped, the tx was processed
   as an ordinary single-input tx. This was never a bug or a laxity that got
   "fixed" per se — it's the documented set semantics for PV<9, and it changed
   specifically at the Conway (PV9) boundary as a blanket policy for ALL
   `Set`-typed wire fields (not TxIn-specific).
3. **Wire-level N2C submission for PV>=9 (Conway, i.e. current mainnet/preview/
   preprod)**: a tx with a literal duplicate `TxIn` fails **CBOR deserialisation**
   before any ledger/mempool logic runs. The `fail` call surfaces through
   cardano-ledger-binary's `DecoderError` as `DecoderErrorDeserialiseFailure`
   (confirmed constructed at `libs/cardano-ledger-binary/src/Cardano/Ledger/
   Binary/Decoding.hs:126`, `Left (e, _) -> Left $ DecoderErrorDeserialiseFailure
   lbl e`) when going through `decodeFullDecoder'`/`decodeFull`-style entry
   points, or as a raw cborg `DeserialiseFailure` when going through the mux
   codec directly. Either way this is a **codec/deserialisation-layer failure,
   not a `SubmitFail`/`ApplyTxError` domain rejection** — it is NOT the same
   code path as a normal Phase-1 UTXOW rejection with a predicate-failure list.
   A LocalTxSubmission client sending such bytes should expect the connection/
   mini-protocol to fail on decode, not a graceful "invalid tx, reasons: [...]"
   response.

## Relevance to Dugite

For a devnet negative test asserting "duplicate input in the same tx must be
rejected" under Conway PV10: the assertion is TRUE, but the mechanism to assert
is a **decode-time rejection** (malformed-CBOR / connection-level failure),
not a Phase-1 `SubmitFail` with a `DuplicateInputs`-style predicate failure —
no such predicate failure name exists anywhere in cardano-ledger. If the test
currently expects a `SubmitFail`/typed rejection reason, it is testing the
wrong layer; dugite's own `Set TxIn`-equivalent decoder needs to independently
enforce the exact same PV9 gate (reject with a decode error) to match Haskell
byte-for-byte, and dugite-cli/N2C should treat it as a submission that never
reaches Phase-1 at all (era >= Conway), while for pre-Conway eras (era <
Conway, PV < 9) dugite must silently dedup on decode exactly like `Set.fromList`
would, with zero validation-layer signal.
