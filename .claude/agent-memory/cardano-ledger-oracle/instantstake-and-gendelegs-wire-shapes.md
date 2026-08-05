---
name: InstantStake type family + ConwayInstantStake/GenDelegs exact wire shapes
description: InstantStake era is an EraStake associated type family (not a single fixed type); Conway's instance is a single-field newtype over a bare Map with GND EncCBOR; GenDelegs is likewise a bare-map newtype. Both are bare CBOR maps on the wire, no array wrapper. Empty = 0xa0.
metadata:
  type: reference
---

# InstantStake era (live-verified @ a88b60bdcf3248dfe5a2f9372c188c399233f479, 2026-08-05)

`InstantStake era` is NOT one fixed type — it's an **associated type family** on the `EraStake`
class (`libs/cardano-ledger-core/src/Cardano/Ledger/State/Stake.hs`):

```haskell
class ( ..., EncCBOR (InstantStake era), DecShareCBOR (InstantStake era),
        Share (InstantStake era) ~ Interns (Credential Staking), ... ) => EraStake era where
  type InstantStake era = (r :: Type) | r -> era
  instantStakeCredentialsL :: Lens' (InstantStake era) (Map (Credential Staking) (CompactForm Coin))
  addInstantStake, deleteInstantStake :: UTxO era -> InstantStake era -> InstantStake era
  resolveInstantStake :: InstantStake era -> Accounts era -> ActiveStake
```

## Conway instance — `eras/conway/impl/src/Cardano/Ledger/Conway/State/Stake.hs`

```haskell
instance EraStake ConwayEra where
  type InstantStake ConwayEra = ConwayInstantStake ConwayEra

newtype ConwayInstantStake era = ConwayInstantStake
  { cisCredentialStake :: Map.Map (Credential Staking) (CompactForm Coin)
  }
  deriving (Generic, Show, Eq, Ord, EncCBOR, NFData, NoThunks, Default, Monoid)
```

**Answers to the three questions:**

1. **Single-field newtype**, `EncCBOR` is `GeneralizedNewtypeDeriving`-derived directly from
   `Map`'s own instance ⇒ transparent pass-through. Wire shape = **bare CBOR map**
   `{ Credential Staking => CompactForm Coin, ... }`, **NO array wrapper of any kind**. Consistent
   with how `CommitteeState`/`GenDelegs` (below) also GND straight through to their underlying
   `Map`.
2. Field list: exactly one field, `cisCredentialStake :: Map (Credential Staking) (CompactForm Coin)`.
   There is no second field in the Conway version (contrast with the legacy `ShelleyInstantStake`
   below, which has two).
3. **Empty wire bytes**: `mempty :: ConwayInstantStake era` (GND `Monoid`, from `Map.empty`) encodes
   as a **definite-length CBOR map with 0 entries = single byte `0xa0`**. Sits directly as element
   `[4]` of `UTxOState`'s `array(6)` — no array/tag/wrapper around it. (`encodeMap`'s
   definite/indefinite-at->23-entries split from [[outputtoobiguxo-maxvalsize-exact-mechanics]]
   is irrelevant here since 0 < 23.)

## Legacy shape — `ShelleyInstantStake` (used by Shelley/Allegra/Mary/Alonzo/Babbage's `EraStake`
instances, `eras/shelley/impl/src/Cardano/Ledger/Shelley/State/Stake.hs`)

```haskell
data ShelleyInstantStake era = ShelleyInstantStake
  { sisCredentialStake :: !(Map.Map (Credential Staking) (CompactForm Coin))
  , sisPtrStake        :: !(Map.Map Ptr (CompactForm Coin))
  }
instance EncCBOR (ShelleyInstantStake era) where
  encCBOR (ShelleyInstantStake cs ps) = encodeListLen 2 <> encCBOR cs <> encCBOR ps
```

Two fields (adds `sisPtrStake` for pointer-address stake, which Conway addresses cannot have) ⇒
hand-written `array(2)[credentialStakeMap, ptrStakeMap]`. This is a genuinely DIFFERENT wire shape
from Conway's bare-map — never conflate the two.

**Decode-side backward-compat shim** (`ConwayInstantStake`'s `DecShareCBOR`, custom instance, NOT
derived): it `peekTokenType`s first — `TypeListLen`/`TypeListLen64`/`TypeListLenIndef` (looks like
an array header) ⇒ decodes as legacy `ShelleyInstantStake` and drops `sisPtrStake`, keeping only
`sisCredentialStake`; any other token (i.e. a map header) ⇒ decodes directly as the bare
`Map (Credential Staking) (CompactForm Coin)`. A bare empty map (`0xa0`) correctly takes the
second branch. This shim exists so old-format ledger-state snapshots still decode; it does NOT
mean Conway's *encoder* ever emits the array form — `EncCBOR` is unconditionally GND-bare-map.

# GenDelegs — bare map, confirmed

`libs/cardano-ledger-core/src/Cardano/Ledger/Hashes.hs:301` (NOT a dedicated CertState/GenDeleg
file — corrects an implicit assumption in prior notes):

```haskell
newtype GenDelegs = GenDelegs
  { unGenDelegs :: Map (KeyHash GenesisRole) GenDelegPair
  }
  deriving (Eq, EncCBOR, DecCBOR, NoThunks, NFData, Generic, FromJSON, ToJSON)
```

Both `EncCBOR` and `DecCBOR` are GND-derived straight from the `Map` instance (ordinary `DecCBOR`,
not `DecShareCBOR` — no credential interning for the genesis-delegate registry, unlike
`InstantStake`/most other CertState maps). Wire shape: **bare CBOR map**, no array wrapper, same
family as `CommitteeState` (see [[conway-certstate-encoding]]). Empty = `0xa0`.
