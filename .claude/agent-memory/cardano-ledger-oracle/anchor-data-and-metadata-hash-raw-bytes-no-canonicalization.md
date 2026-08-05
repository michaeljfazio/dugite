---
name: anchor-data-and-metadata-hash-raw-bytes-no-canonicalization
description: cardano-cli `hash anchor-data`, `governance drep metadata-hash`, and `stake-pool metadata-hash` all compute blake2b_256 over the RAW uncanonicalized input bytes — three different Haskell code paths, byte-identical results for identical input, no CIP-119 JSON-LD canonicalization anywhere. Stake-pool adds a validation GATE (512-byte cap + Aeson schema check) that does not alter the hash input.
metadata:
  type: reference
---

Live-verified 2026-08-05 against `IntersectMBO/cardano-cli` + `IntersectMBO/cardano-api` (`master`).

## `hash anchor-data`

`cardano-cli/src/Cardano/CLI/EraIndependent/Hash/Run.hs:47-85`, `runHashAnchorDataCmd`. Reads bytes verbatim (`readFileCli` for binary/text file, `Text.encodeUtf8` for inline text, `getByteStringFromURL` for URL — no parsing of any kind), wraps as `L.AnchorData bytes`, then `hash = L.hashAnnotated anchorData`. `AnchorData`'s `SafeToHash`/`HashAnnotated` machinery (ledger-side `Cardano.Ledger.Hashes`) computes `Blake2b_256` (`type HASH = Hash.Blake2b_256`, `Hashes.hs:122`) over those exact bytes via the standard `SafeHash` identity-`originalBytes` path. **No validation, no length cap, no JSON parsing — accepts any bytes for any URL/file/text.** This is the actual mechanism used on-chain: the anchor's `dataHash` field committed in a governance-action/DRep-registration certificate is exactly this value.

## `governance drep metadata-hash`

`cardano-cli/src/Cardano/CLI/EraBased/Governance/DRep/Run.hs:222-256`, `runGovernanceDRepMetadataHashCmd`. Calls `hashDRepMetadata` (`cardano-api/src/Cardano/Api/Certificate/Internal/DRepMetadata.hs`):
```haskell
hashDRepMetadata bs =
  let md = DRepMetadata bs
      mdh = DRepMetadataHash (Crypto.hashWith id bs)
   in (md, mdh)
```
`DRepMetadata` is literally `newtype DRepMetadata = DRepMetadata { unDRepMetadata :: ByteString }` — despite the function's "decoded metadata" doc comment, there is **no actual JSON parsing, no CIP-119 JSON-LD canonicalization, no validation of any kind**. `Crypto.hashWith id bs` = `Blake2b_256` (`HASH` type) directly over the raw bytes. Different Haskell code path from `hash anchor-data` (no `SafeHash`/ledger `AnchorData` type involved at all — this is a pure cardano-api-side newtype), but the SAME formula (`Blake2b_256` of raw bytes) — so results are byte-identical for identical input.

## `stake-pool metadata-hash`

`cardano-cli/src/Cardano/CLI/EraBased/StakePool/Run.hs:186-224` → `validateAndHashStakePoolMetadata` (`cardano-api/src/Cardano/Api/Certificate/Internal/StakePoolMetadata.hs`):
```haskell
validateAndHashStakePoolMetadata bs
  | BS.length bs <= 512 = do
      md <- first StakePoolMetadataJsonDecodeError (Aeson.eitherDecodeStrict' bs)
      let mdh = StakePoolMetadataHash (Crypto.hashWith id bs)
      return (md, mdh)
  | otherwise = Left $ StakePoolMetadataInvalidLengthError 512 (BS.length bs)
```
This DOES gate: hard `<= 512` byte cap, and Aeson JSON-decode requiring `name` (<=50 chars), `description` (<=255 chars), `ticker` (3-5 chars), `homepage` present — command fails with an error if any check fails. But the hash itself is still `Crypto.hashWith id bs` over the SAME raw `bs`, not a re-serialization of the parsed `StakePoolMetadata` record (there is no `ToJSON StakePoolMetadata` instance at all — explicitly `-- TODO: instance ToJSON StakePoolMetadata where`, never implemented, so re-serialization is structurally impossible even by accident). Validation is a pure client-side sanity gate; it never changes what gets hashed.

## Summary

All three commands: `Blake2b_256(raw_input_bytes)`, zero canonicalization, zero re-serialization. Three genuinely different Haskell implementations (`L.hashAnnotated`/`SafeHash` vs `hashDRepMetadata` vs `validateAndHashStakePoolMetadata`) that happen to produce byte-identical digests for byte-identical input — NOT one shared function despite the near-identical behavior. Only stake-pool metadata enforces any pre-hash validation (512-byte cap + required-field JSON schema); DRep metadata and generic anchor-data accept arbitrary bytes unconditionally.

## Rust Translation Notes (Dugite)

Any Dugite CLI command computing an anchor/metadata hash needs exactly `blake2b_256(bytes)` with NO JSON parsing/canonicalization step — a byte-for-byte reproduction of whatever the user supplied (file contents, inline text as UTF-8, or URL response body, unmodified). If Dugite wants byte-parity with cardano-cli's stake-pool command specifically, add the 512-byte cap + `name`/`description`/`ticker`/`homepage` Aeson-style schema check as a pre-hash REJECT gate only — never let that validation step touch the bytes that get hashed.
