---
name: localtxmonitor-localtxsubmission-gentx-n2c-wire-format
description: Byte-exact CBOR wire format for LocalTxMonitor MsgReplyNextTx and LocalTxSubmission MsgSubmitTx GenTx embedding on N2C, incl. CBOR-in-CBOR tag-24 wrap and era-index table
type: reference
---

## Repo layout (current, post-restructure)

- `ouroboros-network` repo: mini-protocol codecs live under
  `ouroboros-network/protocols/lib/Ouroboros/Network/Protocol/{LocalTxMonitor,LocalTxSubmission}/Codec.hs`
  (there is no longer a separate `ouroboros-network-protocols` repo — it's a
  package/component inside the monorepo `ouroboros-network`).
  `wrapCBORinCBOR`/`unwrapCBORinCBOR`/`Serialise (Serialised a)` live in
  `ouroboros-network/api/lib/Ouroboros/Network/Block.hs`.
- `ouroboros-consensus` repo: HFC generic `SerialiseNodeToClient` instances in
  `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/HardFork/Combinator/Serialisation/SerialiseNodeToClient.hs`.
  Cardano-specific `BlockNodeToClientVersion` patterns in
  `ouroboros-consensus-cardano/src/ouroboros-consensus-cardano/Ouroboros/Consensus/Cardano/Node.hs`.
  Shelley-based era GenTx `ToCBOR`/`SerialiseNodeToClient` instances in
  `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/{Mempool.hs,Node/Serialisation.hs}`.
  Byron GenTx encode in `ouroboros-consensus-cardano/src/byron/Ouroboros/Consensus/Byron/Ledger/Mempool.hs`.
  Codec assembly (which encoder gets passed to each mini-protocol codec) in
  `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/Network/NodeToClient.hs`
  (`defaultCodecs`/`clientCodecs`).

## LocalTxMonitor.Codec — MsgReplyNextTx (verbatim, `LocalTxMonitor/Codec.hs`)

```haskell
MsgReplyNextTx Nothing ->
  CBOR.encodeListLen 1 <> CBOR.encodeWord 6
MsgReplyNextTx (Just tx) ->
  CBOR.encodeListLen 2 <> CBOR.encodeWord 6 <> encodeTx tx
```
So: absent = `81 06`; present = `82 06 <encodeTx-tx>`. Tag word is 6 in BOTH
cases (array length distinguishes presence, not a different tag word).
Confirms array-of-1 `[6]` (absent) vs array-of-2 `[6, tx]` (present) exactly.

## LocalTxSubmission.Codec — MsgSubmitTx (verbatim)

```haskell
encode (MsgSubmitTx tx) = CBOR.encodeListLen 2 <> CBOR.encodeWord 0 <> encodeTx tx
```
`82 00 <encodeTx-tx>`.

## Codec assembly — same `encodeTx` function for both protocols

In `NodeToClient.hs` `defaultCodecs`/`clientCodecs`:
```haskell
cTxSubmissionCodec = codecLocalTxSubmission enc dec enc dec
cTxMonitorCodec    = codecLocalTxMonitor networkVersion enc dec enc dec enc dec
  where enc = encodeNodeToClient ccfg version  -- SerialiseNodeToClient blk a => a -> Encoding
```
Both mini-protocols pass the exact same `enc = encodeNodeToClient ccfg version`
polymorphic function for the `tx :: GenTx blk` parameter. **MsgSubmitTx and
MsgReplyNextTx embed byte-identical tx CBOR** — confirmed, not inferred.

## HFC layer: `SerialiseNodeToClient (HardForkBlock xs) (GenTx (HardForkBlock xs))`

`SerialiseNodeToClient.hs`:
```haskell
instance SerialiseHFC xs => SerialiseNodeToClient (HardForkBlock xs) (GenTx (HardForkBlock xs)) where
  encodeNodeToClient = dispatchEncoder `after` (getOneEraGenTx . getHardForkGenTx)
```
`dispatchEncoder`, when `HardForkNodeToClientEnabled` (the mode used by every
currently-supported `CardanoNodeToClientVersion` — checked `Cardano/Node.hs`,
ALL patterns use `HardForkNodeToClientEnabled`, never `Disabled`, for the
CardanoBlock N2C versions in current use):
```haskell
(_, HardForkNodeToClientEnabled _ versions, _) -> encodeNS (hczipWith pSHFC aux ccfgs versions) ns
```
`encodeNS` (`HardFork/Combinator/Serialisation/Common.hs:412-418`):
```haskell
encodeNS es ns = mconcat [ Enc.encodeListLen 2, Enc.encodeWord8 $ nsToIndex ns, hcollapse $ hzipWith apFn es ns ]
```
i.e. **`array(2)[era_index_u8, per-era-encoded-tx]`. NO tag-24 wrap at this
HFC dispatch layer** — that's a distinct, separate wrap that happens one
level deeper (see below). `HardForkNodeToClientDisabled` (bare single-era,
array-less form) is legacy/bootstrap-only and not relevant to current N2C
versions.

