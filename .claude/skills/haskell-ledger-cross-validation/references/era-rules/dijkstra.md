# Dijkstra Era Ledger Rules — Delta over Conway

Source: `IntersectMBO/cardano-ledger` master, `eras/dijkstra/`.

## 1. Era identity

```haskell
instance Era DijkstraEra where
  type EraName     DijkstraEra = "Dijkstra"
  type PreviousEra DijkstraEra = ConwayEra
  type ProtVerLow  DijkstraEra = 12
  type ProtVerHigh DijkstraEra = 12
```

Activates at **PV 12.0**. HFC transitions from Conway (PV 9-11) when `HardForkInitiation` for major v12 is enacted.

`DijkstraGenesis = DijkstraGenesis { dgUpgradePParams :: UpgradeDijkstraPParams Identity DijkstraEra }` — 4 new PP at transition.

## 2. PParams additions (keys 34-37)

| Key | Field | Type | Group |
|---|---|---|---|
| 34 | `maxRefScriptSizePerBlock` | `Word32` | Network/Security |
| 35 | `maxRefScriptSizePerTx` | `Word32` | Network/Security |
| 36 | `refScriptCostStride` | `NonZero Word32` | Network/Security |
| 37 | `refScriptCostMultiplier` | `PositiveInterval` | Network/Security |

`maxRefScriptSizePerBlock`: aggregate ref-script bytes across all txs in a block.
`maxRefScriptSizePerTx`: per-tx cap.

`refScriptCostStride` + `refScriptCostMultiplier`: exponential fee schedule for ref-script usage. Stride is byte interval at which multiplier applies; multiplier ratio > 1 per stride. Schedule computed in `batchNonDistinctRefScriptsSize` (`UTxO.hs`).

### Per-tx cap check (LEDGER)
```haskell
validateAllRefScriptSize pp utxo tx =
  let totalRefScriptSize = batchNonDistinctRefScriptsSize utxo tx
      maxRefScriptSizePerTx = fromIntegral $ pp ^. ppMaxRefScriptSizePerTxG
   in failureUnless (totalRefScriptSize <= maxRefScriptSizePerTx) $
        DijkstraTxRefScriptsSizeTooBig Mismatch {...}
```

### Per-block cap check (BBODY)
Inherited from Conway via `validateBodyRefScriptsSizeTooBig`.

### Renamed fields
`dppMinFeeA`/`dppMinFeeB` deprecated aliases for renamed `dppTxFeePerByte`/`dppTxFeeFixed`. Naming cleanup, no semantic change.

## 3. TransactionBody additions

`TxBody.hs`. `DijkstraTxBodyRaw` is a GADT indexed by `TxLevel` phantom (`TopTx` or `SubTx`).

### Top-level body new fields (vs Conway 22)

| CDDL key | Field | Type | Description |
|---|---|---|---|
| 14 | `guards` | `OSet (Credential Guard)` | **Replaces `required_signers`** (now allows script-hash credentials too) |
| 23 | `sub_transactions` | `OMap TxId (Tx SubTx era)` | Ordered map of embedded sub-txs |
| 25 | `direct_deposits` | `DirectDeposits` | Map AccountAddress → Coin (direct stake account deposits) |
| 26 | `account_balance_intervals` | `AccountBalanceIntervals era` | Per-account balance constraints |

### Sub-transaction body fields
The sub-tx body (`DijkstraSubTxBodyRaw SubTx era`) **omits** collateral, totalCollateral, fee. **Adds**:

| CDDL key | Field | Type | Description |
|---|---|---|---|
| 24 | `required_top_level_guards` | `Map (Credential Guard) (StrictMaybe (Data era))` | Guards from sub-tx that must be satisfied by parent |

This is how sub-txs express upward dependencies.

## 4. Sub-transactions

Files: `Rules/SubLedger.hs`, `SubLedgers.hs`, `Ledger.hs`, `Era.hs`.

### What they are
Nearly-complete tx embedded in top-level body at key 23. Has own inputs, outputs, certs, withdrawals, validity, mint, gov procedures, guards, direct deposits, account intervals. Does NOT have own fee, collateral inputs/return, total collateral.

