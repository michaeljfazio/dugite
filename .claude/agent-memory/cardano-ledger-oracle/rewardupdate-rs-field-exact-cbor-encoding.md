---
name: rewardupdate-rs-field-exact-cbor-encoding
description: RewardUpdate.rs is Map (Credential Staking) (Set Reward), NOT Map Cred Reward. RewardUpdate's EncCBOR instance is hand-written (not the Rec/To DSL), so `rs` inherits the plain generic EncCBOR Map/Set instances verbatim — meaning each per-credential Set Reward IS tag-258-wrapped at PV>=9, same convention as every other cardano-ledger Set. Live-verified 2026-08-20 against cardano-node 11.0.1's exact pinned commits.
metadata:
  type: reference
---

## The question this resolves
A prior research pass in this session had "conflicting signals" on whether
`RewardUpdate.rs`'s value type is `Set Reward` or a single aggregated
`Reward`, and whether it's tag-258-wrapped. Resolved by reading the actual
source, not inferring from a sibling type.

## Pinned commits (cardano-node 11.0.1)
Per [[nonmyopic-leaderprobability-precision-and-float-cbor]]'s per-package
CHaP pinning method:
- `cardano-ledger-shelley` 1.18.1.0 — rev `b7c17cf31871062b7883c46e3f367cb5e1b5db6c`
- `cardano-ledger-core` 1.20.0.0 / `cardano-ledger-binary` 1.8.1.0 — rev
  `94e9618c91a16ec08db477632a158b630722089b`

## 1. `rs`'s exact type — verbatim source
`eras/shelley/impl/src/Cardano/Ledger/Shelley/RewardUpdate.hs:111-119`
(pinned `b7c17cf3`):
```haskell
type RewardEvent = Map (Credential Staking) (Set Reward)

data RewardUpdate = RewardUpdate
  { deltaT :: !DeltaCoin
  , deltaR :: !DeltaCoin
  , rs :: !(Map (Credential Staking) (Set Reward))
  , deltaF :: !DeltaCoin
  , nonMyopic :: !NonMyopic
  }
  deriving (Show, Eq, Generic)
  deriving (ToJSON) via KeyValuePairs RewardUpdate
```
**`rs :: Map (Credential Staking) (Set Reward)`, CONFIRMED.** NOT a single
aggregated `Reward` per credential — a member reward and a leader reward
credited to the SAME credential in the SAME epoch both land as two elements
of one `Set Reward`. (Practically: `Set.singleton` is used at the leaf
`rewardStakePoolMember`/`collectLRs` call sites — see
[[reward-maturity-mark-set-go-timeline]] — so cardinality per credential is
small, almost always 1-2, but the TYPE has no such bound.)

## 2. `EncCBOR RewardUpdate` — hand-written, NOT the `Rec`/`To` combinator DSL
Same file, lines 125-132:
```haskell
instance EncCBOR RewardUpdate where
  encCBOR (RewardUpdate dt dr rw df nm) =
    encodeListLen 5
      <> encCBOR dt
      <> encCBOR (invert dr) -- TODO change Coin serialization to use integers?
      <> encCBOR rw
      <> encCBOR (invert df) -- TODO change Coin serialization to use integers?
      <> encCBOR nm
```
Unlike its sibling `RewardSnapShot`/`FreeVars` in the SAME file (which use
`encode (Rec RewardSnapShot !> To fees !> To ver !> ...)`), `RewardUpdate`'s
encoder is a manually written `encodeListLen 5 <> encCBOR f1 <> ...` chain.
This matters only insofar as it means `rs` gets NO special-cased treatment —
`encCBOR rw` dispatches through plain typeclass resolution to whatever the
library's generic `EncCBOR (Map k v)` instance does. There is no bypass, no
custom `Map _ (Set Reward)` instance, no version-gating written specifically
for `RewardUpdate`.

`DecCBOR` (same shape) decodes the two `DeltaCoin` fields and re-inverts them
(the wire carries `invert dr`/`invert df`, i.e. the NEGATED delta) —
`nm <- decNoShareCBOR` for `nonMyopic` specifically (no sharing).

## 3. The generic `EncCBOR (Map k v)` / `EncCBOR (Set a)` instances
`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/EncCBOR.hs`
(pinned `94e9618c`), lines 315-319:
```haskell
instance (EncCBOR k, EncCBOR v) => EncCBOR (Map.Map k v) where
  encCBOR = encodeMap encCBOR encCBOR

instance EncCBOR a => EncCBOR (Set.Set a) where
  encCBOR = encodeSet encCBOR
```
So `rs :: Map (Credential Staking) (Set Reward)` encodes as: outer `Map`
via `encodeMap`, whose VALUE encoder is `encCBOR :: Set Reward -> Encoding`
= `encodeSet encCBOR` (the SAME per-credential `Set Reward` machinery every
other cardano-ledger `Set` field uses).

