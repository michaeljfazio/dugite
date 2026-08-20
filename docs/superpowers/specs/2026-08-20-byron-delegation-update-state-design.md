# Byron delegation + update state — closing #1084's five gapped fields

Status: **DESIGN — no code written.** Tracks: #1084.
Date: 2026-08-20

Scope is deliberately the five measured fields and nothing more:
`byronProtocolParams.{maxBlockSize,maxTxSize,scriptVersion}`, `byronUpdateEpoch`,
`byronDelegation`. Everything else this design touches, it touches only because
one of those five cannot be produced without it.

## Sources, pinned

- **`cardano-ledger-byron` 1.2.0.0** (CHaP) — the exact version the oracle
  binary's build resolves: `cardano-streamer/dist-newstyle/cache/plan.json`
  lists `cardano-ledger-byron 1.2.0.0` from `https://chap.intersectmbo.org/`.
  Quoted below from the tarball in the local cabal cache
  (`~/.cache/cabal/packages/cardano-haskell-packages/cardano-ledger-byron/1.2.0.0/`).
  **Checked against 1.3.0.0, not assumed equivalent**: of the twelve
  load-bearing modules (`Update/Validation/*`, `Delegation/Validation/*`,
  `Block/Validation.hs`, `Byron/API/Validation.hs`, `ProtocolConstants.hs`,
  `Update/ProtocolParameters.hs`), eight are byte-identical and the other four
  differ only in `DecoderErrorUnknownTag` message typing and a `Typeable`
  constraint — zero semantic drift. Grounding on 1.2.0.0 is therefore safe for
  any cardano-node 11.x pin.
