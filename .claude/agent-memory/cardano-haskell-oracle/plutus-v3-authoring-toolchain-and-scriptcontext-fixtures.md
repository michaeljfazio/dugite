---
name: plutus-v3-authoring-toolchain-and-scriptcontext-fixtures
description: Canonical IntersectMBO Plinth (plutus-tx) toolchain for authoring V3 validators, V3 single-arg calling convention proof, exact ScriptContext/TxInfo Data field order, and real pre-compiled context-inspecting .plutus fixtures for devnet testing
type: reference
---

Researched 2026-08-02 against IntersectMBO/plutus tag `1.66.0.0` (master resolves
to this version; release `1.66.0.0` published 2026-07-15), IntersectMBO/plinth-template
`main`, IntersectMBO/cardano-api `master`, IntersectMBO/cardano-ledger `master`,
IntersectMBO/cardano-node-tests `master`.

## Canonical authoring path (Q1)

- Compiler = `plutus-tx` + `plutus-tx-plugin` (GHC plugin), packages published on
  CHaP (`https://chap.intersectmbo.org/`), NOT Hackage.
- GHC: "Plinth supports a specific major version of GHC (currently 9.6)."
  (`doc/docusaurus/docs/using-plinth/environment-setup.md`). CI matrix in
  `.github/workflows/cabal-build-all.yml` is `ghc: [ghc912, ghc96]`, default via
  nix `compiler-nix-name = lib.mkDefault "ghc96"` (`nix/project.nix`). No more
  specific patch (e.g. 9.6.5 vs 9.6.6) is pinned anywhere in-repo — don't
  overclaim a patch version.
- Cabal >=3.8 recommended (`plinth-template/README.md`).
- Reference starter repo is **`IntersectMBO/plinth-template`** (NOT the archived
  `plutus-starter`). `plinth-template.cabal` pins
  `plutus-core/plutus-ledger-api/plutus-tx/plutus-tx-plugin ^>=1.66.0.0`, and
  Plinth-only ghc-options include
  `-fplugin-opt PlutusTx.Plugin:target-version=1.1.0` (targets Plutus Core
  1.1.0, the sums-of-products version, CIP-85).
