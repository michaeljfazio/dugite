//! Datum witness completeness validation (Rule 9c).
//!
//! This module implements two complementary datum-related Phase-1 rules that
//! together ensure the transaction's `plutus_data` witness set is exactly the
//! set required — no more and no fewer datums than necessary.
//!
//! ## Required datums (missing datum check)
//!
//! For every spending input whose UTxO carries a `DatumHash` AND whose
//! address is locked by a **Plutus** script (payment credential is
//! `Credential::Script` AND the script resolves to Plutus V1/V2/V3/V4 in
//! `ScriptsProvided`), the raw datum bytes MUST be present in
//! `tx.witness_set.plutus_data`.
//!
//! Inputs locked by **native scripts** are explicitly exempt even when their
//! UTxO carries a `DatumHash`.  Haskell's `getInputDataHashesTxBody`
//! (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs`) only sets the
//! required flag when `isSpendingPlutusScript addr` is true —
//! `lookupPlutusScript` returns `Nothing` for native scripts, and the
//! `DatumHash` case falls through to `_ -> ans` (no change).  The Haskell
//! source includes the comment: *"Though it is somewhat odd to allow native
//! scripts to include a datum, the Alonzo era already set the precedent with
//! datum hashes, and several dapp developers see this as a helpful feature."*
//!
//! Inputs that are NOT script-locked at all do not require a datum witness
//! even if their UTxO carries a `DatumHash`.
//! Inputs with `OutputDatum::InlineDatum` are also exempt: the datum is
//! already embedded in the UTxO and does not need to be re-supplied.
//!
//! ## Extra datums (spurious datum check)
//!
//! Any datum in `tx.witness_set.plutus_data` whose blake2b-256 hash is NOT
//! in the "needed" set makes the transaction malformed.  The needed set is:
//!
//! - All `DatumHash` values from script-locked spending input UTxOs, plus
//! - All `DatumHash` values from transaction outputs (outputs that declare a
//!   datum hash are allowed to have the datum supplied by the witness set).
//!
//! The second bullet mirrors Haskell's `allowedSupplementalDatums` which
//! permits output datum hashes as an additional "allowed" set in addition to
//! the strictly required input datum hashes.
//!
//! ## Reference: Haskell ledger
//!
//! Cardano.Ledger.Alonzo.Rules.Utxow:
//! - `missingRequiredDatums` — inputs with DatumHash but no matching witness datum
//! - `notAllowedSupplementalDatums` — witness datums whose hash is not in
//!   `requiredDatums ∪ allowedSupplementalDatums`

use std::collections::{HashMap, HashSet};

use dugite_primitives::credentials::Credential;
use dugite_primitives::hash::{DatumHash, Hash28, Hash32};
use dugite_primitives::transaction::{OutputDatum, Transaction};

use crate::utxo::UtxoLookup;

use super::ValidationError;

/// Hash a plutus datum by CBOR-encoding it and applying blake2b-256.
///
/// This mirrors Haskell's `hashData :: Data era -> DataHash (EraCrypto era)`
/// which is `blake2b_256(to_cbor(data))`.
///
/// **The encoding MUST be the canonical Plutus `Data` form** that
/// cardano-node/plutus produce — this is the encoding every on-chain datum
/// hash was computed over (`IntersectMBO/plutus:PlutusCore/Data.hs`). Its
/// salient, non-obvious rules:
///
///   - a **non-empty** `List`/`Constr`-fields array is **indefinite-length**
///     (`0x9f … 0xff`), an empty one is definite `0x80` (cborg's
///     `Serialise [a]` instance);
///   - a byte string longer than 64 bytes is chunked into ≤64-byte segments
///     wrapped in an indefinite-length byte string (`0x5f … 0xff`);
///   - integers outside `[-2^64, 2^64)` use the bignum tag form.
///
/// `dugite_serialization::encode_plutus_data` emits **definite-length** arrays
/// and a single un-chunked byte string, so its hash diverges from the
/// on-chain hash for any datum containing a non-empty list or a long byte
/// string (verified against mainnet datum `8b9604d4…`: on-chain bytes use
/// `d8799f…ffff`, dugite's old encoder produced `d87984…`). We therefore
/// delegate to the conformance-proven `dugite_uplc` `Data` encoder, which
/// reproduces the canonical form byte-for-byte (it backs the `serialiseData`
/// builtin and passes the full Plutus conformance suite).
fn hash_plutus_datum(datum: &dugite_primitives::transaction::PlutusData) -> Hash32 {
    let data = dugite_uplc::tx_info_populate::plutus_data_to_data(datum);
    // `Data::to_cbor` only fails on a writer error, which cannot happen for an
    // in-memory `Vec`; the fallback is unreachable but avoids an unwrap.
    let cbor = data
        .to_cbor()
        .unwrap_or_else(|_| dugite_serialization::encode_plutus_data(datum));
    dugite_primitives::hash::blake2b_256(&cbor)
}

