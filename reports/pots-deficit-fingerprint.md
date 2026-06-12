# Mainnet validation DB — inherited ledger-pots deficit: fingerprint

**Date:** 2026-06-12 · **DB:** `db-mainnet-val` (read-only analysis; node untouched)
**Status:** injection epoch pinned, magnitude estimated, mechanism ranked — confirmation procedure defined.

## TL;DR

A single, one-shot reserves deficit of **≈ 996,138 ADA (996,138,309,353 lovelace)** was injected at the
**epoch 388 → 389 boundary** (mainnet slot 82,684,800), processed by session **val12** at
2026-06-11T22:07:45 UTC, ~2 minutes after a restart that restored a **mid-epoch-388 snapshot (70.3 % in)**.
Treasury was **byte-exact at injection**; the observed treasury drift (−29,420.55 ADA at ep442) is purely
the compounded τ·ρ·η·D accrual from that reserves deficit over 53 boundaries. No further injections
occurred (the model fits all observations to < 0.2 %).

---

## 1. Back-integration model (Task 1)

Data: Koios mainnet REST — `/totals` (full series, pots per epoch) and
`/epoch_info?select=epoch_no,blk_count,fees&epoch_no=gte.340&epoch_no=lte.445`.

Definitions (all diffs are Koios − dugite, positive = dugite short):

- Anchors (established): `D(442) = 906,493,555,626` lovelace (reserves), `TD(442) = 29,420,551,118` lovelace (treasury).
- Reserves-deficit evolution: the deficit compounds out of reserves at the same fractional rate as the
  reserves themselves (every flow component — expansion, treasury cut, allocation, deltaR2 return — is
  ∝ reserves): `D(E) = D(E+1) / (1 − f_E)` with `f_E = (R(E) − R(E+1)) / R(E)` from Koios.
- Treasury-drift increment over boundary (E−1)→E (RUPD computed during E−1, η lagged one epoch):
  `incT = τ·ρ·η(E−2)·D(E−1)`, τ = 0.2, ρ = 0.003, η(e) = min(1, blk_count(e)/21600).

**Model verification:** predicted increment over 441→442 = 533.08 ADA vs the established measured
533.76 ADA (0.13 %); model `D(441) = 908,058 ADA` falls inside the live ADA-truncated measurement
window 907.2–908.2 K. ✓

**Backward integration of TD:**

| epoch | D (ADA) | incT (ADA) | cum TD (ADA) |
|------:|--------:|-----------:|-------------:|
| 441 | 908,058 | 533.1 | 28,887.5 |
| 420 | 941,537 | 541.0 | 17,564.8 |
| 400 | 975,763 | 568.4 | 6,359.7 |
| 391 | 992,377 | 576.6 | 1,206.4 |
| 390 | 994,239 | 574.8 | 631.7 |
| **389** | **996,138** | 586.4 | **+45.2** |
| 388 | 998,029 | 584.0 | −538.8 |
| 387 | 999,894 | 580.3 | −1,119.1 |

The cumulative treasury drift extinguishes at **epoch 389** with a residual of **+45 ADA — 7.7 % of one
epoch's increment**. Interpretation: treasury was still exact at epoch 389; the reserves deficit already
existed during epoch 389 (its RUPD, computed during 389 from start-of-389 reserves, produced the first
short treasury increment at boundary 389→390). **⇒ E\* = the 388→389 boundary itself.**

- **Injected amount: D₀ = D(389) = 996,138,309,353 lovelace ≈ 996,138.3 ADA.**
- Sensitivity: under the (unphysical) no-decay bound `D ≡ D(442)` the crossing smears to ~ep385–386, so
  the hard worst-case window is E\* ∈ [386, 389]; under the physically-correct f-decay model the crossing
  is sharply 389 (±1 epoch).
- Degenerate alternative: a paired injection at boundary 389→390 of (R −994,239 ADA, T −632 ADA) fits
  identically — but requires the treasury error to coincidentally equal τ·ρ·η·R-error, which is exactly
  the ratio produced naturally by the deficit existing one epoch earlier. Occam: **R-only at 388→389**.

Earlier rough estimate "ep387±5" refined to **boundary 388→389** (epoch 389 start, mainnet ~2023-01-25).

## 2. Session/log forensics around E\* (Task 2)

All `mainnet-val*` sessions are the SAME node/DB (`./db-mainnet-val`, config-csj). Local = UTC+8.
`logs/mainnet-val9.log` was **overwritten** by the live session (starts 2026-06-12T07:59 UTC, v2.0.4) —
the original session covering epochs 367→384 (Jun 11 17:37–20:42 UTC) is lost. The model excludes that
window anyway (TD would carry a ≥ 2.3 K-ADA unexplained residual).

