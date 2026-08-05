---
name: cardano-cli-ledger-state-decode-fallback-mechanism
description: cardano-cli `query ledger-state` decode-success vs raw-CBOR-diagnostic fallback; exact JSON shape; DecoderError is discarded
type: reference
---

## The command chain

`cardano-cli conway query ledger-state` runs `runQueryLedgerStateCmd`
(`cardano-cli/src/Cardano/CLI/EraBased/Query/Run.hs:801-837`, IntersectMBO/cardano-cli
`main`). It fetches `SerialisedDebugLedgerState era` via the N2C
`GetDebugNewEpochState` LSQ (`queryDebugLedgerState`), then dispatches on
`outputFormat :: Vary [FormatJson, FormatText, FormatYaml]`. Per the golden
help (`test/cardano-cli-golden/files/golden/help/latest_query_ledger-state.cli`),
**`--output-json` is the default** if no flag is given.

## JSON/YAML: real typed-decode-success path exists, diagnostic is fallback-only

`ledgerStateAsJsonByteString` / `ledgerStateAsYamlByteString` (Run.hs:839-872)
BOTH do this first:

```haskell
case decodeDebugLedgerState serialisedDebugLedgerState of
  Left (bs, _decoderError) -> <fallback: cborToTextByteString bs, catching exceptions
                                as QueryBackwardCompatibleError "query ledger-state">
  Right decodedLedgerState -> Json.encodeJson decodedLedgerState <> "\n"   -- (or encodeYaml)
```

`decodeDebugLedgerState` (cardano-api `Cardano.Api.Query.Internal.Type.QueryInMode`,
lines 381-388): `first (ls,) (Plain.decodeFull ls)` where `Plain.decodeFull`
is the LEGACY (unversioned) `cardano-binary` decoder, re-exported by
`Cardano.Ledger.Binary.Plain` (`module Cardano.Binary`). Its contract
(`cardano-base/cardano-binary/src/Cardano/Binary/Deserialize.hs:58-81`,
`decodeFullDecoder`): **exact, full consumption** — `Right (x, leftover) ->
if BS.null leftover then pure x else Left (DecoderErrorLeftover ...)`; any
structural mismatch anywhere → `DecoderErrorDeserialiseFailure`.

**The success JSON shape is NOT a generic `NewEpochState` dump.** It's a
hand-rolled `ToJSON (DebugLedgerState era)` instance
(`cardano-api/src/Cardano/Api/Query/Internal/Type/DebugLedgerState.hs:31-59`)
that destructures the record and re-keys exactly six top-level fields:

```haskell
[ "lastEpoch" .= nesEL
, "blocksBefore" .= nesBprev
, "blocksCurrent" .= nesBcur
, "stateBefore" .= nesEs            -- full EpochState, itself deeply nested
, "possibleRewardUpdate" .= nesRu
, "stakeDistrib" .= nesPd
]
```

`FromCBOR (DebugLedgerState era)` (same file, lines 25-29) literally calls
`fromCBOR :: Decoder s (Shelley.NewEpochState (ShelleyLedgerEra era))` — the
REAL era-indexed `NewEpochState` decoder from cardano-ledger, not a
weakened/generic one.

**CRITICAL for debugging "why did decode fail": the `DecoderError` is
discarded.** `Left (bs, _decoderError) ->` — cardano-cli throws away the
actual `DecoderError` (which for `DecoderErrorDeserialiseFailure` would
carry a byte offset / reason, or for `DecoderErrorLeftover` the exact
leftover bytes) and falls back to re-parsing `bs` generically just to
pretty-print it. **cardano-cli's own output gives you zero information
about WHERE in the nested structure the mismatch is** — only that it
failed. To find the real reason you must run `Plain.decodeFull` (or the
era's `DecCBOR (NewEpochState ...)` instance) yourself, e.g. in GHCi,
against the captured bytes, and inspect the real `DecoderError`.

**A python/generic-CBOR pass proving top-level array/map arities match is
NOT sufficient evidence the bytes will decode.** `decodeFull` walks the
exact `FromCBOR`/`DecCBOR` instance tree field-by-field, in order, at every
depth (EpochState → LedgerState → UTxOState/CertState/VState/PState/DState
→ ConwayGovState → DRepPulsingState → RatifyState → EnactState → ...). A
single wrong nested tag, wrong Map-vs-Array framing, wrong int width
forcing a different sub-decoder branch, or field-order swap anywhere in
that tree produces a decode failure indistinguishable, from the top, from
"totally broken" — the failure is almost never at the outer arity level a
manual/python trace tends to check first.

## `--output-text`: does NOT decode at all, ever

`ledgerStateAsTextByteString` (Run.hs:853-858) is a third, orthogonal path:
`pure $ unSerialised serLedgerState` — the RAW serialised CBOR bytes,
unconditionally, success-or-failure concept doesn't even apply. This is
NOT what produces the `87 # list(7)` diagnostic (that's pretty-printed
text, not raw binary) — so a report of that diagnostic format necessarily
came through the JSON or YAML formatter's FAILURE branch.

## The `87  # list(7)` / `00  # int(0)` diagnostic — confirmed source

`cborToTextByteString` → `cborToText` → `cborToTextList`
(`cardano-cli/src/Cardano/CLI/Helper.hs:106-128`):

```haskell
case deserialiseFromBytes decodeTerm bs of   -- Codec.CBOR.Read (cborg), generic Term
  Left err -> throwCliError $ CBORPrettyPrintError err
  Right (remaining, decodedVal) ->
    let text = Text.pack . prettyHexEnc $ encodeTerm decodedVal  -- Codec.CBOR.Pretty (cborg)
    in ...
```

Confirmed: **`Codec.CBOR.Pretty.prettyHexEnc`, package `cborg`**, invoked
from cardano-cli's own `Cardano.CLI.Helper` module — not dugite-adjacent
tooling, not a different tool. This is cardano-cli's genuine
backward-compatibility fallback (the discarded-branch error constructor is
literally named `QueryBackwardCompatibleError`), meant for e.g. an OLDER
cardano-cli talking to a NEWER node whose `NewEpochState` schema changed —
NOT the intended/normal rendering path.

## Verdict

Against a real Haskell cardano-node, the LSQ response bytes are produced
by the exact same `EncCBOR`/`ToCBOR (NewEpochState era)` instance that
`Plain.decodeFull`'s corresponding `FromCBOR` instance expects — so for a
real cardano-node peer, the six-key structured JSON is the normal,
expected, near-universal outcome, NOT a raw diagnostic dump. If a peer
(e.g. dugite) instead produces the raw CBOR term diagnostic, that is
positive evidence its `GetDebugNewEpochState` reply bytes fail
`Plain.decodeFull @(DebugLedgerState era)` — i.e., a real encoder bug
somewhere in the NewEpochState/EpochState/LedgerState nested tree, not a
tool artifact and not "cardano-cli always does this."

Directly relevant to dugite issue #1027 (P1: "ledger-state encoder
undecodable") — this file documents the exact mechanism that issue is
chasing, and the fact that cardano-cli discards the real `DecoderError`
means the fix must be found by decoding dugite's own emitted bytes with a
real Haskell `decodeFull` call (GHCi / small test), not by reading
cardano-cli's stderr/stdout.
