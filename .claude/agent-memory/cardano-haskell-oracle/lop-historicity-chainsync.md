---
name: lop-historicity-chainsync
description: Complete LoP leaky bucket + HistoricityCheck + idling (varIdling) in ChainSync client — ouroboros-consensus 3.0.1.0 (cardano-node 11.0.1)
metadata:
  type: reference
---

# LoP Leaky Bucket, HistoricityCheck, and Idling in ChainSync Client

Source: ouroboros-consensus tag `release-ouroboros-consensus-3.0.1.0` (SHA c87aa760001e60f0f0d3353f793eb089adb917e7), pinned by cardano-node 11.0.1.

## Key Files

- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/MiniProtocol/ChainSync/Client.hs`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/MiniProtocol/ChainSync/Client/HistoricityCheck.hs`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Util/LeakyBucket.hs`
- `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/Node/Genesis.hs`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/MiniProtocol/ChainSync/Client/State.hs`
- `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Genesis/Governor.hs`

## ChainSyncLoPBucketConfig

```haskell
data ChainSyncLoPBucketConfig
  = ChainSyncLoPBucketDisabled
  | ChainSyncLoPBucketEnabled ChainSyncLoPBucketEnabledConfig

data ChainSyncLoPBucketEnabledConfig = ChainSyncLoPBucketEnabledConfig
  { csbcCapacity :: Integer   -- tokens; default 100_000
  , csbcRate     :: Rational  -- tokens/second; default 500
  }
```

## Default Values (from mkGenesisConfig in Node/Genesis.hs)

```
defaultCapacity  = 100_000    -- tokens (= 200 seconds of patience at 500 tok/s)
defaultRate      = 500        -- tokens per second (= 1 token per 2ms)
```

Comments in source: "Empirically, it takes less than 1ms to validate a header, so leaking one token per 2ms is conservative. The capacity of 100_000 tokens corresponds to 200s, which is definitely enough to handle long GC pauses."

## GSM-Gated Activation

LoP is ONLY active when `GsmState == Syncing`. In `bracketChainSyncClient`:

```haskell
lopBucketConfig :: GsmState -> LeakyBucket.Config m
lopBucketConfig gsmState =
  case (gsmState, csBucketConfig) of
    (Syncing, ChainSyncLoPBucketEnabled
      ChainSyncLoPBucketEnabledConfig{csbcCapacity, csbcRate}) ->
        LeakyBucket.Config
          { capacity       = fromInteger csbcCapacity
          , rate           = csbcRate
          , onEmpty        = throwIO EmptyBucket
          , fillOnOverflow = True
          }
    (_, ChainSyncLoPBucketDisabled)          -> dummyConfig
    (PreSyncing, ChainSyncLoPBucketEnabled _) -> dummyConfig
    (CaughtUp,   ChainSyncLoPBucketEnabled _) -> dummyConfig
```

The config is updated dynamically via `cschOnGsmStateChanged = updateLopBucketConfig lopBucket`.

## Token Grant Condition (checkLoP)

A token is granted ONLY when the incoming header advances the peer's best-seen block number:

```haskell
checkLoP ConfigEnv{tracer} DynamicEnv{loPBucket} hdr
         kis@KnownIntersectionState{kBestBlockNo} =
  if blockNo hdr > kBestBlockNo
    then do
      lbGrantToken loPBucket
      traceWith tracer $ TraceGaveLoPToken True hdr kBestBlockNo
      pure $ kis{kBestBlockNo = blockNo hdr}
    else do
      traceWith tracer $ TraceGaveLoPToken False hdr kBestBlockNo
      pure kis
```

`lbGrantToken loPBucket = void $ LeakyBucket.fill' lopBucket 1` (adds 1 token).

Token is NOT granted for:
- Duplicate or non-advancing headers (blockNo <= kBestBlockNo)
- Invalid headers (disconnect before checkLoP)
- Future headers (caught in checkTime)
- Known-invalid blocks (caught in checkKnownInvalid)

## Bucket Pause/Resume

```
lbPause  = LeakyBucket.setPaused' lopBucket True
lbResume = LeakyBucket.setPaused' lopBucket False
```

Bucket is PAUSED when peer sends MsgAwaitReply:
```haskell
onMsgAwaitReply = do
  historicityCheck ...
  idlingStart idling    -- sets csIdling = True
  lbPause loPBucket     -- stops leaking
  ...
```

Bucket is RESUMED on MsgRollForward or MsgRollBackward:
```haskell
recvMsgRollForward = \hdr theirTip -> do
  idlingStop idling >> lbResume loPBucket
  ...
recvMsgRollBackward = \intersection theirTip -> do
  idlingStop idling >> lbResume loPBucket
  ...
```

Semantics: when paused, `leaked = 0` (no tokens drain), so the client "patiently" waits for the peer to send more without losing patience.

## Empty Bucket Exception

When bucket level reaches 0, `onEmpty = throwIO EmptyBucket` fires in the `leak` thread, which calls `throwTo actionThreadId EmptyBucket`.

`EmptyBucket` is a constructor of `ChainSyncClientException`:
```haskell
data ChainSyncClientException
  = ... | EmptyBucket | ...
```

This exception causes the ChainSync client to disconnect from that peer.

## Idling Set (varIdling / csIdling)

`csIdling :: !Bool` is a field of `ChainSyncState blk` (in Client/State.hs).

Updates via `ChainSyncStateView.csvIdling`:
```haskell
csvIdling = Idling
  { idlingStart = atomically $ modifyTVar csHandleState $ \s -> s{csIdling = True}
  , idlingStop  = atomically $ modifyTVar csHandleState $ \s -> s{csIdling = False}
  }
```

