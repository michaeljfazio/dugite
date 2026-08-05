---
name: Conway CertState/DState/PState/VState CBOR encoding
description: Complete encoding for Conway CertState and all sub-states, including StakePoolState vs StakePoolParams distinction
type: reference
---

# Conway CertState CBOR Encoding

Source files:
- ConwayCertState: `eras/conway/impl/src/Cardano/Ledger/Conway/State/CertState.hs`
- VState: `eras/conway/impl/src/Cardano/Ledger/Conway/State/VState.hs`
- DState/PState: `libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs`
- StakePoolState/StakePoolParams: `libs/cardano-ledger-core/src/Cardano/Ledger/State/StakePool.hs`

## ConwayCertState = array(3)

Encoder:
```haskell
encodeListLen 3
  <> encCBOR conwayCertVState
  <> encCBOR conwayCertPState
  <> encCBOR conwayCertDState
```

```
array(3)
  [0] conwayCertVState :: VState   (array(3))
  [1] conwayCertPState :: PState   (array(4))
  [2] conwayCertDState :: DState   (array(4))
```

CONFIRMED: VState is encoded FIRST. Order: V, P, D.

## VState = array(3)

Uses `Rec (VState @era) !> To vsDReps !> To vsCommitteeState !> To vsNumDormantEpochs`

```
array(3)
  [0] vsDReps             :: Map (Credential DRepRole) DRepState
  [1] vsCommitteeState    :: CommitteeState  (map credential -> authorization)
  [2] vsNumDormantEpochs  :: EpochNo         (u64)
```

## PState = array(4)

Encoder: `encodeListLen 4 <> encCBOR a <> encCBOR b <> encCBOR c <> encCBOR d`
where fields are (a=psVRFKeyHashes, b=psStakePools, c=psFutureStakePoolParams, d=psRetiring)

```
array(4)
  [0] psVRFKeyHashes          :: Map (VRFVerKeyHash StakePoolVRF) (NonZero Word64)
  [1] psStakePools            :: Map (KeyHash StakePool) StakePoolState  (NEW type!)
  [2] psFutureStakePoolParams :: Map (KeyHash StakePool) StakePoolParams (old registration params)
  [3] psRetiring              :: Map (KeyHash StakePool) EpochNo
```

CRITICAL DISTINCTION:
- psStakePools maps pool hash -> StakePoolState (10 fields, includes delegators set + deposit)
- psFutureStakePoolParams maps pool hash -> StakePoolParams (9 fields via CBORGroup, no deposit/delegators)
These are different types!

## StakePoolState = array(10)

Uses `Rec StakePoolState !> To ... (10 fields)`

```
array(10)
  [0] spsVrf          :: VRFVerKeyHash StakePoolVRF  (32 bytes)
  [1] spsPledge       :: Coin
  [2] spsCost         :: Coin
  [3] spsMargin       :: UnitInterval
  [4] spsAccountId    :: AccountId (= Credential Staking)
  [5] spsOwners       :: Set (KeyHash Staking)
  [6] spsRelays       :: StrictSeq StakePoolRelay
  [7] spsMetadata     :: StrictMaybe PoolMetadata
  [8] spsDeposit      :: CompactForm Coin
  [9] spsDelegators   :: Set (Credential Staking)
```

This is the NEW UTxO-HD era type with embedded deposit and delegator tracking.

## StakePoolParams = array(9) via CBORGroup

Used for psFutureStakePoolParams. Encoded via EncCBORGroup (listLen=9, fields encoded without header).
When encoded via CBORGroup as a standalone value: `array(9) [id, vrf, pledge, cost, margin, accountAddress, owners, relays, metadata_or_null]`

```
[0] sppId             :: KeyHash StakePool   (28 bytes)
[1] sppVrf            :: VRFVerKeyHash       (32 bytes)
[2] sppPledge         :: Coin
[3] sppCost           :: Coin
[4] sppMargin         :: UnitInterval
[5] sppAccountAddress :: AccountAddress (= Credential Staking)
[6] sppOwners         :: Set (KeyHash Staking)
[7] sppRelays         :: StrictSeq StakePoolRelay
[8] sppMetadata       :: null | PoolMetadata (encodeNullStrictMaybe)
```

Note: metadata uses encodeNullStrictMaybe (CBOR null for Nothing, not array(0)).

## DState = array(4)

Encoder: `encodeListLen 4 <> encCBOR dsAccounts <> encCBOR dsFutureGenDelegs <> encCBOR dsGenDelegs <> encCBOR dsIRewards`

```
array(4)
  [0] dsAccounts         :: Accounts era  (Conway: ConwayAccounts = map credential->ConwayAccountState)
  [1] dsFutureGenDelegs  :: Map FutureGenDeleg GenDelegPair
  [2] dsGenDelegs        :: GenDelegs     (map keyhash -> GenDelegPair)
  [3] dsIRewards         :: InstantaneousRewards (array(4))
```

