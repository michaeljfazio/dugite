---
name: plutus-script-hash-retains-one-cbor-bstr-wrapper
description: hashPlutusScript hashes tag <> CBOR-bstr(flat_bytes), NOT tag <> flat_bytes — the "double CBOR encoding" of Plutus scripts is real and load-bearing for the hash. Empirically verified against a real mainnet PlutusV2 script hash.
metadata:
  type: reference
---

Live-verified 2026-08-05 against `IntersectMBO/cardano-ledger` + `IntersectMBO/cardano-api` (`master`) AND empirically confirmed against a real mainnet script via Koios. Resolves a genuine ambiguity: does a Plutus script's ScriptHash/PolicyId hash the bare flat-encoded UPLC bytes, or the flat bytes still wrapped in one CBOR byte-string header? **Answer: the wrapped form. The extra CBOR bstr header bytes are part of the hash preimage.**

## The formula

`ScriptHash = blake2b_224(tag_byte <> X)` where `X` = the CBOR byte-string encoding of the flat-encoded UPLC program (i.e. `0x58/0x59/... <len> <flat_bytes>`), **not** `flat_bytes` alone. Tags: native=0x00, V1=0x01, V2=0x02, V3=0x03 (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs` lines 473/493/513), V4=0x04 (forward-declared, unreleased).

This is the well-known "Plutus scripts are double CBOR encoded" phenomenon: the witness-set array element (CDDL `plutus_v2_script = bytes`) is itself `bstr(bstr(flat_bytes))` on the wire. `Cardano.Ledger.Plutus.Language.PlutusBinary`'s `DecCBOR` (`libs/cardano-ledger-core/.../Language.hs:250-261`, `deriving newtype DecCBOR` off `ShortByteString`'s standard `decodeBytes`) strips only the OUTER of the two wraps when parsing that one array element — `unPlutusBinary` ends up holding `bstr(flat_bytes)`, i.e. **one wrap still attached**. `SafeToHash PlutusBinary`'s `originalBytes = fromShort` returns those bytes unchanged (`libs/cardano-ledger-core/.../Language.hs:260-261`), and `hashPlutusScript` (`Language.hs:200-204`) hashes `tag <> originalBytes` directly — no further stripping anywhere in the chain.

## cardano-api side: `removePlutusScriptDoubleEncoding`

`cardano-api/src/Cardano/Api/Plutus/Internal/Script.hs:497-507`. Normalizes a `PlutusScript`'s stored bytes (read from a text-envelope's hex-decoded `cborHex`, before any CBOR parsing) toward **exactly one** remaining CBOR-bstr layer, tolerating three possible input shapes:
- 0 wraps (bare flat bytes) → outer `CBOR.decodeBytes` fails → returned unchanged (bare flat bytes stay bare — this shape does NOT reproduce the on-chain hash if actually used as-is; in practice real script files are never 0-wrap).
- 1 wrap (canonical/expected shape) → outer decode succeeds, but decoding its content AGAIN as CBOR-bstr fails → **returns the ORIGINAL bytes unchanged, i.e. still 1-wrapped** (not the stripped inner payload — read this function carefully, it's easy to misread as "always strip one layer").
- 2 wraps (legacy tooling bug, some old `plutus-tx`/serialise-based exporters) → both decode attempts succeed → returns the once-stripped payload, i.e. down to 1 wrap.

Net effect in every reachable real-world case: the bytes fed into `Plutus.PlutusBinary` (`ApiScript.hs:1042-1071`, `hashScript`) end up **1-wrap CBOR-bstr'd**, matching the ledger-side invariant exactly. `serialiseToCBOR (PlutusScriptSerialised sbs) = SBS.fromShort sbs` (`ApiScript.hs:1108`) is a deliberate identity — cardano-api's explicit comment: "the CBOR serialisation is just the raw bytes... we don't do any additional transformation." So a script's on-disk `cborHex` in a text envelope literally IS `hex(CBOR-bstr(flat_bytes))` — decode the hex, and you already have exactly what gets hashed (after prepending the 1-byte tag). No extra unwrap needed by a consumer that already has cborHex bytes in hand and just wants to hash them.

## Empirical proof (2026-08-05, mainnet)

Script hash `8d73f125395466f1d68570447e4f4b87cd633c6728f3802b2dcfca20` (PlutusV2, via Koios `plutus_script_list`/`script_info`). Koios' `bytes` field = `5914690100003323...` (5228 bytes total, `59 1469` = CBOR bstr header declaring 5225-byte payload; payload starts `01 00 00 33...` — `01 00 00` is the UPLC flat version tag, i.e. genuinely flat bytes with no further CBOR nesting inside).

- `blake2b_224(0x02 <> full_5228_bytes)` → `8d73f125395466f1d68570447e4f4b87cd633c6728f3802b2dcfca20` — **matches**.
- `blake2b_224(0x02 <> inner_5225_bytes_with_header_stripped)` → `b4da575b...` — does **not** match.

Confirms the wrapped form is what's actually hashed on real mainnet, not a source-reading artifact. Koios' reported `size: 5228` for this script also equals the full wrapped-byte length, corroborating [[getreferenceinputssize-and-refscriptsize-nondistinct-sum]] (which sums exactly this same `originalBytesSize`).

## Rust Translation Notes (Dugite)

Any Dugite code computing a Plutus ScriptHash — from a witness-set entry, a `TxOut.script_ref`, or a standalone cardano-cli-compatible `hash script`/`transaction policyid` command — must hash `tag_byte <> raw_captured_wire_bytes_of_the_single_remaining_bstr_wrap`, not `tag_byte <> decoded_flat_program_bytes`. If Dugite's CBOR decoder for `plutus_v1/v2/v3_scripts` witness-set arrays fully unwraps to bare flat bytes (i.e. treats each array element as double-wrapped and strips both layers), the resulting ScriptHash/PolicyId will silently diverge from cardano-node for every Plutus script — this is exactly the class of bug flagged in [[project_dugite_native_script_hash_audit_2026_07_06]] for native scripts; the Plutus case has the identical "must capture raw wire bytes, one specific layer, never re-encode or over-strip" requirement, just with an extra wrap to account for. Verify: decode a real mainnet/preview PlutusV2 witness, capture what your decoder stores as "the script bytes," and check its length matches the on-chain script's Koios-reported `size` (a mismatch by exactly a few bytes — the CBOR bstr header size — is the fingerprint of this exact bug).
