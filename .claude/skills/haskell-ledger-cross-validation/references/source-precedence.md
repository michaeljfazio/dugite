# Source-of-Truth Precedence & Oracle Query Patterns

When the dump diff flags a real divergence, the next move is *always* to find the canonical Haskell implementation — never to guess from memory or training data.

## The two oracles

### `cardano-haskell-oracle` (preferred)

Pulls live source from GitHub `IntersectMBO/cardano-ledger`, `IntersectMBO/cardano-node`, `IntersectMBO/ouroboros-consensus`, `IntersectMBO/ouroboros-network`. Always preferred for:

- Conway / post-2025 PRs (post-Chang HF).
- Anything where the implementation may have changed since the pre-built knowledge base of the other oracle was generated.
- DRep / governance / committee logic.
- Recent RUPD / reward-update / treasury changes.

### `cardano-ledger-oracle` (fallback)

Pre-built knowledge base from a snapshot of the same repos. Fast, comprehensive, but **frozen in time**. Use when:

- Asking for established Shelley / Allegra / Mary / Alonzo / Babbage semantics that haven't changed.
- The haskell-oracle returns nothing or times out.
- You need a quick cross-check on something the haskell-oracle just told you.

### Decision: which to call?

If the question is about Shelley reward distribution → either is fine, ledger-oracle is faster.

If the question touches Conway governance, recent CIPs, or anything you suspect has changed in the last 6 months → **haskell-oracle**. If both disagree, the haskell-oracle wins (it's reading current GitHub).

## How to write a useful oracle query

Bad: "How does cardano-node calculate rewards?"

Good: "In `Cardano.Ledger.Shelley.Rules.Rupd`, where is `frTotalUnregistered` produced and what does `applyRUpdFiltered` do with it vs the per-pool reward `rs`? I need to know whether unregistered amounts route to `deltaT` or `deltaR`."

The oracle is a colleague who just walked into the room. Give it:
- The module name if you have it.
- The function or constructor.
- The specific question — not "explain X".
- What you already know or have ruled out.

## Citing the oracle in commits / PRs

Every ledger fix commit must include in its body:

```
Canonical reference:
  https://github.com/IntersectMBO/cardano-ledger/blob/<sha>/libs/<crate>/src/<path>#L<n>-L<m>

Spec:
  Shelley spec §11.3 / Babbage spec §X / CIP-1694 §X.Y

Oracle queried: cardano-haskell-oracle  ("…the question you asked…")
```

This gives the reviewer (and future-you, after the next residual chase) a clear paper trail.

## Spec cross-check

The Haskell source is the implementation of record, but the spec PDFs document *intent*. When they disagree, the spec usually loses to the implementation in practice — but **both must be quoted**. Sometimes the spec catches a Haskell bug.

Specs to consult, in order of likely relevance:
1. **Shelley spec** (`shelley-ledger.pdf`) for reward formulas, snapshots, KES/VRF, basic ledger rules.
2. **Babbage spec** for reference inputs, inline datums, ref scripts.
3. **Conway spec** (`conway-ledger.pdf`) for governance, DReps, committees, hard-fork combinator.
4. **CIPs** (1694 for governance, 9 for Plutus cost model, etc.) for individual feature additions.

If you can't find the relevant section in 5 minutes, ask the oracle: "Which spec section governs [behavior]?"

## What counts as evidence

| Source | Counts as evidence? |
|---|---|
| `cardano-cli debug log-epoch-state` from synced cn 11.0.1+ | **Yes** — ground truth |
| `cardano-haskell-oracle` quote of current GitHub source | **Yes** — ground truth for implementation |
| Spec PDF quote | **Yes** — ground truth for intent |
| `cardano-ledger-oracle` quote (cross-checked w/ recent date) | **Yes** — usually safe |
| Koios JSON | Sanity check only — does not show RUPD intermediates |
| db-sync schema | Sanity check only |
| Memory / "I recall…" / training-data answer | **No** — re-query the oracle |
| Tests passing in dugite | **No** — tests can be rewritten to match a wrong implementation |
| "Looks right" / "this matches my mental model" | **No** |
