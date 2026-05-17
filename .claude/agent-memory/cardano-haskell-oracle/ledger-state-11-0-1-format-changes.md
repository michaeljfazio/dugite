---
name: ledger-state-11-0-1-format-changes
description: Breaking ExtLedgerState CBOR format changes between cardano-node 10.6.2 and 11.0.1 — complete list for dugite decode_state_file
type: project
---

## Root Cause of decode_state_file Failure on 11.0.1 Snapshots

### Change 1: StakePoolState.spsAccountId encoding (FIXED in prior session)

**File**: `libs/cardano-ledger-core/src/Cardano/Ledger/State/StakePool.hs`
**Commit**: `71b57dd6` (2026-02-19) — after cardano-node 10.6.2 (2026-02-12)

- 10.6.2: `spsAccountAddress: AccountAddress` → `bytes(29)` (1-byte header + 28-byte credential)
- 11.0.1: `spsAccountId: AccountId = Credential Staking` → `array(2)[0|1, bytes(28)]`

`AccountId` in `libs/cardano-ledger-core/src/Cardano/Ledger/Address.hs`:
```haskell
newtype AccountId = AccountId {unAccountId :: Credential Staking}
  deriving newtype (NFData, NoThunks, ToJSON, FromJSON, EncCBOR, DecCBOR)
```
`AccountId` is a newtype that derives `EncCBOR` directly from `Credential Staking`, which encodes as `array(2)[0|1, bytes(28)]`.

**Status in dugite**: FIXED in certstate.rs (major-4 dispatch at field [4]).

### Change 2: SnapShots — SnapShotPool format in pool snapshot VMap (CURRENT FAILURE)

**Files**: `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs`
**Commits**:
- `f914065` (2026-02-18): removed `ssPoolParams :: VMap (KeyHash StakePool) StakePoolParams` from SnapShot; changed `encodeListLen 4→3`
- `ef97b72` (2026-03-04): merged PR #5585 "Subsume delegations into activestake"; removed `ssDelegations`; changed `encodeListLen 3→2`; changed `ssActiveStake` type from `Stake` to `ActiveStake`

**Old format (10.6.2)**:
```
SnapShot = array(4) [
  Stake:       VMap (Credential Staking) (CompactForm Coin)  -- uint values
  Delegations: VMap (Credential Staking) (KeyHash StakePool) -- bytes(28) values
  PoolParams:  VMap (KeyHash StakePool)  StakePoolParams      -- 9-field array values
  PoolSnaps:   VMap (KeyHash StakePool)  StakePoolSnapShot    -- 10-field array values
]
```

**New format (11.0.1)**:
```
SnapShot = array(2) [
  ActiveStake: VMap (Credential Staking) StakeWithDelegation  -- array(2)[uint, bytes(28)] values
  PoolSnaps:   VMap (KeyHash StakePool)  StakePoolSnapShot     -- 10-field array values
]
```

Backward compat: decoder accepts n=2 (new) or n=3 (old intermediate, with Stake+Delegations separate).
**No support for n=4** (the 10.6.2 format with PoolParams). This means 10.6.2 state files cannot
be read by 11.0.1 Haskell node, but 11.0.1 files are what dugite is now trying to read.

**StakeWithDelegation encoding**:
```
StakeWithDelegation = array(2) [
  NonZero (CompactForm Coin) = uint,   -- non-zero stake lovelace
  KeyHash StakePool          = bytes(28)
]
```

**StakePoolSnapShot encoding (10-field, Conway version ≥ 9)**:
```
StakePoolSnapShot = array(10) [
  [0] spssStake:                  CompactForm Coin = uint
  [1] spssStakeRatio:             Rational = tag(30) array(2)[int, int]  ← TAG(30) = 0xd8 0x1e
  [2] spssSelfDelegatedOwners:    Set (KeyHash Staking) = tag(258) varlen-array([bytes(28)])
  [3] spssSelfDelegatedOwnersStake: Coin = uint
  [4] spssVrf:                    VRFVerKeyHash = bytes(32)
  [5] spssPledge:                 Coin = uint
  [6] spssCost:                   Coin = uint
  [7] spssMargin:                 UnitInterval = tag(30) array(2)[num, den]
  [8] spssNumDelegators:          Int = uint
  [9] spssAccountId:              AccountId = array(2)[0|1, bytes(28)]
]
```

**Dugite's CURRENT failure** (after fixing Change 1):
`snapshots.rs::decode_snapshot_pool` still uses OLD StakePoolParams layout:
- Expects [0] = `pool_id: bytes(28)` → skips via `skip_cbor_value` (reads `uint` for `spssStake` — OK)
- Expects [1] = `vrf_hash: bytes(32)` → calls `decode_hash32` — **FAILS** because actual [1] is
  `spssStakeRatio = tag(30) array(2)[...]` = byte `0xd8` = major 6 (tag), not major 2 (bytes)

This is the exact "expected bytes (major 2), got major 6 at byte 0xd8" error.