/// Check datum witness completeness: Rule 9c.
///
/// Populates `errors` with:
/// - [`ValidationError::UnspendableUTxONoDatumHash`] for each script-locked
///   input with `OutputDatum::None` whose locking script is PlutusV1 or
///   PlutusV2 (CIP-0069: PlutusV3 inputs are exempt).
/// - [`ValidationError::MissingDatumWitness`] for each script-locked input
///   whose UTxO carries a `DatumHash` with no matching entry in the witness
///   plutus_data.
/// - [`ValidationError::ExtraDatumWitness`] for each witness datum whose hash
///   is not in the needed set (required input datums ∪ allowed supplemental
///   output datums).
///
/// `script_versions` maps each Plutus script hash (from witness set and
/// reference inputs) to its language version (1=V1, 2=V2, 3=V3).  This is
/// used to distinguish V1/V2 (which require a datum) from V3 (exempt per
/// CIP-0069).  Build it via `collateral::plutus_script_version_map`.
///
/// Called unconditionally from `run_phase1_rules` after input existence has
/// been confirmed (so UTxO lookups are safe).
pub(super) fn check_datum_witnesses(
    tx: &Transaction,
    utxo_set: &dyn UtxoLookup,
    script_versions: &HashMap<Hash28, u8>,
    errors: &mut Vec<ValidationError>,
) {
    // ------------------------------------------------------------------
    // Step 1 — Build the "needed" set of datum hashes.
    //
    // This is the union of two sub-sets:
    //   (a) required_datums: DatumHash from script-locked spending input UTxOs.
    //       These MUST have a matching witness datum.
    //   (b) allowed_supplemental: DatumHash declared on transaction outputs.
    //       Witness datums for these are allowed (but not required).
    //
    // `needed = required_datums ∪ allowed_supplemental`
    //
    // Any witness datum whose hash is outside `needed` is extraneous.
    // ------------------------------------------------------------------

    // (a) Required: datum hashes from script-locked spending input UTxOs.
    let mut required_datum_hashes: HashSet<DatumHash> = HashSet::new();

    for input in &tx.body.inputs {
        // If the UTxO is not found, Rule 2 (InputNotFound) will have already
        // fired.  We skip silently here to avoid duplicate/confusing errors.
        let Some(utxo) = utxo_set.lookup(input) else {
            continue;
        };

        // Only script-locked inputs need a datum witness.
        let is_script_locked = matches!(
            utxo.address.payment_credential(),
            Some(Credential::Script(_))
        );
        if !is_script_locked {
            continue;
        }

        // CIP-0069 / Haskell UnspendableUTxONoDatumHash:
        // Script-locked inputs with OutputDatum::None are only rejected when
        // the locking script is PlutusV1 or PlutusV2.  Native scripts and
        // PlutusV3 scripts are exempt.
        //
        // Mirrors Haskell:
        //   | Just lang <- spendingPlutusScriptLanguage addr
        //   , lang < PlutusV3 -> add to "missing datum" set
        //
        // `spendingPlutusScriptLanguage` returns Nothing for native scripts,
        // so version == 0 (not found in the Plutus map) means native script —
        // exempt from this rule.
        if matches!(utxo.datum, OutputDatum::None) {
            if let Some(Credential::Script(script_hash)) = utxo.address.payment_credential() {
                let version = script_versions.get(script_hash).copied().unwrap_or(0);
                // Only reject if the locking script IS a Plutus V1/V2 script.
                if version > 0 && version < 3 {
                    errors.push(ValidationError::UnspendableUTxONoDatumHash {
                        input: input.to_string(),
                        language: match version {
                            1 => "PlutusV1".to_string(),
                            2 => "PlutusV2".to_string(),
                            _ => format!("PlutusV{version}"),
                        },
                    });
                }
                // V3 or native script: NoDatum is fine — do not add to required_datum_hashes
            }
            continue; // NoDatum inputs never contribute to required_datum_hashes
        }

        // Only Plutus-script-locked inputs require a datum witness when the
        // UTxO carries a DatumHash.  Native-script-locked inputs are explicitly
        // exempt — Haskell's `getInputDataHashesTxBody` only adds to the
        // required set when `isSpendingPlutusScript addr` is true
        // (`lookupPlutusScript` returns `Nothing` for native scripts).
        //
        // Reference: IntersectMBO/cardano-ledger,
        // eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs,
        // `getInputDataHashesTxBody`, DatumHash branch:
        //   DatumHash dataHash
        //     | isSpendingPlutusScript addr -> (Set.insert dataHash hashSet, …)
        //   -- "Though it is somewhat odd to allow native scripts to include a
        //   --  datum, the Alonzo era already set the precedent with datum
        //   --  hashes, and several dapp developers see this as a helpful
        //   --  feature."
        //   _ -> ans
        //
        // InlineDatum outputs embed the datum in the UTxO itself — no witness
        // needed regardless of script type.
        if let OutputDatum::DatumHash(hash) = &utxo.datum {
            if let Some(Credential::Script(script_hash)) = utxo.address.payment_credential() {
                let version = script_versions.get(script_hash).copied().unwrap_or(0);
                // version == 0: native script or unknown → NOT required (exempt)
                // version >= 1: PlutusV1/V2/V3/V4 → REQUIRED
                if version > 0 {
                    required_datum_hashes.insert(*hash);
                }
            }
        }
    }

    // (b) Allowed supplemental: datum hashes from transaction outputs AND
    // reference inputs.  Cardano allows a transaction to supply datum
    // pre-images for:
    //   - outputs it creates (so future spenders have the datum bytes)
    //   - reference inputs it reads (the datum may be needed by scripts)
    //
    // These are optional and do not trigger MissingDatumWitness, but they
    // DO count toward the allowed set for the ExtraDatumWitness check.
    //
    // Haskell reference: `notAllowedSupplementalDatums` includes both
    // output datum hashes and reference input datum hashes in the
    // "allowed" set.
    let mut allowed_supplemental_hashes: HashSet<DatumHash> = HashSet::new();

    for output in &tx.body.outputs {
        if let OutputDatum::DatumHash(hash) = &output.datum {
            allowed_supplemental_hashes.insert(*hash);
        }
    }

    // Collateral return counts as an output too: Haskell's Babbage
    // `getBabbageSupplementalDataHashes` iterates `allSizedOutputsTxBodyF`
    // = regular outputs ++ collateral return
    // (eras/babbage/impl/src/Cardano/Ledger/Babbage/UTxO.hs).
    if let Some(collateral_return) = &tx.body.collateral_return {
        if let OutputDatum::DatumHash(hash) = &collateral_return.datum {
            allowed_supplemental_hashes.insert(*hash);
        }
    }

    // Reference inputs: their UTxO datum hashes are also supplemental.
    for ref_input in &tx.body.reference_inputs {
        if let Some(utxo) = utxo_set.lookup(ref_input) {
            if let OutputDatum::DatumHash(hash) = &utxo.datum {
                allowed_supplemental_hashes.insert(*hash);
            }
        }
    }

    // Union: all datum hashes that are acceptable in the witness set.
    let needed: HashSet<DatumHash> = required_datum_hashes
        .iter()
        .chain(allowed_supplemental_hashes.iter())
        .copied()
        .collect();

    // ------------------------------------------------------------------
    // Step 2 — Hash each witness datum and build the "supplied" set.
    //
    // We compute the hash of every datum in plutus_data so we can:
    //   (a) check that required hashes are covered (missing datum check), and
    //   (b) check that no supplied hash is outside `needed` (extra datum check).
    // ------------------------------------------------------------------
    // Datum hashes MUST be computed over each witness datum's **original** CBOR
    // bytes — Haskell memoises the raw bytes (`MemoBytes`/`Data`) and hashes
    // those, never a re-encoding. On-chain datums are frequently encoded
    // non-canonically (general `Constr` tag-102 form for small indices,
    // definite-length field arrays, non-minimal integers, …) that re-encoding
    // cannot reproduce, so a structural re-hash diverges from the on-chain
    // datum hash. Prefer the preserved per-element raw spans; fall back to a
    // canonical re-encode only for datums with no original bytes (e.g. ones the
    // node constructs itself).
    let supplied_hashes: HashSet<DatumHash> = match tx
        .witness_set
        .raw_plutus_data_cbor
        .as_deref()
        .and_then(dugite_serialization::plutus_data_element_spans)
        .filter(|spans| spans.len() == tx.witness_set.plutus_data.len())
    {
        Some(spans) => spans
            .iter()
            .map(|raw| dugite_primitives::hash::blake2b_256(raw))
            .collect(),
        None => tx
            .witness_set
            .plutus_data
            .iter()
            .map(hash_plutus_datum)
            .collect(),
    };

    // ------------------------------------------------------------------
    // Step 3 — Missing datum check.
    //
    // Every required datum hash MUST appear in the supplied set.
    // ------------------------------------------------------------------
    for hash in &required_datum_hashes {
        if !supplied_hashes.contains(hash) {
            errors.push(ValidationError::MissingDatumWitness(hash.to_hex()));
        }
    }

    // ------------------------------------------------------------------
    // Step 4 — Extra datum check.
    //
    // Every supplied datum hash MUST appear in the needed set.
    // ------------------------------------------------------------------
    for hash in &supplied_hashes {
        if !needed.contains(hash) {
            errors.push(ValidationError::ExtraDatumWitness(hash.to_hex()));
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{Address, BaseAddress};
    use dugite_primitives::credentials::Credential;
    use dugite_primitives::hash::{Hash28, Hash32};
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::transaction::{
        OutputDatum, PlutusData, Transaction, TransactionInput, TransactionOutput,
    };
    use dugite_primitives::value::Value;

    /// Byte-exact cross-validation against authoritative mainnet data.
    ///
    /// Mainnet datum `8b9604d4…` (creation tx
    /// `0e253dfe96521ab184e3c19d93f92c57f71a531ee36995335177a08e408abcc7`,
    /// fetched from Koios `datum_info`) has the structure
    /// `Constr(0, [Constr(0, [B(28), B(28), B("ADAX"), I(100)])])` and on-chain
    /// CBOR `d8799fd8799f581c0f1d…8a0f581c0c78…3c5f4441444158 1864 ff ff` — note
    /// the **indefinite-length** (`9f…ff`) Constr-field arrays. Its datum hash
    /// MUST be reproduced exactly. dugite's old definite-length encoder produced
    /// `dad1fe73…` instead, causing the Alonzo `MissingDatumWitness` /
    /// `ExtraDatumWitness` sync divergence.
    #[test]
    fn test_datum_hash_matches_mainnet_indefinite_encoding() {
        use num_bigint::BigInt;
        let policy: Vec<u8> =
            hex_to_bytes("0f1d8826255e871c201c91d40d9ea420369ff61b0475005cc2ad8a0f");
        let asset: Vec<u8> =
            hex_to_bytes("0c78f619e54a5d00e143f66181a2c500d0c394b38a10e86cd1a23c5f");
        let name: Vec<u8> = hex_to_bytes("41444158"); // "ADAX"
        let datum = PlutusData::Constr(
            0,
            vec![PlutusData::Constr(
                0,
                vec![
                    PlutusData::Bytes(policy),
                    PlutusData::Bytes(asset),
                    PlutusData::Bytes(name),
                    PlutusData::Integer(BigInt::from(100)),
                ],
            )],
        );
        let got = hash_plutus_datum(&datum).to_hex();
        assert_eq!(
            got, "8b9604d44ee907461ff45f5031da4a90241ecac5c99fd3b6d3b9c8219bd11ad5",
            "datum hash must match the on-chain (indefinite-length) Plutus encoding"
        );
    }

    fn hex_to_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The supplied-datum hash set must be built from each witness datum's
    /// **original** CBOR bytes, not a structural re-encoding.
    ///
    /// Vector: mainnet datum `5ff23baed5…` (creation tx
    /// `e253…`-class, Koios `datum_info`) encoded in the general `Constr` form
    /// (CBOR tag 102, `d866`) with a definite 6-field array for constructor
    /// index 0 — the canonical Plutus encoder emits tag 121 + indefinite array,
    /// which hashes differently. We deliberately put a *mismatched*
    /// `PlutusData::Integer(0)` in `plutus_data`: if the supplied-hash logic
    /// ever regresses to re-encoding `plutus_data`, the hash becomes
    /// `hash(I 0)` and the test fails with Missing+Extra. Hashing the preserved
    /// raw span reproduces the on-chain hash → no error.
    #[test]
    fn test_supplied_datum_hashed_over_original_noncanonical_bytes() {
        use num_bigint::BigInt;
        let datum_bytes = hex_to_bytes(
            "d866820086581ca3250750af6227b5a7dc689de94c83728a9d1d4029cc232d4a46f81e\
             1a041cdb40581c023cec350597bdf2a2b6945e62e0111d9808caf7a9353a2ab91e8beb\
             50534f434945545932354c4d4239323332581c63a3bc3807c6a51f85570ad9a82ed46b\
             db96feeabae6c4aa0526d4ed181e",
        );
        // On-chain datum hash = blake2b256 of the original bytes.
        let onchain_hash = dugite_primitives::hash::blake2b_256(&datum_bytes);
        assert_eq!(
            onchain_hash.to_hex(),
            "5ff23baed51ec22e9342ace92e6dd9976be5ded109575f58a8a2419f064818d0"
        );

        // `script_output_with_datum_hash` uses script hash [0xbb; 28] — mark it
        // as PlutusV1 so the datum witness is required (and checked via raw spans).
        let script_hash = Hash28::from_bytes([0xbbu8; 28]);
        let (utxo_set, input) = make_utxo(script_output_with_datum_hash(onchain_hash));

        // array(1) wrapping the datum — as stored in `raw_plutus_data_cbor`.
        let mut raw_array = vec![0x81u8];
        raw_array.extend_from_slice(&datum_bytes);

        // Structural plutus_data deliberately mismatches the raw bytes.
        let mut tx = make_tx(
            vec![input],
            vec![],
            vec![],
            vec![PlutusData::Integer(BigInt::from(0))],
        );
        tx.witness_set.raw_plutus_data_cbor = Some(raw_array);

        // PlutusV1 → datum witness is required; the raw-span path must reproduce
        // the on-chain hash exactly.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 1u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);
        assert!(
            errors.is_empty(),
            "datum hash must be computed over the original (tag-102) bytes; got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build a UTxO set containing a single entry and return it along with the
    /// `TransactionInput` key that addresses that entry.
    fn make_utxo(output: TransactionOutput) -> (crate::utxo::UtxoSet, TransactionInput) {
        let mut utxo_set = crate::utxo::UtxoSet::new();
        let input = TransactionInput {
            transaction_id: Hash32::from_bytes([0xaau8; 32]),
            index: 0,
        };
        utxo_set.insert(input.clone(), output);
        (utxo_set, input)
    }

    /// Build a minimal `Transaction` whose only populated fields are those
    /// examined by `check_datum_witnesses`.
    fn make_tx(
        inputs: Vec<TransactionInput>,
        outputs: Vec<TransactionOutput>,
        reference_inputs: Vec<TransactionInput>,
        plutus_data: Vec<PlutusData>,
    ) -> Transaction {
        Transaction::empty_with_hash(Hash32::ZERO).with_parts(
            inputs,
            outputs,
            reference_inputs,
            plutus_data,
        )
    }

    // Helper method — attach fields to a `Transaction::empty_with_hash` return
    // value without requiring the full builder chain.
    trait WithParts {
        fn with_parts(
            self,
            inputs: Vec<TransactionInput>,
            outputs: Vec<TransactionOutput>,
            reference_inputs: Vec<TransactionInput>,
            plutus_data: Vec<PlutusData>,
        ) -> Self;
    }

    impl WithParts for Transaction {
        fn with_parts(
            mut self,
            inputs: Vec<TransactionInput>,
            outputs: Vec<TransactionOutput>,
            reference_inputs: Vec<TransactionInput>,
            plutus_data: Vec<PlutusData>,
        ) -> Self {
            self.body.inputs = inputs;
            self.body.outputs = outputs;
            self.body.reference_inputs = reference_inputs;
            self.witness_set.plutus_data = plutus_data;
            self
        }
    }

    /// Build a script-locked `TransactionOutput` with a `DatumHash` attachment.
    fn script_output_with_datum_hash(datum_hash: Hash32) -> TransactionOutput {
        TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0xbbu8; 28])),
                stake: Credential::VerificationKey(Hash28::from_bytes([0xccu8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::DatumHash(datum_hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Build a script-locked `TransactionOutput` with an `InlineDatum`.
    fn script_output_with_inline_datum(data: PlutusData) -> TransactionOutput {
        TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(Hash28::from_bytes([0xbbu8; 28])),
                stake: Credential::VerificationKey(Hash28::from_bytes([0xccu8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::InlineDatum {
                data,
                raw_cbor: None,
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Build a VKey-locked `TransactionOutput` (no datum).
    fn vkey_output_no_datum() -> TransactionOutput {
        TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0xddu8; 28])),
                stake: Credential::VerificationKey(Hash28::from_bytes([0xeeu8; 28])),
            }),
            value: Value::lovelace(5_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    /// Compute the datum hash that `check_datum_witnesses` will compute for a
    /// given `PlutusData` (mirrors `hash_plutus_datum` in the production code).
    fn datum_hash_of(data: &PlutusData) -> Hash32 {
        hash_plutus_datum(data)
    }

    /// A simple, unique integer datum suitable for use in tests.
    fn int_datum(n: i128) -> PlutusData {
        PlutusData::Integer(num_bigint::BigInt::from(n))
    }

    // -----------------------------------------------------------------------
    // Test 1 — script-locked Plutus input with matching datum witness → no error
    // -----------------------------------------------------------------------

    #[test]
    fn test_script_input_datum_present() {
        // Script hash used by `script_output_with_datum_hash`.
        let script_hash = Hash28::from_bytes([0xbbu8; 28]);
        // Datum to witness
        let datum = int_datum(42);
        let hash = datum_hash_of(&datum);

        // UTxO: script-locked (Plutus V1), DatumHash in output
        let utxo_output = script_output_with_datum_hash(hash);
        let (utxo_set, input) = make_utxo(utxo_output);

        let tx = make_tx(vec![input], vec![], vec![], vec![datum]);

        // script_versions: the locking script is PlutusV1 → datum witness required.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 1u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingDatumWitness(_))),
            "expected no MissingDatumWitness, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2 — Plutus-script-locked input with missing datum witness → error
    // -----------------------------------------------------------------------

    #[test]
    fn test_script_input_datum_missing() {
        let script_hash = Hash28::from_bytes([0xbbu8; 28]);
        // Datum NOT supplied in witness
        let datum = int_datum(99);
        let hash = datum_hash_of(&datum);

        let utxo_output = script_output_with_datum_hash(hash);
        let (utxo_set, input) = make_utxo(utxo_output);

        // witness plutus_data is empty — no datum supplied
        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        // script_versions: PlutusV1 → datum witness IS required.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 1u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingDatumWitness(_))),
            "expected MissingDatumWitness, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3 — inline datum: no witness entry needed
    // -----------------------------------------------------------------------

    #[test]
    fn test_inline_datum_no_witness_needed() {
        // Output carries an inline datum — no witness required
        let data = int_datum(7);
        let utxo_output = script_output_with_inline_datum(data);
        let (utxo_set, input) = make_utxo(utxo_output);

        // witness plutus_data is empty — none supplied
        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(),
            &mut errors,
        );

        assert!(
            errors.is_empty(),
            "expected no errors for inline datum, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4 — VKey-locked input: datum not required even without witness
    // -----------------------------------------------------------------------

    #[test]
    fn test_non_script_input_no_datum() {
        let utxo_output = vkey_output_no_datum();
        let (utxo_set, input) = make_utxo(utxo_output);

        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(),
            &mut errors,
        );

        assert!(
            errors.is_empty(),
            "expected no errors for vkey-locked input, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5 — datum in witness not referenced by any input/output → error
    // -----------------------------------------------------------------------

    #[test]
    fn test_extra_datum_is_hard_error() {
        // UTxO has no datum at all; the witness carries a spurious datum.
        let utxo_output = vkey_output_no_datum();
        let (utxo_set, input) = make_utxo(utxo_output);

        let spurious_datum = int_datum(123);
        let tx = make_tx(vec![input], vec![], vec![], vec![spurious_datum]);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(),
            &mut errors,
        );

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ExtraDatumWitness(_))),
            "expected ExtraDatumWitness, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6 — output datum hash supplemental: witness is allowed, not extra
    // -----------------------------------------------------------------------

    #[test]
    fn test_output_datum_hash_supplemental() {
        // Transaction output declares a datum hash — the datum in the witness
        // is "supplemental" (allowed but not required).  Must NOT produce
        // ExtraDatumWitness.
        let datum = int_datum(55);
        let hash = datum_hash_of(&datum);

        // Input is VKey-locked (no datum witness required from input side)
        let utxo_output = vkey_output_no_datum();
        let (utxo_set, input) = make_utxo(utxo_output);

        // Transaction output carries that datum hash
        let tx_output = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x11u8; 28])),
                stake: Credential::VerificationKey(Hash28::from_bytes([0x22u8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::DatumHash(hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        let tx = make_tx(vec![input], vec![tx_output], vec![], vec![datum]);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(),
            &mut errors,
        );

        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::ExtraDatumWitness(_))),
            "expected no ExtraDatumWitness for supplemental output datum, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7 — reference input datum cannot satisfy spending input requirement
    // -----------------------------------------------------------------------

    #[test]
    fn test_ref_input_datum_supplemental_only() {
        // Both the spending input UTxO and the reference input UTxO carry the
        // same DatumHash.  The datum is NOT in the witness.
        //
        // Because the spending input is locked by a PLUTUS script,
        // MissingDatumWitness must still fire — the reference input datum hash
        // only makes the witness *allowed*, it does not satisfy the *required*
        // set.
        let script_hash = Hash28::from_bytes([0xbbu8; 28]);
        let datum = int_datum(77);
        let hash = datum_hash_of(&datum);

        // Spending input: Plutus-script-locked, DatumHash
        let spend_output = script_output_with_datum_hash(hash);
        let (mut utxo_set, spend_input) = make_utxo(spend_output);

        // Reference input: also carries same DatumHash (but lives at a
        // different input reference so it is distinct from the spending input)
        let ref_tx_input = TransactionInput {
            transaction_id: Hash32::from_bytes([0x55u8; 32]),
            index: 0,
        };
        let ref_output = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x33u8; 28])),
                stake: Credential::VerificationKey(Hash28::from_bytes([0x44u8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::DatumHash(hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        utxo_set.insert(ref_tx_input.clone(), ref_output);

        // No witness datum supplied
        let tx = make_tx(
            vec![spend_input],
            vec![],
            vec![ref_tx_input],
            vec![], // datum NOT supplied
        );

        // script_versions: PlutusV1 → datum witness IS required.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 1u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingDatumWitness(_))),
            "expected MissingDatumWitness even though ref-input carries the same hash, got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8 — two script inputs with different hashes; only one datum supplied
    // -----------------------------------------------------------------------

    #[test]
    fn test_multiple_script_inputs_one_missing() {
        // Script hash used by `script_output_with_datum_hash`.
        let script_hash = Hash28::from_bytes([0xbbu8; 28]);

        let datum_a = int_datum(10);
        let hash_a = datum_hash_of(&datum_a);

        let datum_b = int_datum(20);
        let hash_b = datum_hash_of(&datum_b);

        // Two distinct UTxOs, both Plutus-script-locked, each with a different DatumHash
        let output_a = script_output_with_datum_hash(hash_a);
        let output_b = script_output_with_datum_hash(hash_b);

        let mut utxo_set = crate::utxo::UtxoSet::new();
        let input_a = TransactionInput {
            transaction_id: Hash32::from_bytes([0x01u8; 32]),
            index: 0,
        };
        let input_b = TransactionInput {
            transaction_id: Hash32::from_bytes([0x02u8; 32]),
            index: 0,
        };
        utxo_set.insert(input_a.clone(), output_a);
        utxo_set.insert(input_b.clone(), output_b);

        // Only datum_a is in the witness; datum_b is absent
        let tx = make_tx(vec![input_a, input_b], vec![], vec![], vec![datum_a]);

        // script_versions: both inputs locked by the same PlutusV1 script hash.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 1u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        let missing: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::MissingDatumWitness(_)))
            .collect();

        assert_eq!(
            missing.len(),
            1,
            "expected exactly one MissingDatumWitness (for datum_b), got: {errors:?}"
        );

        // Confirm the missing hash is hash_b's hex representation
        if let ValidationError::MissingDatumWitness(hex) = missing[0] {
            assert_eq!(
                hex,
                &hash_b.to_hex(),
                "wrong hash in MissingDatumWitness error"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 9 — CIP-0069: V1 script-locked input with NoDatum → error
    //
    // A PlutusV1 script-locked spending input with OutputDatum::None must be
    // rejected with UnspendableUTxONoDatumHash.  V1/V2 scripts require a datum
    // (either DatumHash witness or InlineDatum).
    // -----------------------------------------------------------------------

    #[test]
    fn test_v1_script_no_datum_rejected() {
        // Script hash for a fictional PlutusV1 script.
        let script_hash = Hash28::from_bytes([0x11u8; 28]);

        // UTxO: script-locked, OutputDatum::None.
        let utxo_output = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(script_hash),
                stake: Credential::VerificationKey(Hash28::from_bytes([0x22u8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let (utxo_set, input) = make_utxo(utxo_output);

        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        // script_versions: the locking script is V1.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 1u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::UnspendableUTxONoDatumHash { language, .. }
                if language == "PlutusV1"
            )),
            "expected UnspendableUTxONoDatumHash(PlutusV1), got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 10 — CIP-0069: V3 script-locked input with NoDatum → allowed
    //
    // A PlutusV3 script-locked spending input with OutputDatum::None must NOT
    // produce UnspendableUTxONoDatumHash — V3 is exempt per CIP-0069.
    // -----------------------------------------------------------------------

    #[test]
    fn test_v3_script_no_datum_allowed() {
        // Script hash for a fictional PlutusV3 script.
        let script_hash = Hash28::from_bytes([0x33u8; 28]);

        // UTxO: script-locked, OutputDatum::None.
        let utxo_output = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(script_hash),
                stake: Credential::VerificationKey(Hash28::from_bytes([0x44u8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let (utxo_set, input) = make_utxo(utxo_output);

        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        // script_versions: the locking script is V3 — exempt from datum requirement.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 3u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            !errors.iter().any(|e| matches!(
                e,
                ValidationError::UnspendableUTxONoDatumHash { .. }
            )),
            "expected no UnspendableUTxONoDatumHash for V3 input (CIP-0069 exempt), got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 11 — Native-script-locked input with DatumHash: NO witness required
    //
    // Regression test for mainnet false-positive divergence (epoch ~434, Babbage
    // PV8).  Txs like `af4a50e599f6...` spend native-script-locked UTxOs that
    // carry a `DatumHash` WITHOUT providing the datum preimage in the witness
    // set — Haskell accepts them because `getInputDataHashesTxBody` in
    // `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/UTxO.hs` gates the required
    // set on `isSpendingPlutusScript addr` (which returns `false` for native
    // scripts).  dugite must NOT emit `MissingDatumWitness` for these inputs.
    //
    // Haskell source comment on the native-script DatumHash case:
    //   "Though it is somewhat odd to allow native scripts to include a datum,
    //    the Alonzo era already set the precedent with datum hashes, and several
    //    dapp developers see this as a helpful feature."
    // -----------------------------------------------------------------------

    #[test]
    fn test_native_script_datum_hash_no_witness_required() {
        // Script hash for a native script (all_of or similar).
        let script_hash = Hash28::from_bytes([0xaau8; 28]);

        // UTxO at a native-script address that carries a DatumHash.
        // The datum preimage is deliberately NOT in the witness set.
        let datum = int_datum(42);
        let datum_hash = datum_hash_of(&datum);

        let utxo_output = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(script_hash),
                stake: Credential::VerificationKey(Hash28::from_bytes([0xbbu8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::DatumHash(datum_hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let (utxo_set, input) = make_utxo(utxo_output);

        // Witness set: the native script is present but NO plutus_data.
        // script_versions map does NOT contain an entry for this hash (or maps
        // to version 0), reflecting that native scripts are absent from
        // `plutus_script_version_map`.
        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        // Version map is empty — native script hash not in Plutus map.
        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(),
            &mut errors,
        );

        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingDatumWitness(_))),
            "native-script-locked input with DatumHash must NOT require datum witness; got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12 — Plutus V2 script-locked input with DatumHash: witness required
    //
    // Positive control: a Plutus V2 script-locked input with DatumHash and no
    // witness datum MUST produce MissingDatumWitness.  This ensures the native-
    // script exemption does not accidentally exempt Plutus scripts.
    // -----------------------------------------------------------------------

    #[test]
    fn test_plutus_v2_datum_hash_witness_required() {
        let script_hash = Hash28::from_bytes([0xddu8; 28]);

        let datum = int_datum(77);
        let datum_hash = datum_hash_of(&datum);

        let utxo_output = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::Script(script_hash),
                stake: Credential::VerificationKey(Hash28::from_bytes([0xeeu8; 28])),
            }),
            value: Value::lovelace(2_000_000),
            datum: OutputDatum::DatumHash(datum_hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let (utxo_set, input) = make_utxo(utxo_output);

        // Witness plutus_data is empty — datum preimage absent.
        let tx = make_tx(vec![input], vec![], vec![], vec![]);

        // script_versions: PlutusV2 script at this hash.
        let mut script_versions = std::collections::HashMap::new();
        script_versions.insert(script_hash, 2u8);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingDatumWitness(_))),
            "PlutusV2-locked input with DatumHash and absent witness MUST produce MissingDatumWitness; got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Collateral return datum hash is an ALLOWED supplemental target:
    // Haskell's Babbage `getBabbageSupplementalDataHashes` iterates
    // `allSizedOutputsTxBodyF` = regular outputs ++ collateral return
    // (eras/babbage/impl/src/Cardano/Ledger/Babbage/UTxO.hs). A witness
    // datum matching ONLY the collateral-return's datum hash must NOT be
    // flagged ExtraDatumWitness.
    // -----------------------------------------------------------------------

    #[test]
    fn test_collateral_return_datum_hash_supplemental() {
        let datum = int_datum(77);
        let hash = datum_hash_of(&datum);

        let utxo_output = vkey_output_no_datum();
        let (utxo_set, input) = make_utxo(utxo_output);

        let collateral_return = TransactionOutput {
            address: Address::Base(BaseAddress {
                network: NetworkId::Testnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0x33u8; 28])),
                stake: Credential::VerificationKey(Hash28::from_bytes([0x44u8; 28])),
            }),
            value: Value::lovelace(5_000_000),
            datum: OutputDatum::DatumHash(hash),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };

        let mut tx = make_tx(vec![input], vec![], vec![], vec![datum]);
        tx.body.collateral_return = Some(collateral_return);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(),
            &mut errors,
        );

        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::ExtraDatumWitness(_))),
            "collateral-return datum hash must be an allowed supplemental target; got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // The rejection side of the native-script exemption: a witness datum whose
    // hash matches ONLY a native-script-locked input's DatumHash (not any
    // output / collateral return / reference input) is NOT allowed. Haskell's
    // `missingRequiredDatums` computes supplemental = txHashes − inputHashes;
    // the native-script input contributes nothing to inputHashes, and
    // `getSupplementalDataHashes` never includes spending-input UTxO hashes →
    // NotAllowedSupplementalDatums. The #751-era fix must narrow REQUIRED
    // without widening ALLOWED.
    // -----------------------------------------------------------------------

    #[test]
    fn test_native_script_supplied_datum_rejected_as_supplemental() {
        let datum = int_datum(99);
        let hash = datum_hash_of(&datum);

        // Native-script-locked input (script absent from script_versions map)
        // whose UTxO carries the datum hash.
        let utxo_output = script_output_with_datum_hash(hash);
        let (utxo_set, input) = make_utxo(utxo_output);

        // The witness SUPPLIES the datum preimage anyway; no output declares it.
        let tx = make_tx(vec![input], vec![], vec![], vec![datum]);

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(
            &tx,
            &utxo_set,
            &std::collections::HashMap::new(), // native: not in Plutus map
            &mut errors,
        );

        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::MissingDatumWitness(_))),
            "native-script input must not REQUIRE a datum witness; got: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ExtraDatumWitness(_))),
            "unsolicited datum for a native-script input is NotAllowedSupplementalDatums \
             in Haskell and must stay rejected; got: {errors:?}"
        );
    }

    // -----------------------------------------------------------------------
    // PIN — real mainnet tx af4a50e599f6cd83c3680c3255b91bedc7776e7de090bb37
    // 0f0034ef023eb7e3 (slot 102,975,745, epoch ~434, Babbage PV8), one of
    // the 8 confirmed on-chain txs the pre-fix code falsely rejected with
    // MissingDatumWitness during the v2.0.5 mainnet sync.
    //
    // The tx spends:
    //   • 0a7c723b…#4 — vkey-locked (payment cred 5c27059e…), no datum
    //   • 62cfc1b2…#1 — NATIVE-script-locked enterprise address (script
    //     hash 279b2518…), UTxO carries DatumHash 6cdd5320… whose preimage
    //     is deliberately ABSENT from the witness set
    // The witness set's only datum is the preimage of an OUTPUT's datum
    // hash (supplemental — allowed). Haskell accepted this tx on-chain:
    // `getInputDataHashesTxBody` requires datums only for Plutus-locked
    // inputs (`isSpendingPlutusScript`), so the native-script input
    // contributes nothing to the required set. The full datum check must
    // produce ZERO datum-class errors.
    // -----------------------------------------------------------------------

    const MAINNET_TX_AF4A50E5: &str = include_str!("fixtures/tx-af4a50e5.hex");

    #[test]
    fn pin_mainnet_babbage_native_script_datum_hash_tx_af4a50e5() {
        let s = MAINNET_TX_AF4A50E5.trim();
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect();
        let tx = dugite_serialization::decode::decode_transaction(5, &bytes)
            .expect("decode real Babbage tx");
        assert_eq!(tx.body.inputs.len(), 2, "fixture spends exactly 2 inputs");

        // Reconstruct the spent UTxOs from chain data (Koios tx_info):
        let mut utxo_set = crate::utxo::UtxoSet::new();
        for input in &tx.body.inputs {
            let output = if input.index == 1 {
                // 62cfc1b2…#1 — the native-script-locked UTxO with DatumHash.
                TransactionOutput {
                    address: Address::Enterprise(dugite_primitives::address::EnterpriseAddress {
                        network: NetworkId::Mainnet,
                        payment: Credential::Script(
                            Hash28::from_hex(
                                "279b2518634a7402405b8df3d52c19e26bee2792f770fc0bb536bc4b",
                            )
                            .unwrap(),
                        ),
                    }),
                    value: Value::lovelace(1_198_180),
                    datum: OutputDatum::DatumHash(
                        Hash32::from_hex(
                            "6cdd5320fb8f463541458270bfef9a6444f9c7f3fa06513607d90c7f0fa68808",
                        )
                        .unwrap(),
                    ),
                    script_ref: None,
                    is_legacy: false,
                    raw_cbor: None,
                }
            } else {
                // 0a7c723b…#4 — vkey-locked, no datum.
                TransactionOutput {
                    address: Address::Base(BaseAddress {
                        network: NetworkId::Mainnet,
                        payment: Credential::VerificationKey(
                            Hash28::from_hex(
                                "5c27059e275c30b7307ba74d0823e049a6b642f1da9a1c9732a44f96",
                            )
                            .unwrap(),
                        ),
                        stake: Credential::VerificationKey(Hash28::from_bytes([0u8; 28])),
                    }),
                    value: Value::lovelace(8_754_014_993),
                    datum: OutputDatum::None,
                    script_ref: None,
                    is_legacy: false,
                    raw_cbor: None,
                }
            };
            utxo_set.insert(input.clone(), output);
        }

        // The REAL production path for the Plutus map: the tx's witness set
        // carries the native script, which must NOT enter the map.
        let script_versions = crate::validation::plutus_script_version_map(&tx, &utxo_set);
        assert!(
            !script_versions.contains_key(
                &Hash28::from_hex("279b2518634a7402405b8df3d52c19e26bee2792f770fc0bb536bc4b")
                    .unwrap()
            ),
            "native script must not appear in the Plutus version map"
        );

        let mut errors: Vec<ValidationError> = vec![];
        check_datum_witnesses(&tx, &utxo_set, &script_versions, &mut errors);

        assert!(
            errors.is_empty(),
            "on-chain tx af4a50e5… must produce zero datum-class errors \
             (pre-fix: false MissingDatumWitness for 6cdd5320…); got: {errors:?}"
        );
    }
}
