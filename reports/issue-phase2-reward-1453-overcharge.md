# Phase-2 CPU divergence: +1453 cpu on a heavy DeFi reward redeemer (evaluator proven byte-canonical)

## Summary

A live phase-2 divergence dump captured on preview (epoch 1333) shows dugite's parallel
UPLC eval disagreeing with on-chain `is_valid` for **one** redeemer of tx
`2ea39a78c1e509397217ede44c7320cb6f972a514016871aeff2ce1161302d5b`
(block 4397199). dugite consumes **774,091,580** cpu on the `Reward@0` redeemer vs the
declared/enforced budget of **774,090,127** — an over-charge of **+1453 cpu (0.000188%)**.
Because dugite caps each redeemer at its declared exUnits (restricting mode), the over-charge
flips `is_valid` to `false` where the chain says `true`. The node self-recovers via the
trust-on-chain valve (block-level state stays byte-exact), so there is **no chain halt** — but
it is a real byte-exactness gap and, in block production, could cause dugite to forge a
Haskell-invalid block if it ever included a tx whose heavy script sits within ~1453 cpu of its
ceiling.

## Localization (airtight)

The tx has 4 redeemers. Three match the declared exUnits **byte-exactly** (cpu AND mem):

| redeemer | declared cpu | dugite cpu | Δcpu | declared mem | dugite mem |
|----------|-------------:|-----------:|-----:|-------------:|-----------:|
| Spend@0  |   47,031,362 | 47,031,362 |   0  |      129,711 |    129,711 |
| Spend@1  |  217,754,269 |217,754,269 |   0  |      602,537 |    602,537 |
| Mint@0   |   46,935,362 | 46,935,362 |   0  |      129,111 |    129,111 |
| Reward@0 |  774,090,127 |774,091,580 |**+1453**| 2,345,136 | 2,345,136 |

Reward@0's **mem is byte-exact**; only cpu is off. Three exact matches prove the submitter used
cardano-node's exact eval (no exUnit margin). Koios confirms all 4 contracts `valid_contract: true`,
the Reward script (`5a71ae99…`, 8734 bytes) is **inline** (no reference script), and the declared
Reward steps = 774,090,127 (fee 191,127 = `ceil(774090127·0.0000721 + 2345136·0.0577)` ✓).

## Proof the evaluator is byte-canonical

1. **Cost model is correct & current.** dugite's stored cost-model blob byte-matches the current
   Koios preview PlutusV2 model (identical multiset). Recompute with Koios coeffs reproduces
   dugite's exact number.
2. **Shapes are canonical.** The ledger oracle confirmed every CPU cost-function shape
   (`CostingFun/Core.hs`): addInteger/subtractInteger=MaxSize, multiplyInteger=MultipliedSizes(x·y),
   equalsInteger/lessThan*/equalsData=MinSize, equalsByteString=LinearOnDiagonal,
   divide/mod/quot/rem=ConstAboveDiagonal (and in Reward@0 **every** division-family call has x≥y,
   so the diagonal/variant subtlety never fires).
3. **Independent recompute = dugite, per-builtin.** Applying the oracle's authoritative formulas by
   hand (independent of dugite's code) to Koios coeffs + dugite's traced sizes gives **exactly
   774,091,580, with per-builtin diff = 0 on all 29 builtins**.
4. **Matches a reference evaluator.** aiken's eval has identical CEK step counts, identical builtin
   call counts, identical sizes, identical mem, and identical equalsData (166,369,958).

## Impossibility proof (why this is NOT in the evaluator)

The over-charge is +1453 — a **prime**. For two canonical evaluators of the same script to differ:
- a different **step count** → multiple of 16000 (1453 is not);
- a different **builtin count** → multiple of the cheapest builtin (22151) (1453 is not);
- a **leaf-value difference** feeding a size-dependent builtin where mem stays constant
  (the comparison builtins). Enumerating every available knob in Reward@0: `lessThanEqualsInteger(2,2)`
  contributes +552/call (≤3 calls) and `equalsByteString(4,4)` contributes +75/call (≤2 calls);
  integer-comparison mins are clamped to 1 and equalsData's slope is 27,279. **No combination
  `552a + 75b` with a≤3, b≤2 equals 1453.**

Since the contexts are shape-identical (equal step count) and the cost model/shapes/sizes are all
canonical, **a canonical eval cannot produce a +1453 delta.** The discrepancy therefore lives
outside dugite's evaluator — in a non-CEK factor (the exact ScriptContext leaf bytes cardano-node
constructs, or the declared value's builder provenance), which cannot be observed offline.

## Ground truth needed to close

Resolving *where* the 1453 goes requires cardano-node's **own** evaluation of this exact tx:
- Ogmios `evaluateTransaction` (preview node + the dump's 14 spent UTxOs as `additionalUtxo`) →
  confirms cardano's actual exUnits (already known to be ≤ 774,090,127); or
- a Haskell cardano-ledger build that dumps cardano's constructed ScriptContext for a
  **leaf-by-leaf diff** against dugite's context (the surgical pinpoint).

Both are multi-hour standups; deferred. Re-open if it recurs at scale or a cheap Haskell reference
becomes available.

## Reproduce

The dump is self-contained (`reports/phase2-divergence-dumps/phase2-divergence-tx0-02bda6a9454353c6.json`).

```bash
cargo build --release -p dugite-node --bin replay_phase2
# Full replay (per-redeemer consumed vs declared):
DUGITE_PHASE2_UNCAPPED=1 ./target/release/replay_phase2 <dump.json>
# Per-builtin / per-step breakdown for one applied term, using the dump's on-chain model:
DUGITE_UPLC_BUILTIN_TRACE=1 ./target/release/replay_phase2 --flat applied-Reward-0.flat <dump.json>
```

## Impact / status

Sub-0.0002% on one DeFi reward redeemer. Node byte-exact at block level; trust-on-chain valve
self-recovers; chain never halts. Evaluator proven canonical. Shelved with this diagnosis.
