---
name: ImmutableDB stale fork repair procedure
description: How to repair ImmutableDB when stale fork blocks cause gap-bridge loop; tip.meta format and truncation steps
type: project
---

When `flush_to_immutable()` flushes volatile blocks that are later orphaned, the ImmutableDB ends up with stale fork blocks beyond the canonical ledger tip. Symptoms:

```
WARN Gap-bridge: ledger apply failed: Block does not connect to tip: expected <canonical_hash>, got <fork_hash>
INFO Gap-bridge: advanced ledger to meet rollback target ledger_slot=<canonical> rollback_slot=<fork_tip> replayed=0
```

The node logs "ChainDB fork divergence detected" on every peer connection and uses ledger_tip instead of chain_tip for intersection. All peers find the canonical intersection and send MsgRollBackward to the fork tip, which then loops forever because gap-bridge can't bridge the fork.

**Root cause**: `flush_to_immutable()` writes volatile blocks by slot order. If a fork becomes the "k-deep" cutoff while volatile, those fork blocks get flushed to ImmutableDB permanently. After restart, ledger replay stops at the last canonical block, leaving ImmutableDB tip pointing to orphaned blocks.

**Repair procedure** (requires node to be stopped):

1. Identify the last canonical chunk (`N`) and stale chunks (`N+2`, `N+3`, etc.) from the log:
   - `Ledger restored from snapshot ... tip=slot:X@CANONICAL_HASH (block block:BLOCK_NO)` — canonical tip
   - ImmutableDB tip in `tip.meta` points to the fork hash

2. Parse `tip.meta` (48 bytes, big-endian): `[slot:8][hash:32][block_no:8]`

3. Parse the last secondary index entry of the last good chunk to get canonical slot+hash:
   - Secondary index entries are 56 bytes each: `[block_offset:8][pad:8][header_hash:32][slot:8]`
   - Take the entry with the highest slot — that's the canonical tip

4. Get the canonical block number from the startup log line: `(block block:BLOCK_NO)`

5. Delete stale chunk files (`.chunk`, `.primary`, `.secondary`) for all chunks beyond the canonical last chunk

6. Write new `tip.meta` with canonical slot/hash/block_no:
   ```python
   import struct
   data = bytearray(48)
   struct.pack_into('>Q', data, 0, slot)
   data[8:40] = bytes.fromhex(hash_hex)
   struct.pack_into('>Q', data, 40, block_no)
   with open('db-preview/immutable/tip.meta', 'wb') as f: f.write(data)
   ```

7. Remove stale LSM lock file: `rm -f db-preview/utxo-store/lock`

8. Restart node — intersections will now correctly find the canonical point, no gap-bridge failure

**Observed in**: Preview testnet, 2026-04-20. Stale chunk 25457 had 4 fork blocks (slots 109974245/247/352/404) that were flushed during a previous volatile→immutable transition on a fork. Canonical tip was slot 109969905, block 4204542 in chunk 25455.

**Why:** flush_to_immutable() has no mechanism to check if the blocks being flushed are on the canonical chain at time of flush — it just flushes whatever is in VolatileDB past the k-deep boundary. If a chain selection flip happens after that boundary calculation but before the flush, orphaned blocks get committed to ImmutableDB.
