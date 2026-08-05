---
name: issue-1014-auxdata-key5-shared-decoder-pv-gate
description: AlonzoTxAuxData is ONE shared decoder across all eras with per-key guardPlutus PV gates, not per-era key sets — a ceiling model that only coincides with upstream because every gated key is Plutus-version-shaped
type: reference
---

Issue #1014 (PostAlonzo tag-259 aux-data decoder silently skipped unrecognized
keys, live on Conway today — same defect class as #1013 but reachable on
mainnet, not just Dijkstra). Fixed on branch `issue-1014-missing-fields`
(commits e7e011b766, d66b9237c6), NOT yet merged to main as of 2026-08-05.

## The mechanism, byte-verified at `IntersectMBO/cardano-ledger@4849c13d6f70e5ab46add9af6e0ec5c537b61f69`

`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxAuxData.hs:263-300` — pulled the
raw file via `gh api .../contents/...?ref=<sha>` and confirmed line-for-line,
not just oracle-relayed:

```haskell
decoderByKey acc = \case
  0 -> Just $ do ... (metadata)
  1 -> Just $ do ... (native_scripts)
  2 -> decodeAddPlutus PlutusV1
  3 -> decodeAddPlutus PlutusV2
  4 -> decodeAddPlutus PlutusV3
  5 -> decodeAddPlutus PlutusV4
  _ -> Nothing
  where decodeAddPlutus lang = Just $ do guardPlutus lang; ...

auxDataField 2 = fieldA (addPlutusScripts PlutusV1) (D (guardPlutus PlutusV1 >> decCBOR))
auxDataField 3 = fieldA (addPlutusScripts PlutusV2) (D (guardPlutus PlutusV2 >> decCBOR))
auxDataField 4 = fieldA (addPlutusScripts PlutusV3) (D (guardPlutus PlutusV3 >> decCBOR))
auxDataField 5 = fieldA (addPlutusScripts PlutusV4) (D (guardPlutus PlutusV4 >> decCBOR))
auxDataField n = invalidField n
```

`DecCBOR (AlonzoTxAuxData era)` is **ONE shared instance** reused verbatim
across Alonzo/Babbage/Conway/Dijkstra (`type TxAuxData BabbageEra =
AlonzoTxAuxData BabbageEra`, etc — no era-keyed branch in the Haskell AT ALL).
Both decode paths hard-reject any key with no case arm (`decoderByKey _ ->
Nothing` -> `decodeSparseKeyed`'s `Unknown field key` failMsg; `auxDataField n
= invalidField n` -> `invalidKey`) — neither has an ignore-unknown fallback.

What makes the accept/reject boundary differ BY ERA despite one shared
instance: `guardPlutus` (`libs/cardano-ledger-core/.../Plutus/Language.hs:639-647`)
gates each Plutus-script key on **decoder protocol version**, not era:
`PlutusV1 -> natVersion @5, PlutusV2 -> natVersion @7, PlutusV3 -> natVersion
@9, PlutusV4 -> natVersion @12`. Each era's real chain only ever carries PVs
inside its own fixed range, so the PV floor reproduces an effective per-era
cap: Alonzo {0,1,2}, Babbage {0,1,2,3}, Conway {0,1,2,3,4}, Dijkstra
{0,1,2,3,4,5} — independently confirmed against each era's own CDDL
(`alonzo.cddl`/`babbage.cddl`/`conway.cddl`/`dijkstra.cddl` cap
`auxiliary_data_map` at exactly these keys).

## The trap this creates for a per-era-ceiling implementation

dugite's fix (`max_aux_data_key(era) -> u64` in
`crates/dugite-serialization/src/decode/era_alonzo.rs`) models this as a
per-era CEILING, a genuinely different mechanism from upstream's "always
matched + individually PV-gated". They coincide for every key that exists
TODAY only because every gated key (2-5) happens to be Plutus-version-shaped
and every era's PV floor lines up with the corresponding `guardPlutus`
threshold. **If upstream ever adds a new aux-data key that is NOT PV-gated
the way Plutus keys are, a ceiling model would silently reject it while
upstream accepts.** Re-derive from the live `auxDataField`/`decoderByKey`
table at that point, don't just widen the ceiling by assumption.

## Deliberate, known deviation: Dijkstra capped at 4, not the real 5

`AuxiliaryData` (dugite's Rust type) has no `plutus_v4_scripts` field, so
dugite's Dijkstra ceiling is 4 (matching Conway) even though upstream's real
Dijkstra-era cap is 5. Harmless while Dijkstra is unreleased (no live PV
reaches 12). Tracked by `aux_data_key_5_rejected_every_era_known_gap`
(asserts the CURRENT gap — same "invert when the field lands" pattern as
`pparam_update_keys_38_39_rejected_under_dijkstra_known_gap` for the sibling
PPU-key-38/39 gap, #1014's other still-open half). Recorded on the GitHub
issue itself (not just this code comment) per explicit coordinator ask —
see [[dijkstra-ppu-38-39-maxpledgeleverage-minpoolmargin-types]] for the
PPU-side upstream types, investigated but NOT implemented this pass.

## Traps hit this session, worth remembering

- A doc-comment-ONLY edit (no logic change) invalidates the `--release`
  build cache for every downstream crate — a `just check` re-run after a
  pure comment fix can take nearly as long as a full cold build. Don't
  assume a docs-only commit is free to re-verify.
- `nohup ... &` without capturing `$?` to a file leaves you unable to prove
  the true exit code later — always `cmd; echo $? > file.exit` even when
  backgrounding, and read the file, never infer success from "no error in
  the log tail" alone (though that heuristic held up here on cross-check).
- `git diff main..HEAD` is misleading whenever `main` has moved past your
  branch's fork point (sibling agents merging concurrently) — it picks up
  every file that changed on `main` since divergence, not just your own
  commits. Use `git diff $(git merge-base main HEAD)..HEAD` for a true
  self-diff.
