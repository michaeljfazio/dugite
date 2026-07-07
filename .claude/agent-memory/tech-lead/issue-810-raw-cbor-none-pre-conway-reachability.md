---
name: issue-810-raw-cbor-none-pre-conway-reachability
description: TransactionOutput.raw_cbor is None for EVERY pre-Conway-era (Mary/Alonzo/Babbage) output decoded via the real block-decode path, not just an edge case
type: reference
---

While fixing #810 (Rule 5 min-UTxO `raw_cbor=None` fallback, phase1.rs), traced the decoder
call graph in `crates/dugite-serialization/src/decode/`:

- **Conway (era_id 6/7)**: `decode_conway_tx_body` (era_conway.rs:468) decodes `outputs` via
  `read_babbage_tx_output_with_raw` (era_conway.rs:994), which wraps `read_babbage_tx_output`
  in `KeepRaw::parse_with` and sets `output.raw_cbor = Some(raw.raw.to_vec())`. Every Conway
  output gets `raw_cbor` populated. SAFE.
- **Babbage (era_id 5)**: `decode_babbage_tx_body` (era_babbage.rs:391) decodes `outputs` via
  `read_babbage_tx_output` DIRECTLY (era_babbage.rs:443), with NO `KeepRaw` wrapping — no
  per-output raw-bytes capture. Every Babbage-era output decoded through the real block path
  (`decode_babbage_block`/`decode_babbage_block_mode`) has `raw_cbor: None`.
- **Mary/Alonzo/Allegra (era_id 2/3/4)**: same gap — `decode_alonzo_tx_body` (era_alonzo.rs:452)
  decodes `outputs` via `read_alonzo_tx_output` directly (era_alonzo.rs:497), no raw capture.
- The `_standalone` variants (`decode_babbage_tx_output_standalone`,
  `decode_alonzo_tx_output_standalone`, `decode_conway_tx_output_standalone` in
  `crates/dugite-serialization/src/decode/mod.rs::decode_transaction_output`) DO set
  `raw_cbor` correctly for all eras — but that's a different, out-of-band UTxO-resolution
  entry point, NOT the one used for normal block/tx body decoding.

**Implication**: this is NOT a rare edge case — it is the DEFAULT state for every single
Mary/Alonzo/Babbage transaction output decoded via the standard block-processing path (from-genesis
sync, ChainDB replay on restart, Mithril-imported chunk re-decode). Before the #810 fix,
`min_utxo_for_output_size` fell back to the 29-byte ADA-only floor for every one of these
outputs — under-charging the true min-UTxO for any multi-asset (Mary+) output. Escalates #810
from its filed severity (P2, "reachability limited... confirm during fix") to a confirmed,
widespread (though currently dormant on live networks, since mainnet/preview/preprod are all
past Babbage) historical-replay divergence.

**Not fixed here** — root cause lives in `dugite-serialization` (a different crate); the #810
fix itself (phase1.rs Rule 5) now re-encodes the output via `dugite_serialization::encode_transaction_output`
when `raw_cbor` is `None`, which correctly neutralizes the effect for ALL eras regardless of
this decoder gap. But the decoder-level gap itself (`read_alonzo_tx_output`/`read_babbage_tx_output`
missing `KeepRaw` wrapping, unlike Conway's `_with_raw` pattern) should be filed as its own
follow-up issue — likely relevant to other `raw_cbor`-dependent checks (e.g. `compute_min_fee`'s
`ref_script_fee` / `fee_tx_size` paths, script-data-hash-from-cbor fallback) that may ALSO
silently degrade for pre-Conway eras. Live N2C tx submission today is unaffected (current
networks are Conway-only, so submitted txs decode via the safe Conway path).