| session | start (UTC) | binary (bin-val) ≈ commit | restored from | covered | end |
|---|---|---|---|---|---|
| val7 | 11th 14:12 | v5 | — | 356→367 | clean; hit the 365→366 divergence (see below) |
| val8 | 11th 16:21 | v6 ≈ `eed333984a` | **full from-genesis replay** (UTxO store wiped, chunks 0→73,277,026) | →367 | clean — **repairs the 365→366 damage** |
| val9 (orig, lost) | 11th ~17:37 | v6 | ep367 | 367→384 | (log overwritten) |
| val10 | 11th 20:42 | v6 | ep384 @ slot 80,557,426 (7.6 % in) | 384→387 | clean |
| val11 | 11th 21:55 | v7 ≈ `33923f8aa3`/`1c8afcd27d` | ep387 @ 82,199,209 (87.6 % in) | 387→388 | clean @ 70.3 % of 388 |
| **val12** | **11th 22:05** | **v8 ≈ `f2a87d81e7`** | **ep388 @ 82,556,304 (70.3 % in)** | **388→391 (boundary 389 ★)** | **forced exit** (graceful-shutdown 30 s timeout, 22:44:47 — after ep391 snapshot; #750 fixed later) |
| val13 | 11th 22:44 | v9 ≈ `01cba7031a` | ep391 @ 83,810,690 | 391→392 | clean |
| val14+ | 11th 22:54 | v2.0.3 (`2b2b0e52e1`) | ep392 | 392→… | … |

Boundary 388→389 was crossed by val12 at 22:07:45 UTC (last ep388 block slot 82,684,788 → first ep389
block slot 82,685,027) **with zero WARN/ERROR or ledger log lines** — the only val12 warnings are network
noise (#747 ingress overruns, keepalive timeouts, GDD disconnects). No wedge, no panic, no replay gap
(val11's shutdown snapshot tip = val12's restore tip).

Prior incident (different, repaired): commit `eed333984a` records a live divergence at boundary 365→366
(treasury short 857,600,586 lovelace, reserves correspondingly HIGH — the pv6-prefilter freeze-gate bug,
#736-class). val8's from-genesis replay rebuilt the ledger through ep367 with the fix, so it does NOT
contribute to the current deficit (confirmed: the model leaves only +45 ADA at 389, not ~+857).

**Binary note:** v6→v8 ledger diffs are all phase-1-validation-side (#743/#744/#745/#746/#749) — none
touch RUPD/pots application. The RUPD code at boundary 389 was identical to the code that produced
byte-consistent boundaries 385–388. The `#736` persistence fix (`40db083021`) and the freeze-gate fix
(`eed333984a`) were both present; the known #736 field (`rupd_addrs_rew`) is **inert at pv7** (pv ≥ 7
forgoes the reward prefilter). The post-v2.0.3 `68daa12026` (full DELEGS dereg sequencing, #748) was NOT
in v8, but is check-side only.

## 3. Mechanism ranking & magnitude analysis (Task 3)

Signature to explain: at one boundary, dugite reserves end **996,138 ADA short**, treasury **byte-exact**,
one-shot (no recurrence in 53 subsequent boundaries).

Treasury exactness is a strong constraint: `ΔT = τ·(ρ·η·R + feeSS)` was correct ⇒ the RUPD *inputs*
(reserves, η/blocks-total, feeSS) were all correct. The divergence is strictly **downstream of the τ cut**:
dugite's `totalAllocated` (Σ member+leader rewards in the computed RUPD) was **996,138 ADA higher** than
Haskell's, i.e. its deltaR2 return to reserves was understated by **10.04 % of the true ≈9,919,205-ADA
deltaR2** at that boundary (Koios flow decomposition: gross expansion 28.15 M, net outflow 18.24 M).
By conservation, dugite's reward-accounts pot should be **≈ +996,138 ADA above** Haskell's from ep389 on
(testable prediction).

Ranked candidates:

1. **[LEADING] Restart-perturbed reward distribution at the first post-restore boundary.** The 388→389
   RUPD was computed entirely at the boundary (dugite computes the full reward update from persisted
   snapshot state — `go` snapshot, `bprev_blocks_by_pool`, `ss_fee`, prev-pparams) by a process that had
   restored a mid-epoch (70.3 %) snapshot minutes earlier; the consumed `go` snapshot (taken at 386→387
   by val10/v6) had round-tripped v22 serialization across three binaries (v6→v7→v8). A distribution-side
   perturbation (per-pool stake/pledge/params or per-pool block attribution in `bprev` — total preserved,
   composition shifted) inflates Σ allocated while leaving ΔT exact. Fits sign, magnitude class, one-shot
   nature, and the restart correlation. *No specific code defect identified yet* — that is what the
   confirmation replay isolates.
2. **Deterministic content-triggered divergence in epoch-388 reward computation** (e.g. an edge case in
   member/leader caps or pool-reward aggregation specific to that epoch's go-snapshot contents). Same
   pots signature; distinguishable from #1 because it **reproduces on a clean no-restart replay**.
3. **Ruled out:**
   - *MIR double-apply / missed MIR*: no reserves-MIR flow anomaly at 388→389 (net outflow 18.235 M sits
     between neighbours 17.97/18.31 M; a 996 K lump would be visible); reserves→address Catalyst MIRs had
     ended by this era; a treasury-MIR error would hit treasury.
   - *Unregistered-rewards filter at application*: Haskell routes those to **treasury** (per the 365→366
     incident analysis) — treasury was exact.
   - *η / feeSS / reserves input corruption* (incl. block double-count through the restore): would move
     ΔT — treasury was exact.
   - *#736 `rupd_addrs_rew` loss*: pv ≥ 7 forgoes the prefilter — field unused at ep388.
   - *`eed333984a` freeze-gate*: only differs when prev-pv ≠ cur-pv; ep388 is pv7/pv7.
   - *Residue of the 365→366 incident*: repaired by val8's from-genesis replay (verified in val8 log).
   - *Ongoing live bug*: forward accrual 441→442 matches τ·ρ·η·D within 1 ADA (established).

## 4. Predictions and definitive confirmation (Task 4)

Model-predicted drifts (Koios − dugite) for cross-checking against any retained snapshot/dump:

| epoch | Tdiff (ADA) | Rdiff (ADA) |
|------:|------------:|------------:|
| 390 | 631.7 | 994,239.4 |
| 400 | 6,359.7 | 975,762.8 |
| 420 | 17,564.8 | 941,536.7 |
| 440 | 28,351.6 | 909,642.0 |

**Definitive confirmation (in priority order):**

1. **Clean replay across E\***: in a separate DB copy (never the live `db-mainnet-val`), replay epochs
   387→390 from immutable chunk data with the current binary (`epoch-state-debug` dumps at each boundary),
   no restarts. Diff pots at 389 vs Koios/`cardano-cli debug log-epoch-state` (cn 11.0.1):
   - reserves exact at 389 ⇒ mechanism #1 confirmed (restart-dependent); then reproduce by forcing a
     stop/restart at ~slot 82,556,000 (70 % of 388) before the boundary — pots short ≈ 996,138 ADA at 389
     reproduces the injection and bisects the corrupted snapshot field (diff the restored vs in-memory
     `go`/`bprev` state).
   - reserves short ≈ 996,138 ADA even without restart ⇒ mechanism #2 (deterministic RUPD bug); dump
     per-pool reward allocations at the boundary and diff against Koios `pool_history` (epoch 387 rewards)
     to find the over-paid pools/accounts.
2. **Reward-pot cross-check**: dugite's aggregate reward-accounts balance at any epoch ≥ 389 should
   exceed the Haskell/Koios value by ≈ 996,138 ADA (decaying only via the affected accounts' withdrawals —
   strictly, the *sum of pots* utxo+treasury+reserves+rewards+deposits+fees still equals 45 B in dugite;
   the deficit lives in T+R and the surplus in reward accounts). Identifying *which* credentials hold the
   surplus pins the exact code path.
3. **Drift-table spot-check**: any single retained snapshot between 390 and 441 matching the prediction
   table above (±2 ADA on Tdiff) confirms the single-injection model and excludes later top-ups.

**Remediation once confirmed:** the deficit is inherited state — fix requires either a from-genesis (or
pre-388-snapshot) re-replay with the fixed/clean binary (as val8 did for the 365 incident), or continuing
the run with a documented known-constant offset (T-drift grows ~534–586 ADA/epoch; R-deficit decays ~0.2 %/epoch).

---
*Method artifacts: Koios pulls in `/tmp/koios_totals_all.json`, `/tmp/koios_epochinfo_340_445.json`;
model script inline in this session. Logs consulted: `logs/mainnet-val{,2-8,10-24}.log` (val9 = live,
header/tail only). No node, DB, or crate sources were modified.*
