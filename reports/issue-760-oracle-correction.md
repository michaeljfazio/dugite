# #760-A — Oracle correction: the original A1 fix design is Haskell-DIVERGENT

Date: 2026-06-13. Source: cardano-haskell-oracle live read of IntersectMBO/
ouroboros-consensus `Ouroboros.Consensus.MiniProtocol.ChainSync.Client(.Jumping)`
and `Ouroboros.Consensus.Genesis.Governor`.

## What the original design (reports/issue-760-genesis-csj-wedge-design.md) claimed
A1: "record the RollForward header into pending_headers + candidate-fragment +
CSJ FIRST, then apply forecast_park_or_disconnect as pipeline BACKPRESSURE" —
justified by "Haskell, where ChainSync blocks at the forecast horizon only for
issuing MsgRequestNext, while theirFrag/jTheirFragment already hold the streamed
headers."

## What Haskell ACTUALLY does (authoritative, source-grounded)
`rollForward` ordering for ONE received header:
1. `checkKnownInvalid`
2. `Jumping.jgOnRollForward (blockPoint hdr)` — CSJ jump trigger. **READS** the
   current `cschJumpInfo` to decide whether to schedule a jump; does **NOT**
   update `jTheirFragment`.
3. `setLatestSlot (NotOrigin slot)` — `csLatestSlot` set to the RAW received slot,
   BEFORE the forecast. (This is the ONLY shared state ahead of the forecast.)
4. `checkTime` — the forecast block. **STM-retries / BLOCKS here** if the slot is
   `OutsideForecastRange` (> ledger_tip + 3k/f). LoP bucket paused for the wait.
5. `checkValid` → appends header to `theirFrag` (LOCAL `kis` only).
6. `checkLoP`.
7. `atomically { updateJumpInfoSTM (jTheirFragment := theirFrag); setCandidate
   (csCandidate := theirFrag) }` — the shared candidate fragment that BlockFetch
   reads, AND the CSJ jump fragment, are BOTH written here, AFTER the forecast.
8. `nextStep` → MsgRequestNext.

**Therefore Haskell does NOT put beyond-horizon headers into `csCandidate` or
`jTheirFragment`.** Buffering unvalidated/beyond-horizon headers into the
candidate fragment (the original A1) would DIVERGE from Haskell — and could
mis-feed LoE/GDD density and CSJ jumps. A1 as written is REJECTED.

## The true deadlock condition (oracle)
Haskell self-primes because `csCandidate` holds forecast-validated headers from
`ledger_tip` up to `ledger_tip + 3k/f` (mainnet ≈ 8640 headers). BlockFetch
fetches those bodies, applies them, the ledger advances, the forecast horizon
advances, the parked header unblocks. The deadlock occurs ONLY when `csCandidate`
is EMPTY/stuck at the ledger tip with nothing for BlockFetch — i.e. the FIRST
header received after restart is already beyond the forecast horizon, so the
candidate never fills.

Haskell avoids this on cold restart because **the peer serves headers from the
INTERSECTION POINT (at/near the snapshot tip), not from its tip** — the first
headers are near the snapshot, within the just-loaded ledger's forecast horizon,
so `csCandidate` fills before the horizon is reached.

GDD/LoE detail: `loeFrag = sharedCandidatePrefix curChain (csCandidate <$> states)`;
LoE tip = head of the longest common prefix of ALL peers' `csCandidate`. Chain
selection is capped at the LoE tip (`trimToLoE`). The ledger advances up to the
LoE tip via BlockFetch on headers already in `csCandidate`. GDD ALSO consults
`csLatestSlot` for the `hasBlockAfter` genesis-window density bound (so a
received-but-not-yet-in-csCandidate header still influences density correctly).
BlockFetch reads `csCandidate` ONLY (no csLatestSlot fallback, no separate buffer).

## Corrected direction for the dugite fix (to be confirmed by LIVE repro)
The fix must make dugite mirror Haskell's self-priming, NOT buffer beyond-horizon
headers. Candidate hypotheses for dugite's actual divergence (need live evidence):
1. The dynamo's candidate fragment (`peer_state` / `pending_headers`) does fill
   with in-horizon headers, but the LoE is pinned at the immutable tip because the
   JUMPERS' fragments are empty until the dynamo broadcasts its first jump — and
   something prevents the jump/jumper-advance/LoE-advance loop from priming
   (dynamo rotation watchdog firing during legitimate parks? jump cadence vs
   forecast interplay? csLatestSlot not fed to dugite's GDD hasBlockAfter?).
2. The post-snapshot ChainSync intersection / `reintersect_promoted_peer` lands
   somewhere that makes the dynamo stream headers immediately beyond the horizon
   (so the candidate never fills) — the precise Haskell-avoidance mechanism.
3. The forecast check incorrectly returns OutsideForecastRange for in-horizon
   headers (stale view / wrong stability window).

NEXT: reproduce the wedge live on db-mainnet-val (genesis cold restart) WITH
diagnostics on (candidate-fragment length per peer, LoE tip slot, dynamo role +
rotations, forecast park reasons), and read the ACTUAL stuck state before writing
any fix. Do NOT implement the original A1.
