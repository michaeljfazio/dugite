# Bug J fix — 30-minute soak result (2026-05-16)

Branch: `feature/local-testnet-docs`
Commit: `c054c7a39` (Bug J ChainSync server cursor revalidation fix)
       + `88aabadf2` (verify.sh canonical-chain filter + 3-way tx consistency)
Evidence: `testnet/local-devnet/evidence/20260516T151504Z/`

## Result

```
PASSED all predicates:
- p1:forge-cross-check (263 canonical blocks, >=3 observers each; 87 orphan(s) excluded)
- p2:per-bp-attribution (pool1=160 pool2=190 via observer)
- p3:tx-inclusion (45 txs submitted; 18 accepted/27 rejected — all 3 nodes agree)
- p4:tip-parity (350/350 ticks in-parity = 100% across all 3 observers)
```

## Convergence

- **Final tip**: slot 1805, block 303, hash `a0a897fcaa16c680b20c81ad6e29105e661bbe14e0173258a657e663bde87278` — identical on all three nodes (dugite-bp, dugite-relay, cardano-bp 11.0.1).
- **Tip parity**: 350/350 samples = 100% across the entire 30-minute window.
- **Fork-unreachable events**: 0 (was 133 over 10 minutes pre-fix).

## Forge attribution

| BP | own forges | adopted (Chain extended) |
|---|---|---|
| dugite-bp (pool1) | 166 | 168 |
| cardano-bp (pool2) | ~190 | n/a |

Roughly balanced as expected for 50-50 stake (some forges orphaned via slot battles).

## Configuration

- K = 10 (security parameter)
- f = 0.2 (active slot coefficient)
- Stake: 50-50 between two pools
- Topology: hub-and-spoke (dugite-bp ↔ dugite-relay ↔ cardano-bp)
- Slot length: 1 second
- Era: Conway

Genesis system_start: 2026-05-16T15:14:54Z. cardano-node version 11.0.1.

## Test plan checked

- [x] `cargo nextest run --workspace`: 4742/4742 tests pass.
- [x] `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- [x] `cargo fmt --all -- --check`: clean.
- [x] `./testnet/local-devnet/verify.sh --self-test`: 8/8 fixture predicates pass.
- [x] 30-min soak with hub-and-spoke devnet: all 4 predicates green.