- Oracle dump shape: `michaeljfazio/cardano-streamer`, branch
  `dugite/full-era-ledger-dumps`, `src/Cardano/Streamer/Run.hs` (the local
  checkout this repo's PROVENANCE.txt records).
- Measured trajectories: the **209 archived mainnet oracle dumps** at
  `reports/mainnet-exactness/cstreamer-byron-full/` and the preprod oracle
  dumps at `reports/preprod-pv11-oracle/`.
- **Independently cross-checked**: a separate research pass (per this repo's
  divergence-fix process) fetched the same modules from the cardano-ledger
  monorepo at `faa7a9dc347697b11d4da5b7818b1731e11aeeef` — cardano-node
  11.0.1's ledger pin — and its verbatim quotes match the 1.2.0.0 tarball's
  byte-for-byte at every overlap (the UPI `State` record, `initialState`,
  `registerEpoch`, `upAdptThd`, the 2k/4k constants, the delegation machine,
  the genesis-delegation invariants). Its extra findings are folded in below
  and marked where they carry weight.

Note the namespace: this is the OLD `Cardano.Chain.*` tree
(`eras/byron/ledger/impl/src/` in the monorepo), not `Cardano.Ledger.*`. The
terminology differs from every other oracle exercise in this repo —
"blockVersion" means protocol version, "updateImplicit" in genesis JSON maps to
`ppUpdateProposalTTL`, and the CDDL lives at `cddl-spec/byron.cddl` inside the
package.

---

## 1. What the oracle emits, and what mainnet actually did

### 1.1 The five fields' exact shapes

`Run.hs :: extractByronSnapshotData` (fork, current head):

```haskell
delegs = Bimap.toList . Byron.unMap $ ByronDI.delegationMap (cvsDelegationState cvs)
...
, "byronUpdateEpoch" Aeson..= Byron.getEpochNumber (ByronUPI.currentEpoch upiState)
...
, "byronDelegation"  Aeson..= Aeson.object [ "count" Aeson..= length delegs ]
, "byronProtocolParams" Aeson..= Aeson.object
    [ "scriptVersion" Aeson..= ByronPP.ppScriptVersion pparams
    , "maxBlockSize"  Aeson..= ByronPP.ppMaxBlockSize pparams
    , "maxTxSize"     Aeson..= ByronPP.ppMaxTxSize pparams
    , "txFeePolicy"   Aeson..= txFeePolicyJson (ByronPP.ppTxFeePolicy pparams) ]
```

where `pparams = ByronUPI.adoptedProtocolParameters upiState` and
`upiState = cvsUpdateState cvs`. So the sources are, precisely:

| dump field | Haskell state |
|---|---|
| `byronProtocolParams.*` | `UPI.State.adoptedProtocolParameters` — the ADOPTED record, not genesis |
| `byronUpdateEpoch` | `UPI.State.currentEpoch` |
| `byronDelegation.count` | `Bimap.size` of the delegation ACTIVATION map (`DI.State → Activation.State.delegationMap`) |

`txFeePolicy` is emitted structured (`summand` lovelace + `multiplier` exact
rational), which dugite's genesis-derived emission already matches 43/43.

### 1.2 The dump instant is post-EBB

`dumpEpochSnapshots` (fork `Run.hs:640-706`) fires on the FIRST block whose
slot-derived epoch advances, and dumps `swbNewExtLedgerState` — the post-block
state of that block. On mainnet every Byron epoch begins with an EBB at slot
`epoch * 21600` (filenames confirm: `16-345600.json`, 345600 = 16 x 21600), so
**the compared state is the post-EBB state**. dugite's `run_dump_snapshot`
fires post-`apply_block` when `current_epoch > last_epoch` — the same block.
The 43/43 `epoch`/`lastSlot` matches in #1084's measurement prove the pairing.

This matters because it fixes WHERE adoption must have happened: the epoch-16
dump shows the new `maxTxSize`, so cardano-node's state carries the adopted
value **already at the EBB**. §2.2 shows the mechanism that makes that true
(consensus ticks `epochTransition` at the block's slot, before applying the
EBB), and §3.4 shows dugite's ordering reproduces it.

### 1.3 Mainnet's measured trajectory — the numbers the implementation must hit

Extracted from all 209 archived oracle dumps (value-change points only):

| epoch | maxBlockSize | maxTxSize | scriptVersion | byronUpdateEpoch | delegation count |
|---|---|---|---|---|---|
| 0-15 | 2000000 | 4096 | 0 | 0 | 7 |
| 16-83 | 2000000 | **65536** | 0 | 0 | 7 |
| 84-207 | **32768** | **8192** | 0 | 0 | 7 |

So mainnet exercises **two on-chain adoptions** (visible at the 15→16 and
83→84 boundaries), `scriptVersion` never moves, `byronUpdateEpoch` is 0 at all
208 dump points, and the delegation map holds 7 pairs throughout. Note the
issue text's "epoch 100: 32768/8192" sits inside the third band; the first
adoption (epoch 16, `maxTxSize` 65536 alone) was not previously recorded and
is the harder test — it changes ONE field of the fourteen.

preprod (`reports/preprod-pv11-oracle/1-21600.json`): genesis values
(2000000/4096/0), `byronUpdateEpoch` 0, count 7 — across all 3 Byron epochs.
**preprod exercises seeding and the no-op pipeline only; it contains no Byron
update proposal and no post-genesis delegation certificate.** preview has no
Byron era at all. Only mainnet tests adoption. §5 builds the validation plan
on that fact rather than around it.

---

## 2. The Haskell mechanism, from pinned source

### 2.1 State shape

`Cardano.Chain.Block.Validation:138`:

```haskell
data ChainValidationState = ChainValidationState
  { cvsLastSlot :: !SlotNumber
  , cvsPreviousHash :: !(Either GenesisHash HeaderHash)
  , cvsUtxo :: !UTxO
  , cvsUpdateState :: !UPI.State
  , cvsDelegationState :: !DI.State
  }
```

dugite already models `cvsLastSlot` (tip) and `cvsUtxo`. The two missing
fields are exactly the two this design adds.

### 2.2 How consensus drives it — tick vs block vs boundary

The consensus path is NOT `Block.Validation.updateBlock` (that is the CHAIN
test rule, its own comment says so). It is
`Cardano.Chain.Byron.API.Validation`, which ouroboros-consensus calls:

```haskell
-- Byron/API/Validation.hs:107
applyChainTick cfg slotNo cvs =
  cvs
    { CC.cvsUpdateState =
        CC.epochTransition (mkEpochEnvironment cfg cvs) (CC.cvsUpdateState cvs) slotNo
    , CC.cvsDelegationState =
        D.Iface.tickDelegation currentEpoch slotNo (CC.cvsDelegationState cvs)
    }
  where currentEpoch = CC.slotNumberEpoch (Gen.configEpochSlots cfg) slotNo
```

- **Tick (every block's slot, EBBs included)**: `epochTransition` (UPIEC — the
  adoption rule) + `tickDelegation` (activate + prune).
- **Main block** (`validateBlock`): `headerIsValid` (size only) + `updateBody`
  (delegation certs, UTxO, `UPI.registerUpdate`), then
  `cvsLastSlot := blockSlot`.
- **EBB** (`validateBoundary`): prev-hash check, size ≤ 2e6, and
  `cvsLastSlot := boundaryBlockSlot epochSlots epoch` (= `epoch * epochSlots`,
  `Block/Block.hs:533`). **No body, no UPI, no delegation.**

`epochTransition` (`Block/Validation.hs:483`):

```haskell
epochTransition env st slot =
  if nextEpoch > currentEpoch
    then UPI.registerEpoch updateEnv st nextEpoch
    else st
  where nextEpoch = slotNumberEpoch (kEpochSlots k) slot
```

with `currentEpoch = slotNumberEpoch epochSlots (cvsLastSlot cvs)` — the epoch
of the last APPLIED block. So ticking to the EBB of epoch 16 (cvsLastSlot
still in epoch 15) runs `registerEpoch` BEFORE the EBB is applied — which is
why the post-EBB dump carries the adopted values (§1.2). The first main block
of epoch 16 ticks again and no-ops (16 > 16 is false).

**A reading trap, worth naming because the independent pass fell into it**:
`Block.Validation.updateBlock` (the CHAIN test rule) runs `epochTransition`
only for MAIN blocks, so reading that file alone concludes "adoption fires on
the first main block, EBBs are inert" — and the post-EBB dump at epoch 16
showing the ADOPTED `maxTxSize` refutes that conclusion. The consensus path is
`Byron.API.Validation.applyChainTick`, which ticks `epochTransition` at every
block's slot, EBBs included. The measured dump is the arbiter, and it agrees
with the API path.

### 2.3 UPI.State, and why `byronUpdateEpoch` is 0 forever

`Update/Validation/Interface.hs:108` — eleven fields:

```haskell
data State = State
  { currentEpoch :: !EpochNumber
  , adoptedProtocolVersion :: !ProtocolVersion
  , adoptedProtocolParameters :: !ProtocolParameters
  , candidateProtocolUpdates :: ![CandidateProtocolUpdate]
  , appVersions :: !Registration.ApplicationVersions
  , registeredProtocolUpdateProposals :: !Registration.ProtocolUpdateProposals
  , registeredSoftwareUpdateProposals :: !Registration.SoftwareUpdateProposals
  , confirmedProposals :: !(Map UpId SlotNumber)
  , proposalVotes :: !(Map UpId (Set KeyHash))
  , registeredEndorsements :: !(Set Endorsement)
  , proposalRegistrationSlot :: !(Map UpId SlotNumber)
  }
```

`initialState` seeds `currentEpoch = 0`, `adoptedProtocolVersion = 0.0.0`,
`adoptedProtocolParameters = Genesis.configProtocolParameters config` (the
genesis `blockVersionData`), everything else empty.

**The `currentEpoch` finding.** `registerEpoch` (`Interface.hs:487-518`) has
two branches: no version change returns `st` untouched; a version change
updates `adoptedProtocolVersion`/`adoptedProtocolParameters` and clears seven
proposal-tracking fields — **and its record update does not name
`currentEpoch`**. A grep of the whole package finds exactly one writer:
`initialState`'s `currentEpoch = 0`. The field is vestigial. That is the
canonical grounding for the oracle's measured `byronUpdateEpoch = 0` at all
208 mainnet dump points, and for why parameters still adopt while the epoch
counter never moves (the two facts looked contradictory until the source
settled it). dugite models the field as what it is — a state field with no
writer after seeding — with this provenance in its doc comment. It is NOT a
hardcoded 0 in the dump: if upstream ever grows a writer, the state field is
where the change lands.

### 2.4 The per-block update pipeline (`updateBody` → `UPI.registerUpdate`)

`Block/Validation.hs:362-448`. Per MAIN block, in order:

1. block size ≤ `ppMaxBlockSize` of the ADOPTED params (validation — out of
   scope here, but note it exists: the latent consensus edge #1084 records).
2. `DI.updateDelegation` over `blockDlgPayload` (certificates).
3. `UTxO.updateUTxO` (dugite already does this).
4. `UPI.registerUpdate` with:

```haskell
updateSignal = UPI.Signal updateProposal updateVotes updateEndorsement
updateProposal = Update.payloadProposal $ blockUpdatePayload b
updateVotes    = Update.payloadVotes $ blockUpdatePayload b
updateEndorsement = Endorsement (blockProtocolVersion b) (hashKey $ blockIssuer b)
```

**Every main block registers an endorsement** of the protocol version its
header advertises, keyed by the issuer's key hash. This is why decoding the
header's `protocolVersion` (extra-data field 0) and `issuer_pubkey`
(consensus-data field 1) — both currently skipped — is IN scope: without them
no endorsement can ever be counted and no version can ever adopt.

**A load-bearing ordering subtlety**: `updateEnv`'s `delegationMap` is read
from the `BodyState`'s ORIGINAL `delegationState` binding — the pre-payload
map — not from `delegationState'` produced by step 2. A delegation certificate
in a block does not affect vote/endorsement resolution in that same block.
This is #1074's lesson in miniature (statement order inside a decomposed
Haskell expression is a consensus surface); dugite must preserve it.

The sub-rules, each with its consequence for dugite:

**Registration** (`registerProposal`, `Registration.hs:330+`): proposer must
be a delegate of a genesis key (`Delegation.memberR proposerId delegationMap`);
signature over the proposal body; a "null update" (protocol version AND
parameters unchanged AND software version not new) is rejected outright
(mainnet/preprod magics have no exemption — the two exemption slots are
staging-only, magic 633343913); the protocol half then requires no other
registered proposal with the same version, `pvCanFollow` (same major ⇒
minor+1; major+1 ⇒ minor 0), and `canUpdate`: proposal size ≤
`ppMaxProposalSize`, `newMaxBlockSize <= 2 * adoptedMaxBlockSize`,
`newMaxTxSize < newMaxBlockSize`, scriptVersion diff ∈ {0, 1}. On success the
proposal enters `registeredProtocolUpdateProposals` (keyed by
`UpId = recoverUpId = hashDecoded` — **blake2b-256 over the proposal's own
serialized bytes**, so the decoder must KeepRaw the proposal) with the new
FULL parameter record (`PPU.apply` overlays the sparse update on the adopted
record at registration time, not at adoption time), and
`proposalRegistrationSlot` records the slot. The software half registers into
`registeredSoftwareUpdateProposals` independently; a proposal may carry
either half or both.

A structural consequence worth stating because it settles a design question:
`protocolVersionChanged` is
`not (protocolVersion == adoptedPV && PPU.apply ppu adoptedPP == adoptedPP)` —
true when the PARAMETERS differ even at an unchanged version — and such a
proposal then hits `pvCanFollow`, which requires a strict version increment.
**Parameters cannot change without a protocol-version bump.** Every mainnet
adoption therefore also moved `adoptedProtocolVersion`, which is why the
version is mandatory state even though no dump field reports it: registration
validity, endorsement matching, and the FADS ordering all key on it.

**Voting** (`Voting.hs:126-208`): the vote's `voter` verification key is
resolved BACKWARD through the delegation map (`Delegation.lookupR`) to the
genesis key it signs for — votes are counted per GENESIS key, one each
(`VotingVoteAlreadyCast`); the proposal must be registered. Confirmation:

```haskell
pastThreshold votes' = length (M.findWithDefault Set.empty upId votes') >= threshold
```

with `threshold = upAdptThd numGenKeys adoptedProtocolParameters`
(`ProtocolParameters.hs:217`):

```haskell
upAdptThd numGenKeys pps = floor $ stakeRatio * toRational numGenKeys
  where stakeRatio = lovelacePortionToRational . srMinThd . ppSoftforkRule $ pps
```

`LovelacePortion` has implicit denominator 1e15; mainnet/preprod/preview all
carry `softforkRule.minThd = 600000000000000` = 0.6 and 7 genesis keys, so the
threshold is **floor(0.6 x 7) = 4** everywhere. On confirmation the UpId moves
into `confirmedProposals` with the confirming slot. Confirmation also promotes
the software half into `appVersions` and drops it from
`registeredSoftwareUpdateProposals` (`registerVotes`, `Interface.hs:315-355`).

**Endorsement** (`Endorsement.hs:148-236` + `Interface.hs:408-479`): the
endorsing key hash resolves backward through the delegation map to its genesis
key (an unresolvable key is silently ignored, per the UPEND comment). If a
registered proposal exists for the endorsed version AND that proposal is
confirmed-and-STABLE — confirmed at least `kSlotSecurityParam k` = **2k slots**
ago — AND the per-version endorsement count reaches the SAME `upAdptThd`
threshold (4), a `CandidateProtocolUpdate {cpuSlot = currentSlot, ...}` is
created. The FADS rule prepends it only if its version strictly exceeds the
current head's, so the candidate list is newest-first by construction. On
EVERY endorsement registration (i.e. every main block), proposals older than
`ppUpdateProposalTTL` slots (genesis JSON `updateImplicit`; mainnet 10000) and
not confirmed are pruned from all four proposal-tracking maps, and
endorsements for versions no longer registered are dropped.

**Adoption** (`registerEpoch` → `PVBump.tryBumpVersion`,
`Interface/ProtocolVersionBump.hs:41-63`): at the first tick that crosses an
epoch boundary,

```haskell
stableCandidates =
  filter ((\x -> addSlotCount (kUpdateStabilityParam k) x <= epochFirstSlot) . cpuSlot)
         candidateProtocolVersions
```

— the newest candidate whose `cpuSlot` is at least `kUpdateStabilityParam k` =
**4k slots** (`ProtocolConstants.hs`) before the NEW epoch's first slot is
adopted: `adoptedProtocolVersion`/`adoptedProtocolParameters` replaced, and
`candidateProtocolUpdates`, both proposal maps, `confirmedProposals`,
`proposalVotes`, `registeredEndorsements`, `proposalRegistrationSlot` all
cleared. If nothing qualifies, the state is untouched (candidates persist to
later boundaries — a candidate created less than 4k before a boundary adopts
at the NEXT one).

Mainnet constants: k = 2160, so 2k = 4320 slots (1 day), 4k = 8640 slots
(2 days), epoch = 21600 slots.

### 2.5 The delegation machine

`Delegation.Map` (`Delegation/Map.hs:37`) is a `Bimap KeyHash KeyHash` —
delegator ↔ delegate, both directions unique. `KeyHash` is
`blake2b_224 (sha3_256 (cbor vk))` (`Common/AddressHash.hs` — the double
hash; dugite's Byron address code already computes this family, see §6d).

`DI.State` = `{ schedulingState, activationState }`:

```haskell
-- Scheduling.hs
data State = State
  { scheduledDelegations :: !(Seq ScheduledDelegation)   -- (sdSlot, sdDelegator, sdDelegate)
  , keyEpochDelegations :: !(Set (EpochNumber, KeyHash)) }
-- Activation.hs
data State = State
  { delegationMap :: !Delegation.Map
  , delegationSlots :: !(Map KeyHash SlotNumber) }
```

**Scheduling** (`scheduleCertificate`, `Scheduling.hs:176+`): issuer must be a
genesis key; certificate epoch ∈ {currentEpoch, currentEpoch+1}; one
certificate per (epoch, issuer) pair; one per issuer per activation slot;
signature valid. On success:
`activationSlot = currentSlot + kSlotSecurityParam k` (**2k slots**), appended
to the Seq.

**Activation** (`Activation.hs::activateDelegation`): a scheduled delegation
with `sdSlot <= currentSlot` activates iff the DELEGATE is not already a
delegate in the bimap (`notMemberR`) and
`prevDelegationSlot < slot || slot == 0`; activation is
`Delegation.insert delegator delegate` (bimap insert — replaces the
delegator's previous pair, which is why mainnet's count stays 7 through
re-delegations) plus `delegationSlots[delegator] = slot`.

**Tick** (`tickDelegation = prune . activateDelegations currentSlot`,
`Interface.hs:181-217`): activate everything due, then prune scheduled
delegations with `sdSlot <= currentSlot` and keyEpoch pairs with
`epoch < currentEpoch`. Runs at every consensus tick AND as the tail of
`updateDelegation` (after scheduling a block's certificates) — idempotent, and
dugite runs it at the same two points (§3.4).

**Genesis seeding** (`DI.initialState`, `Interface.hs:94-137`): start from the
IDENTITY map — every genesis key delegates to itself, `delegationSlots` all 0 —
then apply the genesis `heavyDelegation` certificates through the normal
`updateDelegation` path with **k = 0** (immediate activation). Mainnet's 7
certificates each replace one identity pair, leaving 7 (issuer → delegate)
pairs. The seeding MUST route through the same schedule/activate code as
on-chain certificates, exactly as upstream does — a shortcut that just inserts
pairs would skip the `notMemberR` rule and drift on any genesis where a
delegate collides.

`allowedDelegators` = the key set of genesis `bootStakeholders`
(`gdGenesisKeyHashes` parses the `bootStakeholders` JSON field,
`Genesis/Data.hs:93`; weights are irrelevant here). Checked on mainnet:
`bootStakeholders` has 7 keys, identical to `heavyDelegation`'s key set.
`numGenKeys = Set.size allowedDelegators` (`Block/Validation.hs:440`).

### 2.6 Wire formats (from `cddl-spec/byron.cddl` + the CBOR instances)

All inside the main-block body `[txPayload, sscPayload, dlgPayload, updPayload]`
that `era_byron.rs:444-447` currently skips:

```cddl
dlgPayload = [* dlg]
dlg  = [ epoch : u64, issuer : pubkey, delegate : pubkey, certificate : signature ]

updPayload = up = [ "proposal" : [? upprop], votes : [* upvote] ]   ; arity 2, proposal FIRST
upprop = [ bver, bvermod, softwareVersion : [text, u32],
           data : { * text => [hash, hash, hash, hash] },
           attributes, from : pubkey, signature ]                    ; arity 7
bver   = [u16, u16, u8]                                              ; major, minor, alt
bvermod = [ scriptVersion : [? u16], slotDuration : [? bigint],
            maxBlockSize : [? bigint], maxHeaderSize : [? bigint],
            maxTxSize : [? bigint], maxProposalSize : [? bigint],
            mpcThd : [? u64], heavyDelThd : [? u64],
            updateVoteThd : [? u64], updateProposalThd : [? u64],
            updateImplicit : [? u64], softForkRule : [? [u64, u64, u64]],
            txFeePolicy : [? txfeepol], unlockStakeEpoch : [? u64] ]  ; arity 14, Maybe = 0/1-array
upvote = [ voter : pubkey, proposalId : updid, vote : bool, signature ]
updid  = blake2b-256      ; recoverUpId = hashDecoded — hash of the PROPOSAL'S OWN BYTES
```

The vote's `bool` is a wire fossil: the encoder hardcodes `True` and the
pinned decoder does `void $ decCBOR @Bool` (`Update/Vote.hs:206`) — decoded
and DISCARDED. Negative voting does not exist; dugite's decoder accepts the
bool and drops it, and `ByronUpdVote` carries no decision field.

In the header (both currently skipped):

```cddl
blockcons   = [slotid, pubkey, difficulty, blocksig]   ; field 1 = issuer_pubkey (64 bytes)
blockheadex = [ "blockVersion" : bver, "softwareVersion" : [text,u32], attributes, extraProof ]
```

`blockheadex` is header field 4 (`extra_data`, skipped at `era_byron.rs:407`);
`blockVersion` is its element 0 — the endorsement's protocol version.
`pubkey` is the 64-byte extended verification key; its CBOR (for KeyHash
computation) is a plain 64-byte bstr.

Genesis JSON (`config/*/byron-genesis.json`): `heavyDelegation` maps issuer
KeyHash hex (28 bytes) → `{ cert: sig hex, delegatePk: base64 64B,
issuerPk: base64 64B, omega: epoch }` (`Delegation/Certificate.hs:251-262` —
`omega` IS the certificate epoch, 0 in every genesis). `bootStakeholders` maps
KeyHash hex → weight.

**A trap already in the tree**: the synthetic fixture at `era_byron.rs:879-881`
builds `upd_payload` as `[votes, maybe_proposal]` with a MAP for votes — the
comment has the element order reversed and both elements are the wrong shape.
Harmless today (both are `skip()`ed); the moment the decoder lands, that
fixture is invalid input and the fixture builder must be corrected to
`[maybe_proposal_as_0_or_1_array, votes_array]`.

---

## 3. dugite design

### 3.1 Decode (`dugite-serialization/src/decode/era_byron.rs`)

New typed carrier, boxed onto `Block` (dugite-primitives):

```rust
pub struct ByronBlockAux {
    /// Header extra-data field 0 (`bver`) — the version this block ENDORSES.
    pub protocol_version: (u16, u16, u8),
    /// Header consensus-data field 1 — needed to key the endorsement.
    pub issuer_pubkey: Vec<u8>,             // 64 bytes
    pub dlg_certs: Vec<ByronDlgCert>,        // {epoch, issuer_vk, delegate_vk, signature}
    pub upd_proposal: Option<ByronUpdProposal>,
    pub upd_votes: Vec<ByronUpdVote>,        // {voter_vk, proposal_id: Hash32, signature} — bool discarded (§2.6)
}
// Block gains: pub byron: Option<Box<ByronBlockAux>>   // None for EBBs and non-Byron
```

- `ByronUpdProposal` carries `up_id: Hash32` computed at decode time as
  blake2b-256 over the proposal's raw CBOR span (KeepRaw over the `upprop`
  element — the same `KeepRaw::parse_with` discipline the tx decoder uses),
  plus the parsed `bver`, the 14 `bvermod` optionals, and `software_version:
  (String, u32)`. `data`/`attributes` are skipped as opaque (they only feed
  the hash, which is over raw bytes anyway).
- `bvermod` numeric fields: CDDL says `bigint` for the sizes. Decode CBOR
  uint or bignum, hard-error above u64 — no real chain carries one, and a
  silent truncation on a consensus-adjacent value is #952's shape.
- `Box` per the existing `Box<Block>` size discipline (CLAUDE.md key
  patterns); the common case (every non-Byron block, every EBB) pays one
  `Option` niche.

**Do NOT populate `BlockHeader.issuer_vkey` for Byron.** `apply.rs:765-771`
feeds any non-empty `issuer_vkey` into `consensus.epoch_blocks_by_pool`, which
`eras/shelley.rs:366` reads as `bprev` at epoch transitions. Today the Byron
decoder leaves it empty, that branch is dead for Byron, and the whole
Byron→Conway pipeline is validated 0-divergent over 312 mainnet epochs on
exactly that behaviour. Routing the issuer key through `header.issuer_vkey`
would silently change validated state at the Byron→Shelley seam. The issuer
key lives in `ByronBlockAux` only.

Fuzzing: `fuzz_decode_block` covers the new paths automatically; add one real
mainnet main-block with a non-empty `dlgPayload` and one with an `updPayload`
proposal to `fuzz/seeds/decode_block/` (mind `-max_len` — libFuzzer truncates,
cov 84-vs-1331 precedent).

### 3.2 Ledger state (`dugite-ledger`)

New top-level `LedgerState` field `byron: ByronSubState` (always present;
default-empty on Shelley-genesis networks; inert after the Byron era):

```rust
pub struct ByronSubState {
    pub delegation: ByronDelegationState,
    pub update: ByronUpdateState,
    /// Genesis constants the rules need at apply time.
    pub allowed_delegators: BTreeSet<Hash28>,   // bootStakeholders key set
}

pub struct ByronDelegationState {
    pub scheduled: Vec<ScheduledDelegation>,        // (slot, delegator, delegate), append-order (Seq)
    pub key_epoch_delegations: BTreeSet<(u64, Hash28)>,
    pub delegation_map: BTreeMap<Hash28, Hash28>,   // delegator -> delegate
    pub delegation_map_rev: BTreeMap<Hash28, Hash28>, // delegate -> delegator (Bimap's other half)
    pub delegation_slots: BTreeMap<Hash28, u64>,
}

pub struct ByronUpdateState {
    /// Vestigial upstream: only writer is initialState (§2.3). Dumped as byronUpdateEpoch.
    pub current_epoch: u64,
    pub adopted_protocol_version: (u16, u16, u8),
    pub adopted_protocol_parameters: ByronProtocolParameters,   // all 14 fields (§2.4 needs them)
    pub candidate_protocol_updates: Vec<ByronCandidate>,        // newest-first (FADS order)
    pub app_versions: BTreeMap<String, (u32, u64)>,             // name -> (version, slot)
    pub registered_protocol_update_proposals: BTreeMap<Hash32, ((u16,u16,u8), ByronProtocolParameters)>,
    pub registered_software_update_proposals: BTreeMap<Hash32, (String, u32)>,
    pub confirmed_proposals: BTreeMap<Hash32, u64>,             // UpId -> confirmation slot
    pub proposal_votes: BTreeMap<Hash32, BTreeSet<Hash28>>,     // UpId -> genesis keys
    pub registered_endorsements: BTreeSet<((u16,u16,u8), Hash28)>,
    pub proposal_registration_slot: BTreeMap<Hash32, u64>,
}
```

- **Every map/set is ordered** — #1088's rule: an unordered map in the
  snapshot makes the format-hash digest nondeterministic and
  `snapshot_format_hash_stability` cannot catch it with single-entry fixtures.
- The bimap is two BTreeMaps kept in lockstep by the two mutation sites
  (insert-with-replacement, and nothing else — Byron has no delegation
  removal). Both directions are load-bearing: `lookupR` resolves votes and
  endorsements to genesis keys, `notMemberR` gates activation.
- `ByronProtocolParameters` holds all 14 fields with EXACT arithmetic:
  `LovelacePortion` fields as u64 numerators over implicit 1e15, `txFeePolicy`
  as `(u64, (u64, u64))` (the same exact-rational shape
  `ByronTxFeePolicy::to_exact` already produces), sizes as u64. The threshold
  `floor(minThd/1e15 * numGenKeys)` must be computed in integers:
  `(min_thd as u128 * num_gen_keys as u128 / 1_000_000_000_000_000) as usize`
  — floor of an exact rational, no floats.
- `k` and `byron_epoch_length` already live on `LedgerState`
  (`security_param`, `byron_epoch_length`); the rules read them from there.

**Seeding.** One function, `LedgerState::seed_byron_genesis(...)`, mirroring
`UPI.initialState` + `DI.initialState`: adopted params from the FULLY-parsed
genesis `blockVersionData` (extend `dugite-node/src/genesis.rs`'s
`ByronBlockVersionData` from 4 parsed fields to all 14 — `_heavy_delegation`
and `_boot_stakeholders` lose their underscores and gain typed structs),
protocol version 0.0.0, identity delegation map, then the genesis certificates
through the REAL schedule/activate path with activation delay 0. Called from
every ledger-init site that seeds genesis UTxOs today (`node/mod.rs` x3,
`main.rs` dump path) — and note the standing trap: the zero-value-UTxO fix
(c0556c4e64) was nearly hidden by a SECOND copy of a genesis filter. Seed in
ONE dugite-ledger function; the node only parses and passes.

### 3.3 Snapshot + rollback

- `LedgerStateSnapshot` (bincode, positional) gains the `byron` field; the
  conversion functions, `fixture_populates_every_snapshot_field`, and
  `snapshot_format_hash_stability` extend accordingly — the hash-stability
  fixture MUST populate the new maps with ≥2 entries each (#1088: a guard that
  cannot fail for multi-entry maps is weaker than it reads).
- Rides the CURRENT fresh `SNAPSHOT_VERSION` — 39 as of `013e699f6a` this
  session. Whether 39 can still be extended in place when this lands is decided
  by `xtask/tests/snapshot_one_bump_invariant.rs` against the tag history, not
  by this document.
- **`LedgerDelta`**: one content-diffed field
  `byron_snapshot: Option<ByronSubState>`, following the
  `genesis_delegates_snapshot` precedent exactly (`ledger_seq.rs:259-266`):
  a TOP-LEVEL `LedgerState` field needs BOTH the delta field (applied in
  `apply_delta_to_state`) AND an explicit copy-back in `rollback_via_seq`
  (`state/mod.rs`) — the wholesale-copied substates do not cover it. The
  content diff is `None` on almost every block: outside an active proposal
  window the per-block endorsement short-circuits (`Endorsement.register` with
  no matching registered proposal returns the state unchanged), pruning
  no-ops on empty maps, and the delegation tick no-ops on an empty Seq. During
  a proposal window nearly every block clones a state of a few KB — acceptable.
  Epoch-boundary adoption lands inside the boundary block's own content diff;
  no `EpochTransitionDelta` change is needed.
- `_assert_ledger_state_fields_audited` (`ledger_seq.rs:1774`): the new
  top-level field makes the exhaustive destructure fail to compile, which is
  the mechanism working. The entry is `byron: _` with a comment naming
  `byron_snapshot` — matching how `certs`/`gov`/`utxo` are recorded, since the
  whole substate is delta'd as one unit.
- **Era transition**: at Byron→Shelley the substate is KEPT, not cleared —
  clearing would itself need delta representation for rollbacks across the
  seam, and the retained state is ~2 KB. Nothing reads it after Byron.
- **Mithril import**: imports land at a Shelley+ tip; `ByronSubState` stays
  default-empty and nothing ever reads it (the dump emits Byron fields only in
  Byron-era epochs). No import-side decode work — there is no
  `ChainValidationState` in a post-Shelley cardano-node snapshot.

### 3.4 Apply hooks (`state/apply.rs`, the Byron branch at :697)

Order per applied Byron block, matching §2.2's tick-then-body:

1. **Epoch transition** (already dispatched before Step 5, at ~:436, via
   `ByronRules::process_epoch_transition`): add the UPIEC arm — compute
   `epoch_first_slot = new_epoch * byron_epoch_length`, scan
   `candidate_protocol_updates` (newest-first) for the first with
   `cpu_slot + 4*k <= epoch_first_slot`; on a hit and a version change,
   install its version+params and clear the seven proposal fields (§2.4's
   exact list). `current_epoch` is NOT touched (§2.3). This runs when applying
   the first block of the new epoch — the EBB on mainnet — which is before
   the dump fires, reproducing the post-EBB adopted values (§1.2).
   Haskell runs ONE `registerEpoch` per tick with `nextEpoch = epoch(slot)`
   even across a multi-epoch gap of empty epochs; if dugite's transition
   dispatch loops per intermediate epoch for Byron, the UPIEC arm must still
   evaluate stability against the FINAL epoch's first slot only (open item
   §6a; unobservable on mainnet, which has no empty Byron epochs).
2. **Delegation tick** at the top of the Byron branch (every Byron block, EBB
   included — EBBs flow through the branch with zero transactions): activate
   scheduled delegations with `slot <= current`, prune scheduled `<= current`
   and keyEpoch `< epoch(current)`.
3. Transactions (unchanged).
4. **Delegation payload**: fold `schedule_certificate` over
   `block.byron.dlg_certs` — genesis-issuer check, epoch ∈ {cur, cur+1},
   per-(epoch,issuer) and per-activation-slot uniqueness; activation slot
   `current + 2*k`. Then tick again (the `updateDelegation` tail). Signature
   verification deferred (§4).
5. **Update payload**, against the delegation map AS OF step 2 (pre-payload —
   the §2.4 ordering subtlety; in practice bind the resolver's view before
   step 4 or document that certificates activate ≥2k slots later so the map
   cannot have changed in step 4 — the latter is TRUE (activation delay is 2k,
   never 0 on-chain) but the former is the faithful shape and costs one
   borrow):
   proposal registration (null-update check, `pvCanFollow`, `canUpdate`,
   duplicate-version check, `PPU.apply` overlay at registration), votes
   (backward-resolve, per-genesis-key dedup, confirm at ≥4), endorsement from
   `(protocol_version, key_hash(issuer_pubkey))` (backward-resolve, silently
   drop unresolvable, candidate at ≥4 endorsements if confirmed ≥2k slots ago,
   FADS prepend), TTL pruning.
   **Failure posture**: in `ValidateAll`, a rule failure rejects the block
   (matching upstream). In `ApplyOnly` — the only mode any real Byron chain
   runs — a rule failure means dugite's state has already diverged from the
   node that accepted this block: hard error, per #914's crash-don't-diverge
   precedent. No silent skip.

### 3.5 Dump (`dugite-node/src/main.rs`)

Replace the three `null`s at :2306-2325 with state readouts:
`byronDelegation = {count: ledger.byron.delegation.delegation_map.len()}`,
`byronUpdateEpoch = ledger.byron.update.current_epoch`, and
`byronProtocolParams` = the four keys of §1.1 read from
`adopted_protocol_parameters` — which RETIRES the genesis-derived
`byron_pparams` threading (main.rs:659-692, :1006, :1674): the state is now
the source, and the genesis values reach it via seeding. `txFeePolicy` keeps
its measured 43/43 match because no network ever changed it on-chain (and if
one had, the state readout is the side that is RIGHT).

### 3.6 Deliberately out of scope, named

- **Wiring adopted `maxBlockSize`/`maxTxSize`/`maxHeaderSize` into
  validation.** `ByronFeePolicy::canonical()` and the absence of Byron size
  checks stay exactly as they are. Sequencing is the same as the fee-policy
  work: the dump comparison proves the adopted values against the oracle
  FIRST; moving consensus onto them is its own later change. The latent edge
  (#1084's "reject blocks cardano-node accepts" note) remains recorded, not
  fixed.
- **Byron signature verification — recommend filing as its own issue.**
  Verified absent, not assumed: `era_byron.rs:391-404` skips both
  `issuer_pubkey` (until this design) and `block_sig`; `praos.rs:1550` skips
  verification for empty-KES (Byron) headers; no OBFT/PBFT verifier exists in
  `dugite-consensus`. The natural single issue covers: block signatures
  (`blocksig` variants incl. heavyweight `dlgsig` and lightweight `lwdlgsig`),
  delegation-certificate signatures, and proposal/vote signatures — this
  design decodes the inputs the first two need. Three facts that issue will
  want, pinned now so they are not re-researched: the pinned BLOCK-SIGNATURE
  decoder accepts tag **2 only** and rejects tags 0/1 outright
  (`Block/Header.hs:696-701`) while dugite's `skip()` accepts anything — a
  decode-strictness accept-where-Haskell-rejects on adversarial input; the
  proposal signature covers a SYNTHETIC framing, `0x85` (array-5 header)
  prepended to the `ProposalBody` bytes
  (`recoverProposalSignedBytes = fmap ("\133" <>)`, `Proposal.hs:257`), so
  verification needs a KeepRaw over the BODY span, distinct from the
  whole-proposal span this design captures for `UpId`; and the vote signature
  signs the synthetic pair `(proposalId, True)`, not the wire 4-array. The
  delegation-certificate signed payload is
  `"00" <> delegateVK_xpub_bytes <> cbor(epoch)` (`Certificate.hs::isValid`).
  Until that issue lands, dugite's Byron trust model is ApplyOnly replay of a
  locally-stored chain, which is the honest current state, now with the gap
  enumerated.
- **SSC payload** (`sscPayload`): dead subsystem, no oracle output, stays
  skipped.
- **Lightweight delegation** (`lwdlg`): block-signature-only concept; goes
  with the signature issue.

---

## 4. What "software update" state is kept, and why

The five fields never read `appVersions` or the software-proposal map. They
are modeled anyway (four small fields) because the state MACHINE routes
through them: whether a proposal registers at all depends on
`softwareVersionChanged` (the null-update rule), confirmation moves entries
between them, and TTL pruning restricts them — dropping the fields would fork
the transition function, not just the output. What is NOT modeled is anything
downstream of `appVersions` (nothing is downstream inside the node).

---

## 5. Validation — how this is proven, not argued

**preprod is necessary and NOT sufficient, and that is measured, not
suspected** (§1.3): its 3 Byron epochs contain no proposal, no vote, no
endorsement above zero registered proposals, and no post-genesis certificate.
A green preprod proves seeding, the no-op pipeline, and the boundary ticks —
it is structurally blind to adoption, exactly the #1089 class of green. Do not
let a preprod pass close this issue.

**The primary gate is the mainnet Byron replay against the ARCHIVED oracle.**
The 209 dumps in `reports/mainnet-exactness/cstreamer-byron-full/` are
cardano-node's own state; no oracle re-run, no sync, no disk pressure. dugite
replays mainnet Byron in minutes (the full 6.5M-block clone replay measured
27 min; Byron is the cheap prefix). Run `dugite dump-snapshot` over the
existing chain copy, diff with `scripts/validation/diff-cstreamer-dumps.py`
(method: `reports/mainnet-exactness/METHOD.md`). Success criteria:

1. The five fields compare **0-divergent over all 208 paired Byron epochs** —
   which specifically pins the two adoption steps: `maxTxSize` 4096→65536
   visible first at epoch 16, `maxBlockSize`/`maxTxSize` →32768/8192 first at
   epoch 84, and NOT one epoch earlier or later. A wrong stability window
   (2k for 4k, endorsement-slot for confirmation-slot, first-main-block for
   EBB ordering) shows up as an off-by-one-boundary divergence at exactly
   those two epochs; epochs 17-83 pin that the intermediate state persists.
2. **Every other field is 0-divergent against the previous dugite run** — the
   "did anything else move" check the eta fix established. This change must
   not move `utxo.*`, `lastSlot`, `epoch`, or any Shelley+ field; in
   particular the Byron→Shelley translation inputs (`bprev` emptiness — §3.1's
   issuer_vkey hazard) are covered by treasury/reserves at 208+ remaining
   byte-identical.
3. preprod 1-to-1 re-run: exit code moves for the Byron epochs from SCHEMA GAP
   to compared-and-matching (the three `byronProtocolParams` leaves +
   `byronUpdateEpoch` + `byronDelegation` at epochs 1-3).

**Unit fixtures from real chain data**: vendor the mainnet blocks carrying the
two adoption chains — the proposal, enough votes to confirm, the endorsement
that creates the candidate, and the adopting boundary — as decode+apply
fixtures (locate them during implementation by logging non-empty payloads in
the replay; they are in epochs ≤16 and ≤84 respectively). Each rule lands with
a disarm-proven RED test per repo standard, with the standing caveat that a
RED unit test bounds the function, not the system — the system bound is
criterion 1.

**What the comparison still cannot see, recorded honestly**: the oracle emits
only `byronDelegation.count`, so a delegation map with the right SIZE and
wrong PAIRS compares green (mainnet re-delegations replace pairs and hold the
count at 7). The mechanism tests bound the pair logic; if stronger evidence is
ever wanted, the cheap route is teaching the ORACLE fork to emit the pairs
(both sides sorted) — a follow-up oracle change, deliberately not part of this
design.

---

## 6. Open questions — unresolved, with owners

a. **Multi-epoch-gap dispatch.** Does `apply.rs`'s epoch-transition dispatch
   loop per intermediate epoch for Byron, or fire once? Haskell's tick runs
   `registerEpoch` ONCE with the final epoch (§2.2). The implementer must
   check the dispatch and, if it loops, evaluate UPIEC only on the final
   iteration. Unobservable on mainnet (no empty Byron epochs); still a
   mechanism-fidelity requirement.
b. **`Block.byron` vs re-decoding from `raw_cbor` at apply time.** This design
   chooses the typed field: `raw_cbor` is `Option` and re-decoding at apply
   time creates a second parser path (the N-copies trap). If `Block`'s size or
   clone cost surfaces as a problem in review, revisit — but boxed-Option
   should be free.
c. **preprod/preview `bootStakeholders` == `heavyDelegation` key identity** was
   checked only on mainnet (7 == 7, sets equal). Verify on the other genesis
   files during implementation; the seeding does not ASSUME identity (the
   identity map comes from `bootStakeholders`, the certs from
   `heavyDelegation`, exactly as upstream).
d. **The exact dugite helper for Byron `KeyHash`.** `hashKey =
   blake2b_224 . sha3_256 . cbor(vk)` over the 64-byte verification key
   (`Common/KeyHash.hs:53`, `Common/AddressHash.hs`). dugite's
   `dugite-primitives/src/address/byron.rs` already uses the sha3/blake2b
   pair for address roots — confirm whether an existing helper computes the
   bare-pubkey variant (CBOR bytes(64), no address attributes) or add one.
   Get this wrong and every vote/endorsement resolution fails silently
   (endorsements are DROPPED, not errored, when unresolvable — §2.4), which
   would present as "no adoption at epoch 16" in criterion 5.1, so the replay
   does catch it.
e. **`attributes` fields** (proposal, header extra-data element 2) are opaque
   CBOR maps on every real chain — skipped, with the raw span still inside the
   KeepRaw hash range. If a chain ever carries non-empty semantics there,
   nothing in this design reads it (upstream reads it for nothing relevant to
   the five fields either).

## 7. What was checked and does NOT need re-checking

- 1.2.0.0 vs 1.3.0.0 module diff (§Sources) — semantics identical.
- `UPI.State.currentEpoch` has exactly one writer in the package
  (`initialState`); `registerEpoch` does not touch it (§2.3). This is the
  entire story of `byronUpdateEpoch == 0`.
- EBB slot assignment: dugite's `decode_byron_ebb_block` assigns
  `epoch * byron_epoch_length`, which equals upstream's `boundaryBlockSlot`
  (`Block/Block.hs:533-539`). The dump pairing already matches 43/43.
- Mainnet genesis constants: k=2160, 7 boot stakeholders == 7 heavy
  delegates, `srMinThd` 0.6e15 ⇒ thresholds 4-of-7, `updateImplicit` 10000
  slots, `heavyDelThd` present but unused by these five fields.
- The oracle's Byron dump reads the ACTIVATION map (not scheduling), and
  `adoptedProtocolParameters` (not any candidate/registered record) — §1.1's
  quotes are from the fork as built.
