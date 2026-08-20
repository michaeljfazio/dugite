---
name: handshake-driver-msgacceptversion-negotiated-payload
description: Handshake responder always sends the Accept-computed negotiated vData in MsgAcceptVersion, never raw local/remote echo; one shared acceptOrRefuse fn, verified at driver level
metadata:
  type: reference
---

Pinned: IntersectMBO/ouroboros-network commit `e3ecf566c76892ed6e7f946bbbd10659a47d5757`
(HEAD of `main`, fetched 2026-08-21). Path:
`ouroboros-network/framework/lib/Ouroboros/Network/Protocol/Handshake/`
{Server.hs, Client.hs, Type.hs}.

## The question this answers

Does the Handshake responder (server) put the `acceptVersion local remote`
NEGOTIATED value on the wire in `MsgAcceptVersion`, or something else (raw
local data, raw remote data)? Answer: **always the negotiated value.**

## Server.hs:29-48 — handshakeServerPeer (the whole driver)

```haskell
handshakeServerPeer codec@VersionDataCodec {encodeData, decodeData} acceptVersion query versions =
    Await $ \msg -> case msg of
      MsgProposeVersions vMap  ->
        case acceptOrRefuse codec acceptVersion versions vMap of
          Right (_, _, agreedData) | query agreedData ->
            Yield (MsgQueryReply $ encodeVersions encodeData versions)
                  (Done (Right $ decodeQueryResult decodeData vMap))

          Right (r, vNumber, agreedData) ->
            Yield (MsgAcceptVersion vNumber $ encodeData vNumber agreedData)
                  (Done (Right $ HandshakeNegotiationResult r vNumber agreedData))

          Left vReason ->
            Yield (MsgRefuse vReason)
                  (Done (Left (HandshakeError vReason)))
```

Exactly ONE branch constructs `MsgAcceptVersion`, and its payload is
`encodeData vNumber agreedData` where `agreedData` is destructured straight
out of the `Right (r, vNumber, agreedData)` pattern coming from
`acceptOrRefuse`. No other server code path sends `MsgAcceptVersion`.

## Client.hs:191-217 — acceptOrRefuse (the shared negotiation core)

Defined in Client.hs, exported, and imported into Server.hs (line 13) — so
server and client's simultaneous-open branch share this ONE function:

```haskell
acceptOrRefuse VersionDataCodec {decodeData}
               acceptVersion versions versionMap =
    case lookupGreatestCommonKey versionMap (getVersions versions) of
      Nothing -> Left $ VersionMismatch (Map.keys $ getVersions versions) []
      Just (vNumber, (vParams, Version app vData)) ->
        case decodeData vNumber vParams of
          Left err -> Left (HandshakeDecodeError vNumber err)
          Right vData' ->
            case acceptVersion vData vData' of
              Accept agreedData -> Right (app agreedData, vNumber, agreedData)
              Refuse err        -> Left (Refused vNumber err)
```

`vData` = own compiled-in `versionData` from the `Versions vNumber vData r`
map passed to the driver (LOCAL). `vData'` = peer's proposed data, decoded
off the wire (REMOTE). `acceptVersion vData vData'` is called `local remote`
— matches [[n2n-diffusionmode-dataflow-duplex-gating]]'s reading of
`acceptableVersion local remote` in `cardano-diffusion`. `agreedData` is the
`Accept` constructor's payload, threaded unmodified into the server's
`MsgAcceptVersion`.

## Type.hs:118-120 — the message shape

```haskell
MsgAcceptVersion
  :: vNumber
  -> vParams
  -> Message (Handshake vNumber vParams) StConfirm StDone
```

## The three-part answer

1. **Driver confirms it**: YES, unconditionally. `agreedData` in
   `handshakeServerPeer`'s `MsgAcceptVersion` arm is always the `Accept`
   payload, never `vData` (local, unmodified) or `vData'` (remote, unmodified).
2. **Not fully one code path.** `acceptOrRefuse` is shared by (a) the server,
   always, and (b) the client's simultaneous-open `MsgReplyVersions` handler
   (Client.hs:120-127, `$simultanous-open` doc comment: the algorithm must be
   SYMMETRIC so both sides land on the same `agreedData` independently — on
   simultaneous open NEITHER side sends `MsgAcceptVersion` at all;
   `MsgReplyVersions -> StDone` terminates locally with no reply message). But
   the client's NORMAL `MsgAcceptVersion` handler (Client.hs:147-170,
   `Accept agreedData ->` at line 163) inlines the identical
   decode/`acceptVersion vData vData'`/unwrap-`Accept` logic by hand rather
   than reusing the map-keyed `acceptOrRefuse` helper (it already has one
   decoded `vNumber`/`vParams` pair, not a `Map`). Same rule, same argument
   order, three textual call sites (`acceptOrRefuse` used twice + one inline).
3. **No real-cardano-node exception found.** cardano-node does not
   re-implement this driver — `cardano-diffusion` supplies only
   `acceptVersion`/`VersionDataCodec`/`Versions` as parameters into this
   generic `ouroboros-network` Handshake protocol. The `query` branch
   (Server.hs:38-40) sends a DIFFERENT message, `MsgQueryReply` (the full
   `versions` map, for `cardano-cli`-style handshake probing) — not a variant
   of `MsgAcceptVersion`'s payload, and not reachable unless the peer's
   `agreedData` itself signals a query (`query agreedData` predicate).

## Why this mattered

A Rust reimplementation (Dugite) responder must echo the NEGOTIATED
(min/AND/OR-combined) `NodeToNodeVersionData`/`NodeToClientVersionData` back
in `MsgAcceptVersion`, not its own raw locally-configured data and not the
peer's raw proposal. Getting this wrong is silent: the client still gets a
byte string it can decode, it just decodes to the WRONG per-connection
negotiated parameters (e.g. wrong `diffusionMode`/`peerSharing` used
downstream for `DataFlow` derivation).
