---
name: gov-state-decode-is-wire-level-not-cli-level-and-fully-strict
description: query gov-state has NO Plain.decodeFull/fallback in cardano-cli at all — decode happens in the LocalStateQuery network codec via GetGovState's un-wrapped fromCBOR, equally strict as ledger-state's PParams
type: reference
---

## The question this answers

Does `cardano-cli conway query gov-state` decode as strictly as
`query ledger-state` (see [[cardano-cli-ledger-state-decode-fallback-mechanism]])?
Specifically: can `gov-state` SUCCEED (render full JSON) while the embedded
`PParams` bytes are actually malformed, the way `ledger-state`'s fallback
path can silently degrade to a hex dump on decode failure?

**Answer: no — gov-state has NO fallback at all, so its success is stronger
proof of correct PParams decode than ledger-state's, not weaker.**

## cardano-cli side: no decode call exists

`runQueryGovState` (`cardano-cli/src/Cardano/CLI/EraBased/Query/Run.hs:1601-1627`,
IntersectMBO/cardano-cli `main`):

```haskell
govState <- fromExceptTCli $ runQuery nodeConnInfo target $ queryGovState eon
let output = outputFormat & (Vary.on (\FormatJson -> Json.encodeJson) . ...) $ govState
```

No `Plain.decodeFull`, no `Left (bs, _) -> fallback` branch, no
`cborToTextByteString`/`CBORPrettyPrintError` anywhere near this function
(confirmed by grep — those symbols only appear in the `ledger-state`/
`protocol-state` handlers). `govState` arrives from `runQuery` **already
fully decoded** as a Haskell value; cardano-cli just JSON-encodes it via the
derived `ToJSON (ConwayGovState era)` (`KeyValuePairs` deriving,
`cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs:388-403`).

## Where the decode actually happens: the LocalStateQuery network codec

`queryGovState` maps to `QueryGovState :: QueryInShelleyBasedEra era
(L.GovState (ShelleyLedgerEra era))`
(`cardano-api/src/Cardano/Api/Query/Internal/Type/QueryInMode.hs:308-309`),
dispatched as `Consensus.GetGovState` — **NOT** wrapped in the `GetCBOR`
combinator, unlike `QueryDebugLedgerState`:

```haskell
QueryGovState -> Some (consensusQueryInEraInMode era Consensus.GetGovState)          -- line 655-656, bare
QueryDebugLedgerState -> Some (consensusQueryInEraInMode era
                                  (Consensus.GetCBOR Consensus.DebugNewEpochState))    -- line 625-626, WRAPPED
```

`GetCBOR` is documented in ouroboros-consensus
(`ouroboros-consensus-cardano/.../Shelley/Ledger/Query.hs:178-197`) as
existing SPECIFICALLY so a decode failure degrades to a CBOR-in-CBOR
hex-dump fallback instead of killing the connection ("the client always
successfully decodes the outer CBOR layer... can then fall back to pretty
printing"). `GetGovState` gets none of that: its result type is the bare
`LC.GovState era`, decoded directly by the query codec's dispatch table
(same file, lines 1032/1081):

```haskell
encodeShelleyResult v query = case query of
  ...
  GetGovState -> toCBOR
decodeShelleyResult v query = case query of
  ...
  GetGovState -> fromCBOR
```

This `fromCBOR :: Decoder s (GovState era)` runs **inside the
LocalStateQuery mini-protocol codec**, at the point the `MsgResult` reply is
received off the wire — before `runQuery` even returns to cardano-cli. A
decode failure here is a hard codec/protocol failure (propagates as an
exception through `runQuery`/`fromExceptTCli`), not a value cardano-cli can
inspect, retry, or render as a diagnostic. There is structurally no
lenient path for `GetGovState` to take.

## Confirmed: cgsCurPParams uses the SAME strict PParams decoder

`ConwayGovState`'s `DecCBOR` instance
(`cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs:337-357`):

```haskell
instance EraPParams era => DecShareCBOR (ConwayGovState era) where
  decSharePlusCBOR =
    decodeRecordNamedT "ConwayGovState" (const 7) $ do
      cgsProposals <- decSharePlusCBOR
      cgsCommittee <- lift decCBOR
      cgsConstitution <- lift decCBOR
      cgsCurPParams <- lift decCBOR        -- generic DecCBOR (PParams era)
      cgsPrevPParams <- lift decCBOR
      cgsFuturePParams <- lift decCBOR
      cgsDRepPulsingState <- decSharePlusCBOR
      pure ConwayGovState {..}

instance EraPParams era => DecCBOR (ConwayGovState era) where
  decCBOR = decNoShareCBOR
```

`decodeRecordNamedT "ConwayGovState" (const 7)` is a strict fixed-7-field
record decode (mirrors the `decodeRecordNamed "PParams" (const 31)` pattern
that produces "Expected 31, but found N" errors). `cgsCurPParams <- lift
decCBOR` dispatches to the exact same polymorphic `DecCBOR (PParams
ConwayEra)` instance used by the standalone `GetCurrentPParams` query
(`protocol-parameters`) and by `ledger-state`'s
`EpochState -> LedgerState -> ... -> PParams` chain — there is no separate,
weaker PParams decoder anywhere in this path.

## Verdict / diagnostic implication

`gov-state` and `ledger-state`/`protocol-parameters` are decoded by the
IDENTICAL `PParams` type-class instance, with `gov-state` actually having
LESS tolerance for failure (no fallback exists at all — success is binary,
proven-correct-or-hard-exception). So: if a Rust node's `gov-state` reply
renders full correct JSON while its `protocol-parameters`/`ledger-state`
reply fails PParams arity ("Expected 31, but found 22"), the PParams BYTES
sent on those two wire paths are provably NOT the same bytes — the node has
at least two drifted PParams encoder call sites (one correct, one wrong),
not one buggy PParams encoder used everywhere. Look for multiple encoder
call sites / copies for `PParams`, not a single shared bug.

Directly relevant to dugite issue #1027 ("ledger-state encoder
undecodable") — the gov-state success is real evidence the bug is
call-site-specific (e.g. `GetCurrentPParams`/tag-3 or the
`GetDebugNewEpochState` embedded copy), not in the PParams encoder itself,
per dugite's own documented "N-copies trap" pattern (see CLAUDE.md #977,
#980, #985, #996 — same shape, different field).