Haskell type: `Tx SubTx era` (disambig'd by `TxLevel` phantom). `OMap TxId (Tx SubTx era)` preserves insertion order; key = sub-tx body hash; prevents duplicates.

### Processing order
LEDGER processes sub-txs FIRST, then top-level:
```haskell
let originalUtxo = utxosUtxo (ledgerState ^. lsUTxOStateL)
    subStAnnTxs  = subTransactionsStAnnTx stAnnTx

-- Process all subtransactions first
LedgerState utxoStateAfterSubLedgers certStateAfterSubLedgers <-
  trans @(EraRule "SUBLEDGERS" era) $
    TRC (SubLedgerEnv slot mbCurEpochNo txIx pp chainAccountState originalUtxo
                       (tx ^. isValidTxL),
         ledgerState, subStAnnTxs)
```

SUBLEDGERS folds sequentially via `foldM`, threading ledger state forward.

### Nesting
**NOT recursive**. Only top-level body carries `subTransactions`. Sub-tx body type has no `dtbrSubTransactions` field. CDDL `sub_transaction_body` likewise has no recursive `sub_transactions` key.

### Witness sharing
Aggregated in UTXOW:
```haskell
let allScriptHashesNeeded =
      Set.unions $
        topScriptHashesNeeded
          : (getScriptsHashesNeeded . scriptsNeededStAnnTx <$> subStAnnTxs)
```
- Top-level witness set must cover all aggregated scripts
- Missing/extraneous checked at aggregate level
- Phase-1 (native scripts): per-level, against own guards/keys/validity
- Phase-2 (Plutus): per-level inside SUBUTXOW/SUBUTXO

A script needed by both top + sub need only appear ONCE in top-level. `TxWits DijkstraEra = AlonzoTxWits DijkstraEra` unchanged from Conway; sub-txs carry their own `transaction_witness_set` in CDDL but UTXOW aggregates.

## 5. Guards

Files: `Scripts.hs`, `Rules/Utxow.hs`, `TxBody.hs`.

### What guards are
`Credential Guard` — key-hash OR script-hash credential tagged with new `Guard` key-role phantom. Top-level: `OSet (Credential Guard)` at key 14. Sub-tx: same.

`Guard` role is NEW key-role phantom; not `Witness`/`Staking`.

### Native script guards
Adds 7th variant:
```haskell
data DijkstraNativeScriptRaw era
  = DijkstraRequireSignature !(KeyHash Witness)
  | DijkstraRequireAllOf    !(StrictSeq (DijkstraNativeScript era))
  | DijkstraRequireAnyOf    !(StrictSeq (DijkstraNativeScript era))
  | DijkstraRequireMOf !Int !(StrictSeq (DijkstraNativeScript era))
  | DijkstraTimeStart  !SlotNo
  | DijkstraTimeExpire !SlotNo
  | DijkstraRequireGuard (Credential Guard)   -- NEW: tag 6
```

Eval:
```haskell
go (RequireGuard cred) = cred `OSet.member` guards
```

### Plutus guard purpose
```haskell
data DijkstraPlutusPurpose f era
  = DijkstraSpending  !(f Word32 TxIn)
  | DijkstraMinting   !(f Word32 PolicyID)
  | DijkstraCertifying!(f Word32 (TxCert era))
  | DijkstraRewarding !(f Word32 AccountAddress)
  | DijkstraVoting    !(f Word32 Voter)
  | DijkstraProposing !(f Word32 (ProposalProcedure era))
  | DijkstraGuarding  !(f Word32 ScriptHash)   -- NEW: tag 6
```

CBOR tag 6. Redeemer tag 6 (CDDL: `redeemer_tag = 0..6`). Plutus script with `DijkstraGuarding` purpose invoked once per script-hash credential in guards.

### Guard enforcement in UTXOW
```haskell
let topLevelGuards  = OSet.toSet (txBody ^. guardsTxBodyL)
    missingGuards   = requiredGuardsBySubTxs `Set.difference` topLevelGuards
runTestOnSignal $ failureOnNonEmptySet missingGuards MissingRequiredGuards
```

`requiredGuardsBySubTxs` aggregates `dstbrRequiredTopLevelGuards` (key 24) from every sub-tx body. One-way upward dep.

## 6. Direct deposits

`DirectDeposits = Map AccountAddress Coin`. Key 25 in tx body (also key 25 in sub-tx). ADA directly into stake accounts without routing through outputs.

```haskell
validateWrongNetworkInDirectDeposit netId txb =
  failureOnNonEmptySet depositsWrongNetwork (WrongNetworkInDirectDeposit netId)
  where
    depositsWrongNetwork =
      Map.keysSet $
        Map.filterWithKey
          (\a _ -> aaNetworkId a /= netId)
          (unDirectDeposits $ txb ^. directDepositsTxBodyL)
```

Predicate failure `WrongNetworkInDirectDeposit` = UTXO tag 23.

Value conservation (`validateValueNotConservedUTxO`) accounts direct deposit as "produced" (increases account balances).

Bypasses create-output-then-stake mechanism.

## 7. Account balance intervals

`AccountBalanceIntervals` (key 26). Constrains account balances at validation time:
```haskell
data AccountBalanceInterval era
  = AccountBalanceLowerBound !(Inclusive Coin)
  | AccountBalanceUpperBound !(Exclusive Coin)
  | AccountBalanceBothBounds !(Inclusive Coin) !(Exclusive Coin)

newtype AccountBalanceIntervals era =
  AccountBalanceIntervals { unAccountBalanceIntervals :: Map AccountId (AccountBalanceInterval era) }
```

Half-open: lower inclusive, upper exclusive. CDDL: `array(2)[coin_or_null, coin_or_null]`.

In top-level + sub-tx bodies. Enables conditional logic without Plutus (e.g., prevent over-withdrawal). Validation hook in UTXO using pre-batch account state from `SubLedgerEnv`.

## 8. PlutusV4

```haskell
instance AlonzoEraScript DijkstraEra where
  data PlutusScript DijkstraEra
    = DijkstraPlutusV1 !(Plutus 'PlutusV1)
    | DijkstraPlutusV2 !(Plutus 'PlutusV2)
    | DijkstraPlutusV3 !(Plutus 'PlutusV3)
    | DijkstraPlutusV4 !(Plutus 'PlutusV4)   -- NEW

  eraMaxLanguage = PlutusV3   -- still V3 — V4 not yet activated
```

V4 present as data constructor but NOT activated yet. CDDL: language ID 3 for V4. Cost-model map key 3. Script prefix tag `0x04`.

Fully wired in MemPack (tag 3), `mkPlutusScript`, `withPlutusScript`. CBOR cost-models map: key 3 → `[* int64]`. Number of V4 cost-model entries listed as "TBD" in CDDL.

No new built-ins specific to Dijkstra documented yet — `plutus` repo would contain V4 built-ins; ledger repo has V4 at type level only.

## 9. Block header — prevNonce

```haskell
class EraBlockHeader h era => DijkstraEraBlockHeader h era where
  prevNonceBlockHeaderL :: Lens' (Block h era) Nonce
```

`prevNonceBlockHeaderL` exposes previous epoch's nonce embedded in header. Required for Peras certificate validation.

BBODY use:
```haskell
case blockBody ^. perasCertBlockBodyL of
  SNothing -> pure ()
  SJust cert ->
    let nonce = block ^. prevNonceBlockHeaderL
     in validatePerasCert nonce PerasKey cert
          ?! injectFailure (PerasCertValidationFailed cert nonce)
```

`prevNonce` currently tracked in `PraosState` as `ticknPrevHashNonce`. Dijkstra exposes via header lens so BBODY can pass to Peras validator without reaching into consensus state.

CDDL `header_body` UNCHANGED from Conway in current snapshot.

## 10. Block body — 3-element array

```
block_body = [invalid_transactions / nil, [* transaction], peras_certificate]
```

```haskell
data DijkstraBlockBodyRaw era = DijkstraBlockBodyRaw
  { dbbrTxs       :: !(StrictSeq (Tx TopTx era))
  , dbbrPerasCert :: !(StrictMaybe PerasCert)
  }
```

`PerasCert` currently a `newtype` over `ByteArray` (placeholder). `validatePerasCert` currently mocked to always return `True`. BBODY validates when present, emits `PerasCertValidationFailed` (tag 5) on failure. `SNothing` → accept without cert.

### `is_valid` deprecation
CDDL comment:
> In Dijkstra we're deprecating the `is_valid` flag, but for backwards compatibility we still allow this flag to be present in incoming transactions. Once the transaction is added to a block, the flag will be stripped. In the next era `is_valid` flags will not be allowed even in mempool transactions.

Accepted by decoder for mempool, stripped from block encoding.

## 11. Witness sharing for sub-transactions

`TxWits DijkstraEra = AlonzoTxWits DijkstraEra` unchanged. CDDL `transaction_witness_set` (both top + sub) = same 8-field map from Conway (keys 0-7), no new fields. V4 scripts opaque under same `plutus_v4_script` rule in `script_ref`.

UTXOW aggregates script hashes needed by sub-txs. Script can be supplied either:
- Top-level `transaction_witness_set` (`scriptTxWits`)
- Reference script in original UTxO

UTXOW uses `originalUtxo` (NOT mutated state) for all script lookups:
```haskell
-- All lookups use originalUtxo.
-- A subtx may consume a txout that the top-level tx references,
-- so the UTXO threaded in the state may not contain it.
let topScriptsNeeded = scriptsNeededStAnnTx stAnnTx
```

`dueOriginalUtxo` field in `DijkstraUtxoEnv` threads pre-batch UTxO access.

## 12. Era transition Conway → Dijkstra

`Translation.hs`, `Transition.hs`, `Genesis.hs`.

Context: `TranslationContext DijkstraEra = DijkstraGenesis`. Triggered by `HardForkInitiation` gov action ratified for PV 12.

NewEpochState translation:
- All Conway ledger fields carried forward (`nesBprev`, `nesBcur`, `nesRu`, `nesPd`)
- `PParams`: `upgradeDijkstraPParams` copies 31 Conway fields + fills 4 new from `DijkstraGenesis.dgUpgradePParams`
- `DRepPulsingState`: forced to `DRComplete` via `finishDRepPulser` BEFORE transition — clean ratify state at era start
- `stashedAVVMAddresses`: `()` (unchanged since Shelley)
- `UTxOState.utxosInstantStake`: coerced (`ConwayInstantStake` type unchanged)
- `DState.dsAccounts.ConwayAccountState`: coerced to Dijkstra (delegates to same underlying constructor)
- `GovAction`: upgraded via `upgradeGovAction` (handles new ParameterChange fields if any)

`Proposals`: translated via `translateProposals`, recursively upgrading each `GovActionState`, `ProposalProcedure`, `GovAction`.

## 13. Rule set summary

### Inherited unchanged from Conway
NEWEPOCH, EPOCH, ENACT, UTXOS, TICKF, RATIFY, CERTS, DELEG, HARDFORK, LEDGERS, POOLREAP, RUPD, SNAP, TICK, POOL.

### Overridden in Dijkstra
| Rule | New type | Delta |
|---|---|---|
| LEDGER | `DijkstraLEDGER` | Adds SUBLEDGERS BEFORE main UTXOW/CERTS/GOV; new ref-script-size check |
| UTXOW | `DijkstraUTXOW` | Aggregated script needs across sub-txs; MissingRequiredGuards check |
| UTXO | `DijkstraUTXO` | New env `DijkstraUtxoEnv`; batch collateral; direct-deposit network check; batch withdrawal total |
| BBODY | `DijkstraBBODY` | Peras cert validation; `prevNonceBlockHeaderL` access |
| GOV | `DijkstraGOV` | Dijkstra-specific failures |
| GOVCERT | `DijkstraGOVCERT` | Dijkstra-specific failures |
| CERT | `DijkstraCERT` | Dijkstra cert handling |
| MEMPOOL | `DijkstraMEMPOOL` | Mempool check moved out of LEDGER |

### New rules
SUBLEDGERS, SUBLEDGER, SUBCERTS, SUBCERT, SUBDELEG, SUBGOV, SUBGOVCERT, SUBPOOL, SUBUTXO, SUBUTXOW.

### Deprecated (void-era-rule, always fails)
UPEC, NEWPP, PPUP, MIR, DELEGS.

## 14. CDDL reference

```cddl
transaction_body = {
  ...                          ; keys 0-22 from Conway
  , ? 14 : guards              ; OSet<credential>  (generalised from required_signers)
  , ? 23 : sub_transactions    ; nonempty_oset<sub_transaction>     (NEW)
  , ? 25 : direct_deposits     ; { reward_account => coin }         (NEW)
  , ? 26 : account_balance_intervals                                 (NEW)
}

sub_transaction_body = {
  0  : inputs, 1 : outputs, ? 3..22 : ...,
  , ? 24 : required_top_level_guards  ; {+ credential => plutus_data / nil}  (NEW)
  , ? 25 : direct_deposits
  , ? 26 : account_balance_intervals
}
```

Key 24 ONLY in sub-tx bodies.

## 15. Not yet implemented (as of ~2026-05-23)

- `validatePerasCert` stub returning True. Pending `cardano-base` integration.
- `eraMaxLanguage = PlutusV3` — V4 not yet activated.
- V4 cost-model entry count "TBD".
- `prevNonceBlockHeaderL` no concrete instance for Praos header.
- `AccountBalanceIntervals` encoding + validation hooks present; exact ledger enforcement loop not shown.
- `PerasKey = PerasKey` placeholder.