## InstantaneousRewards = array(4)

```
array(4)
  [0] iRReserves    :: Map (Credential Staking) Coin
  [1] iRTreasury    :: Map (Credential Staking) Coin
  [2] deltaReserves :: DeltaCoin  (integer, may be negative)
  [3] deltaTreasury :: DeltaCoin
```

Still present verbatim in Conway's DState at this commit (survives structurally even though MIR
certs were removed from the Conway tx-body/cert CDDL — it's always-empty in practice post-Conway
but still a required wire field, decoded/encoded unconditionally as part of DState's array(4)).

## DRepState = array(4) (leaf value of vsDReps map)

Source: `libs/cardano-ledger-core/src/Cardano/Ledger/DRep.hs`. `Rec DRepState !> To drepExpiry !>
To drepAnchor !> To drepDeposit !> To drepDelegs`.

```
array(4)
  [0] drepExpiry   :: EpochNo
  [1] drepAnchor   :: StrictMaybe Anchor        -- generic `To` ⇒ DEFAULT encodeStrictMaybe:
                                                    array(0) SNothing / array(1)[anchor] SJust
  [2] drepDeposit  :: CompactForm Coin
  [3] drepDelegs   :: Set (Credential Staking)  -- NOT serialized from vsDReps' perspective on
                                                    decode-share paths but IS a real array(4)
                                                    field on the wire
```

## CommitteeState = BARE MAP, not array-wrapped — DIFFERENT from Governance's Committee type

Source: `libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs`.

```haskell
newtype CommitteeState era = CommitteeState
  { csCommitteeCreds :: Map (Credential ColdCommitteeRole) CommitteeAuthorization }
  deriving (..., EncCBOR, ...)   -- GeneralizedNewtypeDeriving: transparent pass-through
```

Wire shape: a **bare CBOR map** `{ ColdCommitteeRole credential => CommitteeAuthorization, ... }`
— NO array(1)/array(2) wrapper of any kind, because the newtype's `EncCBOR` is derived directly
from the underlying `Map`'s instance. This answers the open question in a prior investigation:
`VState.vsCommitteeState` is a **genuinely different wire shape** from the `Committee` type
embedded in `EnactState`/`ConwayGovState` (`array(2)[Map ColdCred EpochNo, UnitInterval
threshold]`) — they track different concerns (VState's CommitteeState = live
hot-authorization/resignation registry; Governance's Committee = seated members + term + quorum
from the last enactment) and share no encoding.

`CommitteeAuthorization` (map value) is a 2-constructor sum, encoded via the `Sum`/`To` coders
combinator ⇒ `array(2)[tag, field]`:
```
CommitteeHotCredential (Credential HotCommitteeRole) -> array(2)[0, hotCred]
CommitteeMemberResigned (StrictMaybe Anchor)         -> array(2)[1, anchorField]
                                                          -- anchorField uses generic `To`, so it's
                                                          -- the DEFAULT encodeStrictMaybe wrapper:
                                                          -- array(0) or array(1)[anchor], NESTED
                                                          -- inside this array(2) as element [1]
```

## PState leaf types: StakePoolState (array 10) vs StakePoolParams (array 9) — confirmed, no psDeposits field

Re-verified @ SHA a88b60bdcf3248dfe5a2f9372c188c399233f479: `PState` really is exactly
`{psVRFKeyHashes, psStakePools, psFutureStakePoolParams, psRetiring}` (array(4), no 5th
`psDeposits` field at any position — deposit lives per-pool inside `StakePoolState.spsDeposit`
instead, field [8] of that array(10)). `StakePoolParams`'s array(9) is produced by
`withStakePoolParamsFlatEncoding` (a dynamic-length flat encoder, not literally `EncCBORGroup`,
but wire-identical to a flat array(9)) — its own top-level `EncCBOR` instance wraps that in
`encodeListLen n <> ...` directly, so a standalone `StakePoolParams` value on the wire is exactly
`array(9)[id, vrf, pledge, cost, margin, accountAddress, owners, relays, metadata_null]` with
`metadata` using `encodeNullStrictMaybe` (null-or-bare, not array-wrapped).

## ConwayAccountState field [2]/[3] are `Maybe`, not `StrictMaybe` — minor correction

`casStakePoolDelegation :: Maybe (KeyHash StakePool)` and `casDRepDelegation :: Maybe DRep`
(`eras/conway/impl/.../State/Account.hs`) are genuinely `Maybe`, encoded via `encodeNullMaybe`, not
`StrictMaybe`/`encodeNullStrictMaybe` as a prior note said. Wire shape is unaffected (still
null-or-bare-value, no array wrapper) — see [[unit-strictmaybe-maybe-enccbor-wire-shapes]].
