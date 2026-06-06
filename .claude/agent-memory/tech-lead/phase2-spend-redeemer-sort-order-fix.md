---
name: phase2-spend-redeemer-sort-order-fix
description: Phase-2 Spend redeemer index must use Set TxIn sorted order not wire order — Alonzo mainnet divergence (~303/block-window)
metadata:
  type: project
---

## Fix: Phase-2 `Spend` redeemer index resolution must use sorted `Set TxIn` order

**Bug:** `resolve_spend` in `crates/dugite-uplc/src/redeemer_resolve.rs` indexed
into `tx.body.inputs.get(idx)` — the on-wire CBOR array order. Haskell's
`alonzoRedeemerPointer` / `conwayRedeemerPointer` uses `Set.elemAt idx (txBody ^. inputsTxBodyL)` — the i-th element of inputs as an **ascending sorted Set TxIn**.

**Why:** The CBOR array field `inputs` in a tx body has no ordering guarantee from
the ledger's perspective. cardano-ledger stores inputs as `Set TxIn`, and the redeemer
index is into that sorted set. When on-chain tx encoding order differs from sort order,
dugite picked the wrong input → "spent output's address is not script-locked" (~303
divergences per block window during Alonzo mainnet sync).

**Ground truth:** mainnet tx `4a3b78c246f30425754966396d10ffcba0b9cc8b97c6d3f9f54d8c6d30154422`
- wire[0]=`fb71…#0` (script-locked), wire[1]=`f905…#1` (key-locked)  
- sorted[0]=`f905…#1`, sorted[1]=`fb71…#0` (0xf9 < 0xfb)  
- Spend redeemer index=1 → Haskell picks sorted[1]=`fb71…` (correct). Dugite picked wire[1]=`f905…` (wrong).

**Fix (2 sites, same sort key):**
1. `redeemer_resolve.rs::resolve_spend` — sort inputs before indexing: `sort_inputs(&tx.body.inputs).get(idx)`
2. `populate_v1_v2.rs::populate_tx_info_v1/v2` and `populate_v3.rs::populate_tx_info_v3` — sort before building `txInfoInputs` (and `txInfoReferenceInputs`, also a `Set TxIn`)

**Sort key:** Rust's derived `Ord` on `TransactionInput` (`transaction_id: Hash<32>`, then `index: u32`) is byte-exact with Haskell's `Ord TxIn`: raw 32-byte TxId lexicographic, then TxIx numeric.

**New helper:** `tx_info_populate::sort_inputs(&[PrimTxIn]) -> Vec<PrimTxIn>` — exported pub, used at all 5 sites.

**Certs are NOT sorted** (Haskell uses declaration/insertion order for certs — already correct).
**Mint** uses BTreeMap iteration (already sorted by PolicyId). **Withdrawals** use BTreeMap (already sorted).

**Tests added:**
- `redeemer_resolve::tests::spend_redeemer_index_uses_sorted_input_set_not_wire_order` — wire=[A(0xfb),B(0xf9)], redeemer idx=1, asserts resolves to A(0xfb) not B(0xf9)
- `tx_info_populate::tests::sort_inputs_orders_by_txid_bytes_then_index` — pins the canonical 0xfb/0xf9 vector
- `tx_info_populate::tests::sort_inputs_same_txid_ordered_by_index` — TxIx numeric order

**Why:** critical for any Alonzo+ tx where on-chain input encoding order differs from TxId sort order. Affects all V1/V2/V3 Spend scripts.
