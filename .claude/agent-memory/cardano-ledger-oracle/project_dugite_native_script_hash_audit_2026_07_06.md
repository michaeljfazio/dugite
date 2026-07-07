---
name: project_dugite_native_script_hash_audit_2026_07_06
description: Confirmed dugite native-script hashing bug (re-encodes AST instead of hashing raw wire bytes) + a second, already-half-fixed indefinite-length outer-array decode divergence across 3 duplicate decoders
metadata:
  type: project
---

2026-07-06: user asked to verify a suspected byte-exact native-script hashing bug. Confirmed against source (not just suspicion) by reading the actual dugite files. Haskell-side ground truth is [[native-script-hash-memobytes-safetohash]].

**Bug 1 (confirmed, the one asked about) — hash computed over a canonical re-encode, not original bytes.**
- `crates/dugite-primitives/src/transaction.rs:96` `NativeScript` enum is a pure AST with **no raw-byte field** — nothing captures the wire bytes a script was decoded from (unlike tx body/witness-set/header, which DO use `KeepRaw`, see `crates/dugite-serialization/src/decode/raw.rs`).
- `crates/dugite-ledger/src/validation/scripts.rs:147` `native_script_hash()` computes `blake2b_224(0x00 || dugite_serialization::encode_native_script(ns))` — `encode_native_script` (`crates/dugite-serialization/src/encode/script.rs:41`) always emits canonical minimal-int, definite-length CBOR regardless of how the script actually arrived on the wire.
- Same function is reused by `compute_script_ref_hash`'s NativeScript arm (line 163), Phase-1 Rule 13, and `check_extraneous_script_witnesses` — so BOTH witness-set scripts and TxOut reference scripts are affected identically (matches the Haskell finding that both contexts use the same mechanism).
- Fix requires threading original per-script byte spans through from decode (a `KeepRaw`-style wrapper around `NativeScript`, captured once per script — top-level and each nested constructor doesn't need its own span since `hashScript` is only ever called on the outermost `Script`/`Timelock` value) into `native_script_hash`/`compute_script_ref_hash`, replacing the `encode_native_script` call with the captured raw bytes.

**Bug 2 (found incidentally while grounding the answer) — outer array indefinite-length rejection, inconsistent across 3 duplicate implementations.**
- There are 3 separate `read_native_script` implementations: `era_shelley.rs:1161`, `era_alonzo.rs:1143`, `era_conway.rs:2521`.
- `era_shelley.rs` and `era_alonzo.rs` both explicitly reject `arr_len.is_none()` ("native_script: expected definite-length array") — this HARD-REJECTS a Haskell-valid indefinite-length outer Timelock array.
- `era_conway.rs:2521` was ALREADY fixed for this (large comment block citing the same `decodeRecordSum`→`decodeListLike`→`decodeListLenOrIndef` chain independently discovered by the cardano-haskell-oracle agent in this same session — good cross-validation of both findings), referencing "#10 round-4 F2" as the fix commit/issue. Test `era_conway.rs:4923` `script_ref_native_script_indefinite_outer_array_imports` covers it.
- So the Conway-era reference-script-import path already tolerates this; the Shelley witness-set path (native scripts have existed since Shelley, still valid in every later era's witness set) and the Alonzo-path (shared by Babbage via `read_native_script_from_cbor` re-export) do not. This is a live, real over-rejection bug independent of the hashing bug — a legally-Haskell-accepted tx with an indefinite-length native script witness would currently be wrongly rejected by dugite pre-Conway-decoder-paths.
- Nested inner arrays (`ScriptAll`/`ScriptAny`/`ScriptNOfK`'s script lists) already correctly tolerate indefinite-length in all 3 implementations via the shared `read_array` helper (`crates/dugite-serialization/src/decode/reader.rs:332`) — only the outer sum-discriminant array was ever restricted.
- `read_uint` (`reader.rs:702`, via minicbor's `.u64()`) is already non-canonical-tolerant (matches Haskell's plain `decodeWord64`) — no divergence there.

Not yet fixed as of this audit; flagged for a tech-lead follow-up (dedupe the 3 `read_native_script` copies while fixing both, so this doesn't need a 4th audit next time).