- `src/AuctionValidator.hs` in plinth-template is IntersectMBO's own real,
  complete V3 example: imports `PlutusLedgerApi.V3 (ScriptContext(..), ScriptInfo(..), ...)`,
  pattern-matches `ScriptContext txInfo scriptRedeemer scriptInfo`, extracts
  the redeemer via `PlutusTx.fromBuiltinData (getRedeemer scriptRedeemer)`,
  extracts the datum via `case scriptInfo of SpendingScript _ (Just (Datum d)) -> ...`,
  and `PlutusTx.traceError`s on every mismatch ("Failed to parse
  AuctionRedeemer", "Expected SpendingScript with datum", "Not found: refund
  output", etc.) — i.e. it errors when fields don't match, exactly the shape
  the user wants for a devnet field-inspecting validator.
- Untyped wrapper + compile:
  ```haskell
  auctionUntypedValidator :: AuctionParams -> BuiltinData -> PlutusTx.BuiltinUnit
  auctionUntypedValidator params ctx =
    PlutusTx.check (auctionTypedValidator params (PlutusTx.unsafeFromBuiltinData ctx))

  auctionValidatorScript :: AuctionParams -> CompiledCode (BuiltinData -> PlutusTx.BuiltinUnit)
  auctionValidatorScript params =
    $$(PlutusTx.compile [||auctionUntypedValidator||])
      `PlutusTx.unsafeApplyCode` PlutusTx.liftCode plcVersion110 params
  ```
- KNOWN UPSTREAM QUIRK (flag this if citing plinth-template): its own
  `app/GenAuctionValidatorBlueprint.hs` declares
  `preamblePlutusVersion = PlutusV2` and `compiledValidator PlutusV2 code` for
  the blueprint, even though `AuctionValidator.hs`'s own comment says "In this
  example we are writing a Plutus V3 scripts" and the validator type is the
  V3 single-arg shape. This looks like a stale leftover in IntersectMBO's own
  template repo, not an intentional V2/V3 duality.

## V3 = single BuiltinData argument — proof, not just doc claim (Q1/Q5)

Doc statement (`doc/docusaurus/docs/working-with-scripts/ledger-language-version.md`):
> "All Plutus V3 scripts, regardless of script purpose, take a single
> argument: the script context. The datum (for spending scripts) and the
> redeemer are part of the Plutus V3 script context... all Plutus V3 scripts
> should have the following type in Plinth: `BuiltinData -> BuiltinUnit`"
Attributed to CIP-69 (single-arg + optional datum) and CIP-117 (BuiltinUnit
return requirement).

API-level PROOF (not just prose) — `plutus-ledger-api/src/PlutusLedgerApi/V3.hs`:
```haskell
evaluateScriptCounting mpv verbose ec s arg =
  Common.evaluateScriptCounting thisLedgerLanguage mpv verbose ec s [arg]
  -- arg :: Common.Data  -- "The @ScriptContext@ argument to the script"
```
vs `V1.hs`/`V2.hs`, whose `evaluateScriptCounting`/`evaluateScriptRestricting`
take `[Common.Data]` (a caller-supplied list — 3 elements for spending, 2 for
others). V3's own module hardcodes the singleton list at the API boundary;
V1/V2 leave the arity to the caller. This is the strongest evidence available
short of reading the CEK apply loop itself.

## Exact ScriptContext / TxInfo Data field order (Q5)

Source: `plutus-ledger-api/src/PlutusLedgerApi/V3/Contexts.hs` (tag 1.66.0.0).
All indices below are the literal `makeIsDataSchemaIndexed` splices at the
bottom of that file — constructor tag first, then record-declaration field
order (record field order determines `Data` field position; only the
constructor tag is chosen explicitly).

```
ScriptContext (single constructor, tag 0):
  0 scriptContextTxInfo     :: TxInfo
  1 scriptContextRedeemer   :: V2.Redeemer
  2 scriptContextScriptInfo :: ScriptInfo

TxInfo (single constructor, tag 0) -- 16 fields:
  0  txInfoInputs                 :: [TxInInfo]
  1  txInfoReferenceInputs        :: [TxInInfo]
  2  txInfoOutputs                :: [V2.TxOut]
  3  txInfoFee                    :: V2.Lovelace
  4  txInfoMint                   :: V3.MintValue
  5  txInfoTxCerts                :: [TxCert]
  6  txInfoWdrl                   :: Map V2.Credential V2.Lovelace
  7  txInfoValidRange             :: V2.POSIXTimeRange
  8  txInfoSignatories            :: [V2.PubKeyHash]
  9  txInfoRedeemers              :: Map ScriptPurpose V2.Redeemer
  10 txInfoData                   :: Map V2.DatumHash V2.Datum
  11 txInfoId                     :: V3.TxId
  12 txInfoVotes                  :: Map Voter (Map GovernanceActionId Vote)
  13 txInfoProposalProcedures     :: [ProposalProcedure]
  14 txInfoCurrentTreasuryAmount  :: Maybe V2.Lovelace
  15 txInfoTreasuryDonation       :: Maybe V2.Lovelace

ScriptInfo constructors (replaces V1/V2's ScriptPurpose as the
scriptContextScriptInfo carrier; adds the optional datum for spending):
  0 MintingScript    CurrencySymbol
  1 SpendingScript    TxOutRef (Maybe Datum)
  2 RewardingScript   Credential
  3 CertifyingScript  Integer TxCert   -- Integer = 0-based index into txInfoTxCerts
  4 VotingScript      Voter
  5 ProposingScript   Integer ProposalProcedure  -- index into txInfoProposalProcedures

ScriptPurpose constructors (used as the Map key type inside txInfoRedeemers
ONLY -- ScriptContext itself carries ScriptInfo, not ScriptPurpose):
  0 Minting    CurrencySymbol
  1 Spending   TxOutRef
  2 Rewarding  Credential
  3 Certifying Integer TxCert
  4 Voting     Voter
  5 Proposing  Integer ProposalProcedure

Voter: 0 CommitteeVoter HotCommitteeCredential | 1 DRepVoter DRepCredential | 2 StakePoolVoter PubKeyHash
Vote:  0 VoteNo | 1 VoteYes | 2 Abstain
TxCert: 0 TxCertRegStaking .. 10 TxCertResignColdCommittee (11 variants, see file for full arg lists)
GovernanceAction: 0 ParameterChange .. 6 InfoAction (7 variants)
DRep: 0 DRep DRepCredential | 1 DRepAlwaysAbstain | 2 DRepAlwaysNoConfidence
Delegatee: 0 DelegStake | 1 DelegVote | 2 DelegStakeVote
```

Note: `V2.Redeemer`/`V2.Datum`/`ChangedParameters` etc. are
`newtype X = X BuiltinData deriving newtype (ToData, FromData, UnsafeFromData)`
— they encode TRANSPARENTLY as their underlying `Data` payload, no extra
Constr wrapping. Confirmed directly for `ChangedParameters` in the same file;
same idiom applies to `Redeemer`/`Datum` throughout `PlutusLedgerApi.V1/V2`.

## Compiling to a .plutus text envelope (Q2)

- `serialiseCompiledCode :: CompiledCode a -> SerialisedScript` in
  `plutus-ledger-api/src/PlutusLedgerApi/Common/SerialisedScript.hs`.
  `SerialisedScript = ShortByteString`. Internally:
  `serialiseCompiledCode = serialiseUPLC . toNameless . getPlcNoAnn`, and
  `serialiseUPLC = toShort . BSL.toStrict . serialise . SerialiseViaFlat . UPLC.UnrestrictedProgram`
  — i.e. the OUTPUT IS ALREADY CBOR-wrapped flat bytes (one CBOR bytestring
  header around the Flat-encoded program), not raw Flat.
- cardano-api (`cardano-api/src/Cardano/Api/Plutus/Internal/Script.hs`):
  `data PlutusScript lang where PlutusScriptSerialised :: ShortByteString -> PlutusScript lang`
  and `serialiseToCBOR (PlutusScriptSerialised sbs) = SBS.fromShort sbs`
  (verbatim passthrough — no further wrapping). So
  `teRawCBOR == serialiseCompiledCode code` bit-for-bit.
- `textEnvelopeType` for `PlutusScript PlutusScriptV3` = the literal string
  `"PlutusScriptV3"` (`instance IsPlutusScriptLanguage lang => HasTextEnvelope (PlutusScript lang)`,
  same file). PlutusScriptV1/V2/V4 exist too (V4 = forthcoming Dijkstra era,
  already stubbed in cardano-api master).
- JSON shape (`Cardano.Api.Serialise.TextEnvelope.Internal`):
  `{"type": teType, "description": teDescription, "cborHex": hex(teRawCBOR)}`.
  `writeFileTextEnvelope :: HasTextEnvelope a => File content Out -> Maybe TextEnvelopeDescr -> a -> IO (...)`.
- PRACTICAL CONSEQUENCE: you do NOT need cardano-api as a Haskell dependency
  to produce a valid `.plutus` file. `hex(serialiseCompiledCode code)` dropped
  straight into `{"type":"PlutusScriptV3","description":"","cborHex":"<hex>"}`
  is byte-identical to what `writeFileTextEnvelope` would write.

## Real pre-compiled ScriptContext-inspecting V3 fixtures WITHOUT building GHC (Q3)

- `IntersectMBO/plutus` itself deliberately has NO plutus-tx-compiled fixtures
  in its own core packages — explicit design note in
  `plutus-ledger-api/testlib/PlutusLedgerApi/Test/Examples.hs`:
  > "Note [Manually constructing scripts] ... Why not use our great machinery
  > for easily creating scripts with Plutus Tx? Because Plutus Tx relies on a
  > compiler plugin ... It seems better therefore to avoid depending on
  > Plutus Tx in any 'core' projects like the ledger."
  Confirmed: `cardano-ledger`'s own cabal files (`cardano-ledger-core`,
  `cardano-ledger-alonzo`, `cardano-ledger-conway`, `cardano-ledger-test`) all
  depend on `plutus-core`/`plutus-ledger-api` but NEVER on `plutus-tx` or
  `plutus-tx-plugin`. So the "ledger-rules" CI job dugite already runs (builds
  cardano-ledger from source) has GHC 9.6.x + cabal + CHaP warm, and already
  builds `plutus-ledger-api` + its public `plutus-ledger-api-testlib`
  sublibrary (see `cardano-ledger-core.cabal:270`:
  `plutus-ledger-api:{plutus-ledger-api, plutus-ledger-api-testlib}`), but does
  NOT already have `plutus-tx-plugin` (the actual Plinth compiler) warm.
