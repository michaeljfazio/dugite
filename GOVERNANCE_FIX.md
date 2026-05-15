# Governance Metrics Fix

## P0: Committee Composition Wrong

Bug: Duplicate member-committee pairs inflate counts.

```typescript
// Before (buggy)
function getComposition(members) {
  return members.reduce((acc, m) => {
    m.committees.forEach(c => { acc[c] = (acc[c] || 0) + 1; });
    return acc;
  }, {});
}

// After (fixed)
function getComposition(members) {
  const seen = new Set();
  const result = {};
  for (const m of members) {
    for (const c of m.committees) {
      const key = `${m.id}:${c}`;
      if (!seen.has(key)) {
        seen.add(key);
        result[c] = (result[c] || 0) + 1;
      }
    }
  }
  return result;
}
```

## P1: Treasury Divergence

Bug: Block height mismatch between on-chain and indexed data.

```typescript
async function getTreasuryMetrics() {
  const [latest, indexed] = await Promise.all([
    getLatestBlockHeight(),
    getIndexedBlockHeight()
  ]);
  const syncHeight = Math.min(latest, indexed);
  const [onChain, offChain] = await Promise.all([
    getOnChainTreasury(syncHeight),
    getOffChainTreasury(syncHeight)
  ]);
  return { onChain, offChain, syncHeight, divergence: onChain - offChain };
}
```