**Fix required**: Replace `decode_snapshot_pool` in `snapshots.rs` with a new decoder for
`StakePoolSnapShot` (10-field layout):
```rust
fn decode_stake_pool_snapshot(data: &[u8]) -> Result<(HaskellSnapShotPool, usize), SerializationError> {
    // array(10):
    // [0] spssStake:                  uint
    // [1] spssStakeRatio:             tag(30) array(2)[int, int]  -- skip or store
    // [2] spssSelfDelegatedOwners:    tag(258) varlen-array([bytes(28)])  -- skip or decode
    // [3] spssSelfDelegatedOwnersStake: uint
    // [4] spssVrf:                    bytes(32)
    // [5] spssPledge:                 uint
    // [6] spssCost:                   uint
    // [7] spssMargin:                 tag(30) array(2)[int, int]
    // [8] spssNumDelegators:          uint
    // [9] spssAccountId:              array(2)[0|1, bytes(28)]
}
```

Note: `spssSelfDelegatedOwners` at [2] uses `tag(258)` (= `0xd9 0x01 0x02`) because
at Conway version ≥ 9, `encodeSet` ADDS tag(258). The set is then followed by a variable-
length array. `tag(258)` = major 6, additional info 25 (two-byte argument `0x01 0x02`).
The existing `skip_set_tag` helper in both certstate.rs and snapshots.rs handles this correctly.

### Change 3: SnapShots — SnapShot array length gating

No additional decoder change needed: `decode_snapshot` in snapshots.rs already handles n=2 (new)
and n=3 (old intermediate). The n=4 format from 10.6.2 is NOT handled but 11.0.1 writes n=2.

### Change 4: ShelleyLedgerState Peras field (already handled)

**File**: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Ledger.hs`
**Commit**: `39ad1e9d` (2026-02-06)

- 10.6.2: `encodeVersion 2 $ array(3)[tip, NewEpochState, transition]`
- 11.0.1: `encodeVersion 2 $ array(4)[tip, NewEpochState, transition, StrictMaybe(PerasRoundNo)]`

Dugite already handles array(3|4) — not the failing cause.

### Change 5: ConwayAccountState StrictMaybe→Maybe (no wire format change)

**Commit**: `0cfbf861` (2026-04-14)
`casStakePoolDelegation` changed from `StrictMaybe (KeyHash StakePool)` to `Maybe (KeyHash StakePool)`.
Both `StrictMaybe` and `Maybe` encode identically via `encodeNullMaybe`: Nothing/SNothing = CBOR null
(`0xf6`), Just/SJust x = x. **No wire format change.**

### Change 6: psVRFKeyHashes — new field in PState

**Commit**: predates 10.6.2 (was `da534151` 2025-11-04)
PState already had 4 fields in 10.6.2. No change between releases.

### Change 7: PoolMetadata.pmHash type change (ByteString → ByteArray)

**File**: `libs/cardano-ledger-core/src/Cardano/Ledger/State/StakePool.hs`
**Commit**: `January 8, 2026` (before 10.6.2) — `PoolMetadata.pmHash` changed to `ByteArray`.
`ByteArray` and `ByteString` both encode as CBOR `bytes`. **No wire format change.**

### Change 8: SnapShotPool VMap key

The `ssStakePoolsSnapShot` VMap key is `KeyHash StakePool = bytes(28)`. This has NOT changed.
The change is ONLY in the VALUE type (StakePoolSnapShot 10-field vs StakePoolParams 9-field).

## Summary: All Dugite Fixes Required

1. **certstate.rs::decode_stake_pool_state field [4]** — DONE: dispatch bytes(29) vs array(2) for spsAccountId
2. **snapshots.rs::decode_snapshot_pool** — NEW FAILURE: must be replaced with StakePoolSnapShot (10-field) decoder
   - Field layout completely different from StakePoolParams (9-field)
   - pool_id is NO LONGER in the value (it's only in the map key)
   - spssStakeRatio at [1] = tag(30) = source of the 0xd8 failure
3. **snapshots.rs::decode_snapshot_new pool map** — must call new `decode_stake_pool_snapshot` instead of `decode_snapshot_pool`

## No Other Format Changes Between 10.6.2 and 11.0.1

Examined and confirmed unchanged:
- `PState` array(4) structure: unchanged
- `DState` / `ConwayAccountState` wire format: unchanged
- `VState` array(3) structure: unchanged
- `CertState` array(3) structure: unchanged (VState, PState, DState order)
- `psFutureStakePoolParams` values: still `StakePoolParams` (9-field CBORGroup)
- `PraosState`: unchanged (8-field, already handled)
- `NewEpochState` 7-field structure: unchanged
- `EpochState` 4-field structure: unchanged
- `LedgerState` 2-field (CertState, UTxOState): unchanged
- Nonce encoding: unchanged
- Telescope structure: unchanged
- `GovState`/`ConwayGovState`: unchanged between these releases

## Source Files

- `libs/cardano-ledger-core/src/Cardano/Ledger/State/StakePool.hs` — StakePoolState, StakePoolParams
- `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` — SnapShot, SnapShots, StakePoolSnapShot
- `libs/cardano-ledger-core/src/Cardano/Ledger/Address.hs` — AccountId newtype
- `libs/cardano-ledger-core/src/Cardano/Ledger/Binary/Encoding/Encoder.hs` — encodeSet (tag(258) at version≥9), encodeRatio (tag(30) at version≥9)