- REAL FIXTURES DO EXIST, in **`IntersectMBO/cardano-node-tests`** under
  `cardano_node_tests/tests/data/plutus/v3/*.plutus` — genuinely pre-compiled,
  checked into git, usable directly with `cardano-cli` with zero GHC. Notably
  NOT just always-true/false: `witnessRedeemerPolicyScriptV3.plutus`,
  `timeRangePolicyScriptV3.plutus` (checks `txInfoValidRange`),
  `mintTokenNamePolicyScriptV3.plutus`, `constitutionScriptV3.plutus` (real
  governance guardrail shape), `verifyEcdsaPolicyScriptV3.plutus`/
  `verifySchnorrPolicyScriptV3.plutus`, plus a large `batch6/{1.0.0,1.1.0}/`
  set of Value-semantics and CIP-85 (constr/case) conformance scripts.
  Referenced from `cardano_node_tests/tests/plutus_common.py` (e.g.
  `MINTING_TIME_RANGE_PLUTUS_V3 = SCRIPTS_V3_DIR / "timeRangePolicyScriptV3.plutus"`,
  `MINTING_WITNESS_REDEEMER_PLUTUS_V3 = ...`). Did NOT find the Haskell source
  that generated these (not in a public IntersectMBO repo I could locate) —
  only the compiled `.plutus` artifacts are public. Treat as "real compiled
  fixture, provenance unpinned" if used.