## Per-era layer: Shelley-based era GenTx IS CBOR-in-CBOR wrapped

`Shelley/Node/Serialisation.hs`:
```haskell
-- | Uses CBOR-in-CBOR in the @To/FromCBOR@ instances to get the annotation.
instance ShelleyCompatible proto era => SerialiseNodeToClient (ShelleyBlock proto era) (GenTx (ShelleyBlock proto era)) where
  encodeNodeToClient _ _ = toCBOR
  decodeNodeToClient _ _ = fromCBOR
```
The wrap is baked into the `ToCBOR` instance itself, `Shelley/Ledger/Mempool.hs:249-252`:
```haskell
instance ShelleyCompatible proto era => ToCBOR (GenTx (ShelleyBlock proto era)) where
  toCBOR (ShelleyTx _txid tx) = wrapCBORinCBOR toCBOR tx
```
`wrapCBORinCBOR`/`Serialise (Serialised a)` (`ouroboros-network/api/lib/Ouroboros/Network/Block.hs:454-455,495-499`):
```haskell
wrapCBORinCBOR enc = encode . mkSerialised enc   -- mkSerialised enc = Serialised . toLazyByteString . enc
instance Serialise (Serialised a) where
  encode (Serialised bs) = mconcat [ Enc.encodeTag 24, Enc.encodeBytes (Lazy.toStrict bs) ]
```
So per-era Shelley+ GenTx = `tag(24) bstr(<toCBOR tx bytes>)` = `D8 18 <bstr-header><tx-cbor-bytes>`.

Byron is DIFFERENT: `encodeByronGenTx genTx = toByronCBOR (toMempoolPayload genTx)`
(`Byron/Ledger/Mempool.hs:310-311`) — this is Byron's own internal
`AMempoolPayload` tagged-union encoding (tag 0=tx/1=proposal/2=vote), NOT a
CBOR-in-CBOR/tag-24 wrap. CBOR-in-CBOR-at-the-GenTx-layer is Shelley-and-later
specific.

## Full byte layout — MsgReplyNextTx, Conway tx present

```
82 06          -- array(2), word 6 = LocalTxMonitor MsgReplyNextTx tag
   82 06       -- array(2), word8 6 = era index for Conway (HFC encodeNS)
      D8 18    -- tag(24), CBOR-in-CBOR wrap (Shelley-based-era GenTx ToCBOR)
      <bstr-len-prefix> <Conway-Tx-toCBOR-bytes>
```
i.e. `82 06 82 06 D8 18 <bstr-len> <tx-cbor-bytes>` — confirms the tag-24
hypothesis exactly, PRECISELY at the per-era layer (not the HFC array layer).
Note the "6 6" is a coincidence: outer 6 = MsgReplyNextTx message key
(LocalTxMonitor protocol constant), inner 6 = Conway's 0-based era index in
CardanoEras — unrelated numbers that happen to collide.

## CardanoEras era-index table (0-based, `Cardano/Block.hs:245-257`)

```haskell
type CardanoEras c = ByronBlock ': CardanoShelleyEras c
type CardanoShelleyEras c =
  '[ ShelleyBlock (TPraos c) ShelleyEra   -- 1
   , ShelleyBlock (TPraos c) AllegraEra   -- 2
   , ShelleyBlock (TPraos c) MaryEra      -- 3
   , ShelleyBlock (TPraos c) AlonzoEra    -- 4
   , ShelleyBlock (Praos c) BabbageEra    -- 5
   , ShelleyBlock (Praos c) ConwayEra     -- 6
   , ShelleyBlock (Praos c) DijkstraEra   -- 7
   ]
```
Byron=0, Shelley=1, Allegra=2, Mary=3, Alonzo=4, Babbage=5, Conway=6,
**Dijkstra=7 DOES exist in current main-branch source** (future-era
placeholder already scaffolded ahead of an actual Dijkstra hard fork). All
current `CardanoNodeToClientVersionNN` patterns (checked V17-19+ in
`Cardano/Node.hs`) enable all 8 eras via `EraNodeToClientEnabled`.

## Cross-reference

See [[txsubmission2-wire-format]] for the UNRELATED N2N TxSubmission2
mini-protocol wire format — that one governs node-to-node tx propagation and
uses a completely different codec/module (`Ouroboros.Network.Protocol.TxSubmission2`)
with different message shapes; do not conflate with this N2C LocalTxMonitor/
LocalTxSubmission format.
