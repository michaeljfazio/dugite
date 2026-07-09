---
name: issue-gov-action-18cd564-refuted-2026-07-09
description: Preprod cost-model ParameterChange gov_action18cd564yw8... claimed non-enacted by dugite — REFUTED by direct live-node N2C queries; real mechanism documented for regression coverage
metadata:
  type: project
---

Investigation was handed "confirmed facts" that dugite never ratifies/enacts Conway ParameterChange
`gov_action18cd564yw8jcsj392ggtge8swd3pkxm5k6rdhlf3sv308z0rjy3gsqdgxfqp` (preprod, cost-model update,
submitted tx `3e1b4d548e3cb10944aa42168c9e0e6c43636e96d0db7fa630645e713c722451` ep287, ratified 288,
enacted 289 on Haskell), and that this explained a 100K-ADA-short reward-account balance driving a
systematic ~0.0113% reward drift.

**Direct verification against the live BP soak node (`./node.sock`, preprod, epoch 299 at time of
check) REFUTED this**:
- `dugite-cli query protocol-parameters` costModels: PlutusV1/V2 len=332, PlutusV3 len=350, byte-identical
  first5/last5 to the proposal's on-chain CBOR (decoded via `tx_cbor` from Koios) — cost models WERE
  enacted correctly. Preprod genesis has no V1/V2 array and only a 251-entry V3, so this could only come
  from a successful on-chain enactment.
- `dugite-cli query stake-address-info` for the proposal's return-address credential
  (`6a6596e95f3e5f59c22ba49e7dac49f57c4a58e14f189a8347d7f27a`, bech32
  `stake_test1up4xt9hftul97kwz9wjfuldvf86hcjjcu9833x5rgltly7s9xkkn9`) returned
  `rewardAccountBalance: 1472652648`, byte-identical to Koios `account_info.rewards_available`.
  Koios `proposal_refund: 200000000000` for that address = exactly the sum of TWO 100K-ADA deposits
  (this ParameterChange, enacted 289, + a HardForkInitiation to PV11, enacted 294) that both refund to
  the same credential — confirmed via `proposal_list` filtered by `return_address`. dugite's balance
  matching Koios exactly proves BOTH deposits were refunded correctly.
- `dugite-cli query gov-state` pending-proposals list (5 entries, all later/unrelated tx ids) does NOT
  contain the target tx — consistent with it having been ratified+enacted+removed, not stuck pending.

**Likely explanation**: the running soak binary (`target/release/dugite-node`, process started
2026-07-09 00:00:23) postdates the `a88c8fc776` ("Conway governance ratification/committee/pparam-update
batch #799,#800,#802,#812", 2026-07-06 12:03 +0800) and `6222944053` ("enforce pvCanFollow on live GOV
path #858,#812", 2026-07-06 23:08 +0800) fix commits. Whatever caused the original observation (possibly
made against an older build, a different/stale `./db-*` directory, or before those fixes landed) no
longer reproduces. **The 95-account ±100 ADA reward drift needs to be re-investigated against a FRESH
epoch-diff dump from the current binary — this specific gov action is confirmed NOT its cause.**

## Real mechanism found while investigating (kept for defense-in-depth, even though not the live bug)

Oracle-verified (`cardano-haskell-oracle`, live fetch 2026-07-09) exact Haskell source for the CC
ratification gate: `eras/conway/impl/.../Governance/Internal.hs:444-479`
(`votingCommitteeThresholdInternal`) and `.../Rules/Ratify.hs:118-163` (`committeeAccepted`).
`activeCommitteeSize` is `Map.filterWithKey isActive` over `committeeMembers`
(non-expired AND non-resigned AND hot-key-authorized), gated against `pp ^. ppCommitteeMinSizeL` — the
LIVE on-chain value, not a genesis constant. Confirmed dugite's `check_cc_approval`
(`governance.rs:3413-3528`, `active_size < committee_min_size` gate) matches this exactly.

