---
name: v1-txinfo-wdrl-encoding
description: Exact Data encoding of txInfoWdrl in PlutusV1 ScriptContext — List of Constr-0 pairs vs V2 Map, and the Data::Map bug trap
metadata:
  type: reference
---

## The Critical V1 vs V2 Split

**PlutusV1** (`plutus-ledger-api/src/PlutusLedgerApi/V1/Contexts.hs`):
```haskell
txInfoWdrl :: [(StakingCredential, Integer)]
```

**PlutusV2** (`plutus-ledger-api/src/PlutusLedgerApi/V2/Contexts.hs`):
```haskell
txInfoWdrl :: Map StakingCredential Integer
-- Note [V1->V2]: changed from assoc list to a PlutusTx.AssocMap
```

## How Each Is Encoded as `Data`

### V1 — `[(StakingCredential, Integer)]` → `List [Constr 0 [cred, amt], ...]`

The type is a Haskell list of 2-tuples.

- `[a]` → `ToData [a]` (from `PlutusTx.IsData.Class`):
  ```haskell
  instance ToData a => ToData [a] where
    toBuiltinData l = BI.mkList (mapToBuiltin l)
  ```
  This produces **`Data::List [...]`** — a CBOR list, NOT a map, NOT Constr.

- `(a, b)` → `ToData (a,b)` (from `PlutusTx.IsData.Instances` via `unstableMakeIsDataSchema ''(,)`):
  The TH generates `makeIsDataIndexed` with the single constructor `(,)` at index 0,
  which calls `mkConstrCreateExpr 0 [arg1, arg2]` =
  ```haskell
  BI.mkConstr 0 [toBuiltinData arg1, toBuiltinData arg2]
  ```
  This produces **`Data::Constr(0, [toData a, toData b])`**.

So a V1 wdrl with one entry looks like:
```
List [ Constr(0, [ <StakingCredential as Data>, <Integer as I> ]) ]
```

### V2 — `Map StakingCredential Integer` → `Data::Map [...]`

`PlutusTx.AssocMap.Map` ToData:
```haskell
instance (ToData k, ToData v) => ToData (Map k v) where
  toBuiltinData (Map es) = BI.mkMap (mapToBuiltin es)
```
This produces **`Data::Map [(toData k, toData v), ...]`** — the native CBOR map Data constructor.

The Babbage TxInfo (for V2) wraps with `PV2.unsafeFromList`:
```haskell
PV2.txInfoWdrl = PV2.unsafeFromList $ Alonzo.transTxBodyWithdrawals txBody
```
where `unsafeFromList` converts `[(StakingCredential, Integer)]` directly into the `Map`'s internal rep without re-encoding. So the on-wire Data is `Data::Map`.

## Credential Constructor Indices

From `makeIsDataSchemaIndexed`:
```haskell
PlutusTx.makeIsDataSchemaIndexed ''Credential
  [('PubKeyCredential, 0), ('ScriptCredential, 1)]

PlutusTx.makeIsDataSchemaIndexed ''StakingCredential
  [('StakingHash, 0), ('StakingPtr, 1)]
```

So:
- `PubKeyCredential h`  → `Constr(0, [B(28-byte hash)])`
- `ScriptCredential h`  → `Constr(1, [B(28-byte hash)])`
- `StakingHash cred`    → `Constr(0, [<Credential as Data>])`
- `StakingPtr a b c`    → `Constr(1, [I a, I b, I c])`

Full encoding for script-withdrawal credential:
```
StakingHash (ScriptCredential h)
  = Constr(0, [Constr(1, [B(28)])])
```

## cardano-ledger transWithdrawals (source)

From `cardano-ledger/eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/TxInfo.hs`:
```haskell
transWithdrawals :: Withdrawals -> Map.Map PV1.StakingCredential Integer
transWithdrawals (Withdrawals mp) = Map.foldlWithKey' accum Map.empty mp
  where
    accum ans accountAddress (Coin n) =
      Map.insert (PV1.StakingHash (transAccountAddress accountAddress)) n ans

transTxBodyWithdrawals :: EraTxBody era => TxBody t era -> [(PV1.StakingCredential, Integer)]
transTxBodyWithdrawals txBody = Map.toList (transWithdrawals (txBody ^. withdrawalsTxBodyL))
```

From `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs`:
```haskell
transCred :: Credential kr -> PV1.Credential
transCred (KeyHashObj (KeyHash kh)) =
  PV1.PubKeyCredential (PV1.PubKeyHash (PV1.toBuiltin (hashToBytes kh)))
transCred (ScriptHashObj (ScriptHash sh)) =
  PV1.ScriptCredential (PV1.ScriptHash (PV1.toBuiltin (hashToBytes sh)))

transAccountAddress :: AccountAddress -> PV1.Credential
transAccountAddress (AccountAddress _networkId (AccountId cred)) = transCred cred
```

Note: `transAccountAddress` returns `PV1.Credential` (NOT `PV1.StakingCredential`).
`transWithdrawals` wraps it: `PV1.StakingHash (transAccountAddress ...)`.

## Complete Wire Shape for One V1 Wdrl Entry

For a script credential withdrawal of 0 lovelace:
```
List [                                      -- Data::List (the [(.,.)] list)
  Constr(0, [                               -- (,) at index 0
    Constr(0, [                             -- StakingHash at index 0
      Constr(1, [                           -- ScriptCredential at index 1
        B(28-byte scripthash)               -- B bytes
      ])
    ]),
    I(0)                                    -- Integer amount
  ])
]
```

**Pair order**: credential FIRST, amount SECOND (index 0 = credential, index 1 = amount).
A script reading `snd pair` at index 1 gets `I(0)`. A script reading `fst pair` at index 0
gets the `StakingHash(ScriptCredential(...))` wrapped credential.

## The Bug: Data::Map vs Data::List

If dugite emits `Data::Map [...]` for V1 instead of `Data::List [Constr 0 [...], ...]`,
a script doing `unListData` will error with a type mismatch. V1 scripts must see a List.

If dugite emits a List but the pair elements are `Constr(0,[cred,amt])` correctly, and the
script calls `unIData` on the pair instead of on the snd of the pair, the script sees the
Constr (tag mismatch). This is the reported "unIData on non-I Data" with B28 — the script
is destructuring the pair incorrectly, but that is the script's own error assuming the pair
was unwrapped. Haskell emits the same `Constr(0,[cred,amt])` encoding.

## Key Files
- `plutus-ledger-api/src/PlutusLedgerApi/V1/Contexts.hs` — TxInfo definition
- `plutus-ledger-api/src/PlutusLedgerApi/V2/Contexts.hs` — V2 TxInfo (Map wdrl)
- `plutus-ledger-api/src/PlutusLedgerApi/V1/Credential.hs` — Credential/StakingCredential ToData
- `plutus-tx/src/PlutusTx/IsData/Class.hs` — list ToData (mkList)
- `plutus-tx/src/PlutusTx/IsData/TH.hs` — tuple ToData (mkConstr 0)
- `plutus-tx/src/PlutusTx/AssocMap.hs` — Map ToData (mkMap)
- `cardano-ledger/eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/TxInfo.hs` — transWithdrawals
- `cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs` — transCred/transAccountAddress
