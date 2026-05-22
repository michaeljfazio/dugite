# CSJ Phase F Validation Report

_Fill in this template after completing the 24-hour live mainnet validation run
described in `CSJ_PHASE_F.md`. Commit the completed report to the repository._

---

## Run metadata

| Field | Value |
|-------|-------|
| Run date (UTC) | YYYY-MM-DD |
| Run start time (UTC) | HH:MM:SS |
| Run end time (UTC) | HH:MM:SS |
| Total wall-clock duration | Xh Ym Zs |
| Network | mainnet / preview / preprod |
| Network magic | |
| dugite version / commit | |
| cardano-node version | |
| Hardware (CPU, RAM, disk) | |
| dugite database source | Mithril snapshot / fresh sync |
| cardano-node database source | Mithril snapshot / fresh sync |

---

## Sync statistics

| Metric | dugite | cardano-node | Delta |
|--------|--------|-------------|-------|
| Blocks processed | | | |
| Starting slot | | | |
| Ending slot | | | |
| Sync time (wall clock) | | | |
| Sync time delta (%) | — | — | ±___% |

Pass criterion: sync time delta < 10%.
Result: PASS / FAIL

---

## CSJ event counts

| Event | dugite | cardano-node | Ratio |
|-------|--------|-------------|-------|
| DynamoElected | | | |
| DynamoStallDemotion | | | |
| JumpIssued | | | |
| IntersectFound | | | |
| ObjectionRaised | | | |
| ObjectionResolved (DynamoWins) | | | |
| ObjectionResolved (ObjectorWins) | | | |
| InvariantViolation | | — | — |
| LoEViolation | | | |

Pass criteria:
- DynamoElected >= 1 on both sides: PASS / FAIL
- DynamoElected ratio within 2x: PASS / FAIL
- ObjectionRaised ratio within 3x: PASS / FAIL
- InvariantViolation = 0 (dugite): PASS / FAIL
- LoEViolation = 0 on both sides: PASS / FAIL

---

## LoE window violations

Total violations (dugite): ___
Total violations (cardano-node): ___

If any violations were detected, list the log lines from `violations.txt` here:

```
(paste violation lines or write "none")
```

Pass criterion: violations.txt empty on both sides.
Result: PASS / FAIL

---

## Trace event equivalence (diff step)

Attach or summarize the output of the 5-minute bucketed diff:

```bash
jq -r '[.ts[0:15], .event] | @tsv' validation/<timestamp>/csj_events.jsonl | sort > /tmp/d.tsv
jq -r '[.ts[0:15], .event] | @tsv' validation/<timestamp>-haskell/haskell_events.jsonl | sort > /tmp/h.tsv
diff /tmp/d.tsv /tmp/h.tsv
```

Diff output (truncated to first 50 lines if long):

```
(paste diff output or write "no diff")
```

Acceptable diff: No LoEViolation lines. DynamoElected within 2x.
ObjectionRaised/Resolved within 3x.
Result: PASS / FAIL

---

## Observations and anomalies

_Describe any unexpected behavior, transient disconnections, peer topology
oddities, or other notable observations from the run._

---

## Artefact locations

| File | Path |
|------|------|
| dugite CSJ events | `validation/<timestamp>/csj_events.jsonl` |
| dugite LoE samples | `validation/<timestamp>/loe_samples.jsonl` |
| dugite summary | `validation/<timestamp>/summary.txt` |
| dugite violations | `validation/<timestamp>/violations.txt` |
| Haskell events | `validation/<timestamp>-haskell/haskell_events.jsonl` |
| Haskell summary | `validation/<timestamp>-haskell/summary.txt` |
| Haskell violations | `validation/<timestamp>-haskell/violations.txt` |

---

## Overall result

- [ ] PASS — all criteria met; CSJ Phase F validated against cardano-node 10.6.x
- [ ] FAIL — one or more criteria failed (see sections above)

Filed issues (if any):
- [ ] #___ — description

Signed off by: ___  
Date: ___