Set True: on MsgAwaitReply (peer signals no more headers).
Set False: on MsgRollForward or MsgRollBackward (peer sends a new message).

## GSM CaughtUp Transition (blockUntilCaughtUp)

GSM transitions to CaughtUp only when BOTH:
1. ALL peers are idling (`all peerIsIdle states` where `peerIsIdle = csIdling`)
2. NO candidate is better than current selection

```haskell
blockUntilCaughtUp :: STM m (TraceGsmEvent tracedSelection)
blockUntilCaughtUp = do
  varsState <- getChainSyncStates
  states <- traverse StrictSTM.readTVar varsState
  check $
    not (Map.null states)
      && all peerIsIdle states
  selection <- getCurrentSelection
  candidates <- traverse StrictSTM.readTVar varsState
  candidateOverSelection <- getCandidateOverSelection
  let ok candidate = WhetherCandidateIsBetter False == candidateOverSelection selection candidate
  check $ all ok candidates
  ...
```

## GDD DensityBounds and Idle Peers

In `densityDisconnect` (Governor.hs), idle peers are treated differently:

```haskell
guard $ lb1 >= (if idling0 then lb0 else ub0)
```

If peer0 is idling, GDD compares lb1 (peer1's lower bound) against lb0 (peer0's lower bound, a known density). If peer0 is NOT idling, GDD compares lb1 against ub0 (peer0's UPPER bound, the optimistic potential). This means an idle peer with lb0=3 can be disconnected if any other peer offers lb1>=3, whereas a non-idle peer with the same lb0=3 and ub0=10 can only be disconnected if some peer offers lb1>=10.

Guard for applying disconnection at all:
```haskell
guard $ idling0 || not (AF.null frag0) || hasBlockAfter0
```
Idle peer OR has sent at least one header after LoE OR has a block after genesis window.

`wFingerprint` only fires GDD when `(csLatestSlot, csIdling)` changes:
```haskell
GDDSyncing $
  Map.map (\css -> (csLatestSlot css, csIdling css)) gddCtxStates
```

## HistoricityCheck

### Types

```haskell
newtype HistoricityCutoff = HistoricityCutoff
  { getHistoricityCutoff :: NominalDiffTime }
```

### Default Value (mkGenesisConfig)

```haskell
gcHistoricityCutoff = Just $ HistoricityCutoff $ 3 * 2160 * 20 + 3600
-- = 129600 + 3600 = 133200 seconds = 37 hours
```

Comment: "Duration in seconds of one Cardano mainnet Shelley stability window (3k/f slots times one second per slot) plus one extra hour as a safety margin."

(k=2160, f=1/20, so 3k/f = 3*2160*20 = 129600s = 36h; +3600s = 37h total.)

### What Is Checked

`judgeMessageHistoricity` is called for:
- `HistoricalMsgRollBackward`: the oldest header that was rewound
- `HistoricalMsgAwaitReply`: the tip of the candidate fragment when MsgAwaitReply is received

The check is: `historicityCutoff < arrivalTime `diffRelTime` slotTime`

If the ARRIVAL TIME minus the SLOT TIME of the relevant header exceeds the cutoff (37h by default), the peer is disconnected with `HistoricityException`.

### GSM Gating

The check is ONLY applied in `PreSyncing` and `Syncing`. In `CaughtUp`, `noCheck` (always passes):

```haskell
judgeMessageHistoricity = \msg hswt ->
  getCurrentGsmState >>= \case
    PreSyncing -> judgeRollback msg hswt
    Syncing    -> judgeRollback msg hswt
    CaughtUp   -> pure $ Right ()
```

Rationale: "extra resilience against disconnects between honest nodes in disaster scenarios with very low chain density."

### Exception

```haskell
data HistoricityException = forall blk. HasHeader blk => HistoricityException
  { historicalMessage  :: HistoricalChainSyncMessage
  , historicalPoint    :: !(Point blk)
  , slotTime           :: !RelativeTime
  , arrivalTime        :: !RelativeTime
  , historicityCutoff  :: !HistoricityCutoff
  }
```

Wrapped in ChainSyncClientException as `HistoricityError !HistoricityException`.

### noCheck (Praos mode)

When `gcHistoricityCutoff = Nothing` (Praos / disableGenesisConfig), `HistoricityCheck.noCheck` is used which always returns `Right ()`.

## Wire-up at Node Startup

In `Ouroboros.Consensus.Node` (Node.hs):

```haskell
historicityCheck getGsmState =
  case gcHistoricityCutoff llrnGenesisConfig of
    Nothing -> HistoricityCheck.noCheck
    Just historicityCutoff ->
      HistoricityCheck.mkCheck systemTime getGsmState historicityCutoff
```

`gcChainSyncLoPBucketConfig llrnGenesisConfig` is passed directly to `NTN.mkApps`.

## LoE Fragment GSM Gating (setGetLoEFragment)

```haskell
getLoEFragment =
  atomically $ readGsmState >>= \case
    GSM.PreSyncing -> pure $ ChainDB.LoEEnabled $ AF.Empty AF.AnchorGenesis
    GSM.Syncing    -> ChainDB.LoEEnabled <$> readLoEFragment
    GSM.CaughtUp   -> pure ChainDB.LoEDisabled
```

- PreSyncing: most conservative fragment (anchored at genesis, no blocks) — blocks all chain selection advancement
- Syncing: actual GDD-computed LoE fragment
- CaughtUp: LoE disabled — normal Praos chain selection

**Why:** `loE-chain-selection.md` has full details.