Concrete preprod scenario that stress-tests this path: a `NewCommittee` action enacted at epoch 232 set
7 members (4 expiring epoch 242, 3 expiring epoch 372) — exactly meeting `committeeMinSize=7` from
genesis. Separately, a `ParameterChange` enacted at epoch 233 lowered `committeeMinSize` 7→3
on-chain (`gov_action1f9tcawsvss8gytsx3zcfzyhnlxa2adga6r35d3dyl8gr6t9ur9fsq2kuh0j`). By epoch 288, the
4 short-term members had expired, leaving active_size=3 — which only clears the minSize gate because
`committeeMinSize` was ALSO lowered to 3 by that point. **If a node's live-tracked
`committee_min_size` were ever stale (still 7, e.g. via the same bug class as historical issue #94 —
"stale committee_min_size from snapshot defaults" — see `governance.rs:7269-7350`
`test_parameter_change_ex_units_ratified_and_enacted`), active_size(3) < stale_min_size(7) would
PERMANENTLY deadlock the CC leg for every subsequent ParameterChange/HardFork/NewConstitution action
system-wide**, since the committee can never regrow past minSize once genesis members expire without
CC-approved renewal (and CC approval is itself gated by the same broken check) — a self-reinforcing
governance freeze. Verified dugite's live `committee_min_size` tracks the on-chain value correctly today
(`apply_protocol_param_update_impl`, `governance.rs:2649-2650`), but this is exactly the kind of scalar
that issue #94 already proved can go stale on snapshot-resume paths — worth a targeted regression test
(preprod-epoch-232/233/288 fixture: committee shrinks below genesis minSize, minSize itself is lowered
by governance, later action must still ratify) to lock in against regression, even though it isn't
currently broken.

## Lesson

"Confirmed facts" handed into an investigation prompt are a snapshot in time — always re-verify via a
live query against the actual current build/state before deep-diving code, especially for
governance/reward-divergence claims that may already be fixed by very recent commits. Direct N2C queries
(`dugite-cli query protocol-parameters`, `query stake-address-info`, `query gov-state`) against a running
soak node are fast, decisive, and should be the FIRST step, not the last, in this class of investigation.

Related: [[gov-apply-path-prev-action-id-bypass]], [[issues-799-800-802-812-batch-fix]],
[[conway-plutus-v3-cost-model-seeding]].

---
## CORRECTION (orchestrator, 2026-07-09): this "refutation" is INVALID — bug CONFIRMED

The refutation queried the LIVE SOAK NODE, which runs `--include-ancillary` (Haskell-DERIVED) ledger state — correct by construction and NOT a test of dugite's own from-genesis computation. It does not refute the bug.

Dugite's OWN from-genesis epoch-state dumps (offline `dump-snapshot`, current binary WITH the 2026-07-06 gov fixes; `dump-snapshot-epochstate/{287..293}.json`) show `enactedRoots.PParamUpdate` FROZEN at `158ef6b249b7c3ec219c...#0` from ep287→ep293. It NEVER advances to `3e1b4d548e...` (action `gov_action18cd564...`, which chains off 158ef6b2 and which Koios enacted ep289). So dugite genuinely does NOT ratify/enact that ParameterChange during from-genesis replay. `dump-snapshot` uses `apply_block(.., ApplyOnly)` = full state transitions incl per-tx governance votes + boundary ratification, so this is representative, not an artifact. It reproduced the exact original symptom (WithdrawalAmountMismatch for d0f4075a at slot 124936620). CONFIRMED root cause of the reward drift: dugite enacts ParameterChanges up to 158ef6b2 but fails on the next chained one (18cd564) → its 100K deposit is never refunded → proposer stake 100K short → total_active_stake short → sigmaA drift → all rewards that epoch drift. Next: pin the failing ratify leg (CC/DRep/SPO threshold or tally) for 18cd564. Do NOT trust the ancillary live node to validate dugite's own governance/reward computation — use a from-genesis replay.