- `plutus-ledger-api/test/Spec/v1-context-data` is a single checked-in binary
  CBOR `Data` file (a real V1 ScriptContext, NOT a script) used by
  `Spec.ContextDecoding` to assert it parses as V1 but NOT as V2/V3 — useful
  precedent, not directly reusable (V1 only).

## Constructing ScriptContext values for byte-level diffing without a validator at all (Q3/Q4, arguably better than Q3's answer)

`plutus-ledger-api` ships a PUBLIC sublibrary **`plutus-ledger-api-testlib`**
(`visibility: public` in `plutus-ledger-api.cabal`, hs-source-dirs `testlib`)
containing `PlutusLedgerApi.Test.ScriptContextBuilder.Builder` — a real,
composable Haskell builder for `ScriptContext` values (`buildScriptContext`,
`withRedeemer`, `withMint`, `withSpendingScript`, `withInput`, `withOutput`,
`withValidRange`, `addInput`/`addOutput`/`addMint`, etc., all lens-based over
the real `ScriptContext`/`TxInfo` types). cardano-ledger's OWN test suite
already depends on this (`cardano-ledger-core.cabal:270`). Since this already
builds cleanly in the GHC 9.6.x / CHaP setup dugite's `ledger-rules` job
already uses, the single lowest-effort, highest-precision path to Q4 is: pull
in `plutus-ledger-api-testlib` in that same job, construct a `ScriptContext`
for a synthetic tx, call `PlutusTx.toData`/CBOR-serialise it, and diff those
BYTES against dugite's Rust-constructed context for the identical synthetic
tx — no validator run required at all, no plutus-tx-plugin dependency either
(this library is pure `plutus-ledger-api`, no GHC-plugin compile step).
There is no separate "golden CBOR ScriptContext corpus" file shipped anywhere
in the plutus repo for V3 (the only checked-in one, `v1-context-data`, is V1).

## Practical recommendation given dugite's existing pipeline (Q6)

Two complementary additions, in order of effort:
1. Zero-GHC: vendor 3-4 of the cardano-node-tests v3 `.plutus` fixtures above
   (witness-redeemer / time-range / mint-token-name / constitution) through
   the existing conformance-corpus pipeline
   (`scripts/regenerate-conformance-corpus/`, `tests/conformance/upstream/`)
   as an 8th fixture area, and use them as devnet minting-policy validators
   that actually reject on a wrong `txInfoValidRange`/signatory/token name —
   this alone satisfies the user's literal ask (real IntersectMBO validators
   that inspect context on the wire) with no new CI job.
2. If a bespoke validator checking MORE of TxInfo than any single upstream
   fixture covers is wanted: extend the SAME `ledger-rules` CI job (GHC
   9.6.x/cabal/CHaP already resident there for cardano-ledger) by adding
   `plutus-tx`/`plutus-tx-plugin ^>=1.66.0.0` (same CHaP index-state the job
   already trusts) and one module modeled on plinth-template's
   `AuctionValidator.hs`; skip the cardano-api dependency and hand-emit the
   `.plutus` JSON from `hex(serialiseCompiledCode code)` (see Q2 above);
   publish as a release asset like the other seven areas.

See also: [v1v2-scriptcontext-conway-gates.md] for the V1/V2 side of Conway
gating, [plutus-flat-wire-format-defaultfun.md] for the on-chain Flat/UPLC
wire format `serialiseUPLC` ultimately produces.