## 4. `encodeMap` — NO tag, ever. Threshold-23, PV>=2 gate only
`Encoder.hs` (pinned `94e9618c`), lines 390-431:
```haskell
encodeMap encodeKey encodeValue m =
  let mapEncoding = Map.foldMapWithKey (\k v -> encodeKey k <> encodeValue v) m
   in ifEncodingVersionAtLeast
        (natVersion @2)
        (variableMapLenEncoding (Map.size m) mapEncoding)
        (exactMapLenEncoding (Map.size m) mapEncoding)

variableMapLenEncoding len contents =
  if len <= lengthThreshold   -- lengthThreshold = 23
    then exactMapLenEncoding len contents      -- definite: encodeMapLen n
    else encodeMapLenIndef <> contents <> encodeBreak  -- indefinite
```
Zero `encodeTag` calls anywhere in `encodeMap`'s body — **Map NEVER gets a
CBOR tag in cardano-ledger-binary**, at any PV. Only the definite-vs-
indefinite threshold-23 split is version-gated (PV>=2 variable, PV<2 always
exact/definite) — this is the SAME `#938`-class threshold this repo has
already found missing at other call sites (LSQ pparams reply, block/tx
encoders); it applies here too, at the OUTER `rs` map, for any epoch with
>23 reward-earning credentials (i.e. essentially every real-network epoch).

## 5. `encodeSet` — the tag-258 machinery, full 3-way PV gate
`Encoder.hs` lines 472-484:
```haskell
-- | Encode a Set. Versions variance:
-- * [>= 9] - Variable length encoding for Sets larger than 23 elements,
--   otherwise exact length encoding. Prefixes with a special 258 `setTag`.
-- * [>= 2] - Variable length encoding for Sets larger than 23 elements,
--   otherwise exact length encoding
-- * [< 2] - Variable length encoding. Prefixes with a special 258 `setTag`.
encodeSet :: (a -> Encoding) -> Set.Set a -> Encoding
encodeSet encodeValue f =
  let foldableEncoding = foldMap' encodeValue f
      varLenSetEncoding = variableListLenEncoding (Set.size f) foldableEncoding
   in ifEncodingVersionAtLeast
        (natVersion @2)
        ( ifEncodingVersionAtLeast
            (natVersion @9)
            (encodeTag setTag <> varLenSetEncoding)   -- PV >= 9
            varLenSetEncoding                          -- PV in [2,9)
        )
        (encodeTag setTag <> exactListLenEncoding (Set.size f) foldableEncoding) -- PV < 2
```
`setTag :: Word; setTag = 258`
(`libs/cardano-ledger-binary/.../Decoding/Decoder.hs:868-869`, same pin).

**Full 3-regime table for `Set a` (applies verbatim to each credential's
`Set Reward` inside `rs`):**

| PV | tag 258? | length framing |
|---|---|---|
| < 2 | YES | always exact/definite (no indefinite variant at all) |
| 2 <= PV < 9 | NO | threshold-23: definite <=23, indefinite >23 |
| **>= 9** (current mainnet/preprod/preview, cardano-node 11.0.1) | **YES** | threshold-23: definite <=23, indefinite >23 |

So at PV>=9 (i.e. anything a currently-relevant N2C encoder needs to emit):
**each per-credential `Set Reward` inside `rs` IS wrapped in CBOR tag 258**,
by inheritance through the generic `Set` instance — exactly the same
convention as `TxBody`'s `Set (Credential Staking)`-style fields, `owners`,
`vkey_witnesses`, etc. This was NOT a special case dugite had to invent; it
falls out of `RewardUpdate`'s encoder calling plain `encCBOR` on a
`Map _ (Set Reward)` value with no bypass.

## Answer to the "guess the tag" concern
The "guess tag 258" instinct is CORRECT here, but for a specific, checkable
reason (RewardUpdate's encoder has no custom Map/Set handling, so it falls
through to the library default), not merely because other Sets elsewhere
happen to use it. The Map wrapper around `rs` itself gets NO tag, ever —
only the per-credential Set values inside it do.

## Rust / Dugite cross-check (as of 2026-08-20)
`crates/dugite-node/src/node/n2c_query/encoding.rs`'s
`encode_possible_reward_update` (around line 2647) ALREADY implements this
correctly for the per-credential `Set Reward`: `enc.tag(Tag::new(258))`
before each entry's array. **Flagged but not confirmed**: that function
calls `enc.map(ru.rs.len() as u64)` and `enc.array(entries.len() as u64)`
directly via minicbor, which (per minicbor's own semantics) emit a
DEFINITE-length header unconditionally — there is no threshold-23 branch to
indefinite framing. For the inner `Set Reward` this is realistically
harmless (cardinality is almost always 1-2, per §1's `Set.singleton`
note), but the OUTER `rs` map can easily exceed 23 credentials on any real
network, which is exactly the `#938`-class divergence
(`outputtoobiginvalue`/pparams-reply definite-vs-indefinite framing) this
repo has already found and fixed at OTHER call sites. Worth a targeted
check before shipping a new N2C encoder path that emits this field at
mainnet scale — not verified against a live capture in this pass.

## Related
[[nonmyopic-leaderprobability-precision-and-float-cbor]] — NonMyopic (the
sibling RewardUpdate field), same file's pinning method, `EncCBOR Float`.
[[reward-maturity-mark-set-go-timeline]] — where `rs`'s entries get built
(`collectLRs`/`rewardStakePoolMember`), confirms `Set.singleton` cardinality.
