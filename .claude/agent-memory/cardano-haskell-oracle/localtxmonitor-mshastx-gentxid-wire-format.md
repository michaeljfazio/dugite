---
name: localtxmonitor-mshastx-gentxid-wire-format
description: Exact MsgHasTx/MsgReplyHasTx CBOR wire shape for LocalTxMonitor N2C + full GenTxId (HFC) encoding chain that produces it
metadata:
  type: reference
---

## MsgHasTx / MsgReplyHasTx wire shape (verified against cardano-node 11.0.1 pin)

Source: `Ouroboros.Network.Protocol.LocalTxMonitor.Codec.codecLocalTxMonitor`,
repo IntersectMBO/ouroboros-network, path
`ouroboros-network/protocols/lib/Ouroboros/Network/Protocol/LocalTxMonitor/Codec.hs`.
Verified byte-identical (core encode/decode clauses) at commit `17525c3`
(the exact `cardano-diffusion` rev [[haa-outbound-connections-state-verified]]
already pinned as matching cardano-node 11.0.1's CHaP dependency) and at
master (blob `cd7aabd3`) — the two only differ in which error-formatting
library the fallback `fail` case uses (`Text.Printf` vs `Formatting`), never
in wire shape.

```haskell
MsgHasTx txid ->
  CBOR.encodeListLen 2 <> CBOR.encodeWord 7 <> encodeTxId txid
...
MsgReplyHasTx has ->
  CBOR.encodeListLen 2 <> CBOR.encodeWord 8 <> CBOR.encodeBool has
```//decode side requires len==2 for key 7 (`(SingAcquired, 2, 7)`), so a bare
`[7, bstr]` (len 2 but with a bstr second element) still passes the outer
list-length check — the failure happens one level down, inside `encodeTxId`'s
own decoder, which expects an array not a bstr.

**`encodeTxId`/`decodeTxId` are parameters**, not fixed in this module — the
real instantiation is in `ouroboros-consensus-diffusion/.../Ouroboros/Consensus/Network/NodeToClient.hs`
(`defaultCodecs`/`clientCodecs`, `cTxMonitorCodec = codecLocalTxMonitor networkVersion enc dec enc dec enc dec`)
where `enc = encodeNodeToClient ccfg version`, dispatching on
`SerialiseNodeToClient (HardForkBlock xs) (GenTxId (HardForkBlock xs))`.

## GenTxId encoding chain for `CardanoBlock StandardCrypto`

1. `ouroboros-consensus/.../HardFork/Combinator/Serialisation/SerialiseNodeToClient.hs`:
   ```haskell
   instance SerialiseHFC xs => SerialiseNodeToClient (HardForkBlock xs) (GenTxId (HardForkBlock xs)) where
     encodeNodeToClient = dispatchEncoder `after` (getOneEraGenTxId . getHardForkGenTxId)
   ```
   `dispatchEncoder` (same file) calls `encodeNS` on the `NS WrapGenTxId xs` sum
   when the HFC is enabled (post-Byron, i.e. always in practice).
2. `encodeNS` — `ouroboros-consensus/.../HardFork/Combinator/Serialisation/Common.hs:412-418`:
   ```haskell
   encodeNS :: SListI xs => NP (f -.-> K Encoding) xs -> NS f xs -> Encoding
   encodeNS es ns =
     mconcat
       [ Enc.encodeListLen 2
       , Enc.encodeWord8 $ nsToIndex ns
       , hcollapse $ hzipWith apFn es ns
       ]
   ```
   So the `OneEraGenTxId` wire shape is **`[era_index :: word8, <one CBOR item: the per-era GenTxId encoding>]`** — a 2-element array, NOT wrapped in tag(24) (contrast with `GenTx`/block bodies, which DO go through `wrapCBORinCBOR` because they must preserve exact original bytes for hashing; a `GenTxId` already IS a hash, so there's nothing to preserve-by-reference).
3. Per-era: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Node/Serialisation.hs`:
   ```haskell
   instance ... SerialiseNodeToClient (ShelleyBlock proto era) (GenTxId (ShelleyBlock proto era)) where
     encodeNodeToClient _ _ = toEraCBOR @era
   ```
   `toEraCBOR = toPlainEncoding (eraProtVerLow @era) . encCBOR` (cardano-ledger
   `Cardano.Ledger.Core.Era`) — a bare `encCBOR` call, no extra wrapper.
4. `GenTxId (ShelleyBlock proto era)` (i.e. `TxId (GenTx (ShelleyBlock proto era))`)
   = `newtype ShelleyTxId SL.TxId`, deriving `EncCBOR`/`DecCBOR` newtype straight
   from ledger `Cardano.Ledger.TxIn.TxId` (`ouroboros-consensus-cardano/.../Shelley/Ledger/Mempool.hs:217-225`).
5. Ledger `TxId = TxId { unTxId :: SafeHash EraIndependentTxBody }` derives
   `EncCBOR` newtype from `SafeHash`, which derives it newtype from
   `Hash.Hash HASH i` (`cardano-ledger-core/.../TxIn.hs:59-61`,
   `.../Hashes.hs:337-350`) — i.e. a **plain `bstr(32)`**, CBOR `58 20 <32 bytes>`,
   the blake2b-256 hash bytes with zero framing beyond the byte-string header.

`CardanoEras c` index (`ouroboros-consensus-cardano/.../Cardano/Block.hs:245-257`):
Byron=0, Shelley=1, Allegra=2, Mary=3, Alonzo=4, Babbage=5, **Conway=6**,
Dijkstra=7 (matches [[localtxmonitor-localtxsubmission-gentx-n2c-wire-format]]).

## Full byte-exact shape, Conway

```
82 07 82 06 58 20 <32-byte-txid>
^^ ^^ ^^ ^^ ^^^^^^
|  |  |  |  bstr(32) header + hash bytes  (TxId's plain EncCBOR)
|  |  |  era index 6 = Conway              (encodeWord8, single byte)
|  |  array(2)                             (OneEraGenTxId / encodeNS wrapper)
|  message tag 7 = MsgHasTx
array(2)                                    (outer LocalTxMonitor message envelope)
```
38 bytes total (6 header bytes + 32 hash bytes).

`MsgReplyHasTx`: `82 08 f4` (False) / `82 08 f5` (True) — `[8, bool]`,
CBOR simple-value booleans, nothing else.

**Independent confirmation**: dugite's own in-progress fix (uncommitted as of
2026-08-02, `crates/dugite-network/src/protocol/local_tx_monitor/server.rs`)
pins a real packet captured off the wire from `cardano-cli 11.0.1 conway
query tx-mempool tx-exists`:
`8207820658200bece18e734ce8e83f662dfa904925ce25a695b8999e3d6a7ff2539e0efe5482`
→ node replies `8208f4`. Decodes byte-for-byte per the derivation above
(era index `06` = Conway, `5820` + 32 bytes = the txid). This is the exact
shape this memory documents, derived independently from source before the
capture was consulted.

## Version gating (NodeToClientVersion)

The wrapper `Cardano.Network.Protocol.LocalTxMonitor.Codec.codecLocalTxMonitor`
(`cardano-diffusion/protocols/lib/Cardano/Network/Protocol/LocalTxMonitor/Codec.hs`)
picks `LocalTxMonitor_V1` for `version < NodeToClientV_20`, else `V2`. That
flag (`canHandleMeasures`) gates **only** `MsgGetMeasures`/`MsgReplyGetMeasures`
(added at N2C v20). `MsgHasTx`/`MsgReplyHasTx` are NOT version-gated at all —
same 2-element-array shape on every NodeToClientVersion that has the
LocalTxMonitor mini-protocol.

## Decode-failure behavior (answers "what should happen on a malformed message")

The `LocalTxMonitor` protocol grammar (`Type.hs`) has no error/reject message
and no error state — just Idle/Acquiring/Acquired/Busy/Done. There is
structurally no way to "reply with an error." Instead,
`Ouroboros.Network.Driver.Simple.recvMessage` (`framework/lib/Ouroboros/Network/Driver/Simple.hs`)
does:
```haskell
Left failure -> throwIO (DecoderFailure tok failure)
```
`DecoderFailure` is a real `Exception` instance. It propagates out of the
mini-protocol's peer loop and is caught by mux, which tears down the bearer —
since N2C multiplexes ChainSync/LocalTxSubmission/LocalStateQuery/LocalTxMonitor
over ONE Unix-domain-socket connection, a malformed `MsgHasTx` kills the whole
local socket, not just that one query. **Correct behavior is "close the
connection / raise a protocol error," never "silently drop and never reply"**
— the latter (dugite's pre-fix behavior) has no Haskell analogue at all and
is strictly worse (client hangs forever instead of getting an immediate
ECONNRESET/EOF).

## Repos/commits consulted
- IntersectMBO/ouroboros-network — blob `cd7aabd3` (master) + commit `17525c3` (cn 11.0.1 pin) for Codec.hs/Type.hs; blob `a6f9fe5a` for the `cardano-diffusion` version-dispatch wrapper.
- IntersectMBO/ouroboros-consensus — blobs `1134ff70` (NodeToClient.hs), `d042aa16` (HFC SerialiseNodeToClient.hs), `23839c8c` (HFC Serialisation/Common.hs), `e0aa6b12` (Shelley Node/Serialisation.hs), `edd56e11` (Shelley Ledger/Mempool.hs), `222f6ace` (Cardano/Block.hs).
- IntersectMBO/cardano-ledger — blobs `28e825e1` (TxIn.hs), `8d7b1b5e` (Hashes.hs), `8ab7930f` (Core/Era.hs).
