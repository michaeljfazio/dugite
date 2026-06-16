use crate::cbor::*;
use dugite_primitives::hash::Hash32;
use dugite_primitives::transaction::*;

/// Encode a script reference.
///
/// Wire format (PostAlonzo output key 3 value, inside tag(24) bstr):
///   `array(2) [variant_tag, script_bytes]`
///
/// Variant tags:
///   0 = NativeScript, 1 = PlutusV1, 2 = PlutusV2, 3 = PlutusV3, 4 = PlutusV4 (Dijkstra)
pub fn encode_script_ref(script_ref: &ScriptRef) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    match script_ref {
        ScriptRef::NativeScript(ns) => {
            buf.extend(encode_uint(0));
            buf.extend(encode_native_script(ns));
        }
        ScriptRef::PlutusV1(script) => {
            buf.extend(encode_uint(1));
            buf.extend(encode_bytes(script));
        }
        ScriptRef::PlutusV2(script) => {
            buf.extend(encode_uint(2));
            buf.extend(encode_bytes(script));
        }
        ScriptRef::PlutusV3(script) => {
            buf.extend(encode_uint(3));
            buf.extend(encode_bytes(script));
        }
        ScriptRef::PlutusV4(script) => {
            // Dijkstra-only language tag 4 (issue #475 Phase 5).
            buf.extend(encode_uint(4));
            buf.extend(encode_bytes(script));
        }
    }
    buf
}

/// Encode a native script
pub fn encode_native_script(script: &NativeScript) -> Vec<u8> {
    match script {
        NativeScript::ScriptPubkey(hash) => {
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(0));
            // Native script key hashes are 28 bytes (AddrKeyhash) on the wire
            // Our type stores them padded to Hash32, so truncate back to 28
            buf.extend(encode_bytes(&hash.as_ref()[..28]));
            buf
        }
        NativeScript::ScriptAll(scripts) => {
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(1));
            buf.extend(encode_array_header(scripts.len()));
            for s in scripts {
                buf.extend(encode_native_script(s));
            }
            buf
        }
        NativeScript::ScriptAny(scripts) => {
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(2));
            buf.extend(encode_array_header(scripts.len()));
            for s in scripts {
                buf.extend(encode_native_script(s));
            }
            buf
        }
        NativeScript::ScriptNOfK(n, scripts) => {
            let mut buf = encode_array_header(3);
            buf.extend(encode_uint(3));
            buf.extend(encode_uint(*n as u64));
            buf.extend(encode_array_header(scripts.len()));
            for s in scripts {
                buf.extend(encode_native_script(s));
            }
            buf
        }
        NativeScript::InvalidBefore(slot) => {
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(4));
            buf.extend(encode_uint(slot.0));
            buf
        }
        NativeScript::InvalidHereafter(slot) => {
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(5));
            buf.extend(encode_uint(slot.0));
            buf
        }
        NativeScript::RequireGuard(cred) => {
            // Dijkstra (PV12+): tag 6 = DijkstraRequireGuard credential.
            //
            // Wire shape: `array(2) [uint 6, credential]` where
            // `credential = [type, hash28]` (the standard Conway encoding).
            // See `Cardano.Ledger.Dijkstra.Scripts.DijkstraRequireGuard`
            // (`Sum DijkstraRequireGuard 6`). Issue #475 Phase 3.5.
            let mut buf = encode_array_header(2);
            buf.extend(encode_uint(6));
            buf.extend(super::certificate::encode_credential(cred));
            buf
        }
    }
}

/// Encode a redeemer tag
pub(crate) fn encode_redeemer_tag(tag: &RedeemerTag) -> Vec<u8> {
    encode_uint(match tag {
        RedeemerTag::Spend => 0,
        RedeemerTag::Mint => 1,
        RedeemerTag::Cert => 2,
        RedeemerTag::Reward => 3,
        RedeemerTag::Vote => 4,
        RedeemerTag::Propose => 5,
        // Dijkstra (PV12+) only — `DijkstraGuarding`. Issue #475 Phase 3.5.
        RedeemerTag::Guarding => 6,
    })
}

/// Encode a redeemer in Babbage array format: [tag, index, data, ex_units]
///
/// This is the pre-Conway array format. Conway transactions use map format
/// instead (see `encode_witness_set` in transaction.rs). Kept for compatibility
/// with pre-Conway era serialization and as a utility function.
#[allow(dead_code)]
pub(crate) fn encode_redeemer(redeemer: &Redeemer) -> Vec<u8> {
    let mut buf = encode_array_header(4);
    buf.extend(encode_redeemer_tag(&redeemer.tag));
    buf.extend(encode_uint(redeemer.index as u64));
    buf.extend(encode_plutus_data(&redeemer.data));
    buf.extend(encode_array_header(2));
    buf.extend(encode_uint(redeemer.ex_units.mem));
    buf.extend(encode_uint(redeemer.ex_units.steps));
    buf
}

/// Encode a VKey witness [vkey, signature]
pub(crate) fn encode_vkey_witness(w: &VKeyWitness) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    buf.extend(encode_bytes(&w.vkey));
    buf.extend(encode_bytes(&w.signature));
    buf
}

/// Encode a bootstrap witness [vkey, signature, chain_code, attributes]
pub(crate) fn encode_bootstrap_witness(w: &BootstrapWitness) -> Vec<u8> {
    let mut buf = encode_array_header(4);
    buf.extend(encode_bytes(&w.vkey));
    buf.extend(encode_bytes(&w.signature));
    buf.extend(encode_bytes(&w.chain_code));
    buf.extend(encode_bytes(&w.attributes));
    buf
}

/// Encode a metadata map: {label: metadatum}
pub(crate) fn encode_metadata_map(
    metadata: &std::collections::BTreeMap<u64, TransactionMetadatum>,
) -> Vec<u8> {
    let mut buf = encode_map_header(metadata.len());
    for (label, value) in metadata {
        buf.extend(encode_uint(*label));
        buf.extend(encode_metadatum(value));
    }
    buf
}

/// Compute the script data hash for transaction integrity verification.
///
/// Per Cardano ledger spec, this is:
///   blake2b_256(redeemers_cbor || datums_cbor || language_views_cbor)
///
/// When `raw_redeemers_cbor` and `raw_datums_cbor` are provided (captured by
/// the in-house decoder via `KeepRaw::parse_with`), they are used directly
/// instead of re-encoding. This preserves the original encoding format (map
/// vs array for redeemers, definite vs indefinite-length arrays for datums),
/// which is essential for matching the hash computed by the transaction
/// builder.
///
/// Only the language views (cost models) are freshly encoded from protocol
/// parameters, matching what the Haskell cardano-ledger does.
#[allow(clippy::too_many_arguments)]
pub fn compute_script_data_hash(
    redeemers: &[Redeemer],
    plutus_data: &[PlutusData],
    cost_models: &CostModels,
    has_v1: bool,
    has_v2: bool,
    has_v3: bool,
    raw_redeemers_cbor: Option<&[u8]>,
    raw_datums_cbor: Option<&[u8]>,
) -> Hash32 {
    let mut preimage = Vec::new();

    // 1. Redeemers: use raw CBOR when available, otherwise re-encode.
    //
    // Conway uses map format for redeemers in the script data hash preimage:
    //   { [tag, index] => [data, ex_units], ... }
    // Empty redeemers are encoded as 0xa0 (empty map), not 0x80 (empty array),
    // matching Haskell's `hashScriptIntegrity` which uses `encodeRedeemers` always
    // producing a map in the Conway era.
    if let Some(raw) = raw_redeemers_cbor {
        preimage.extend_from_slice(raw);
    } else if redeemers.is_empty() {
        // Empty redeemers: use 0xa0 (empty map) for Conway compatibility.
        preimage.push(0xa0);
    } else {
        // Re-encode as Conway map format: { [tag, index] => [data, ex_units] }
        let mut redeemers_buf = encode_map_header(redeemers.len());
        for r in redeemers {
            redeemers_buf.extend(encode_array_header(2));
            redeemers_buf.extend(encode_redeemer_tag(&r.tag));
            redeemers_buf.extend(encode_uint(r.index as u64));
            redeemers_buf.extend(encode_array_header(2));
            redeemers_buf.extend(encode_plutus_data(&r.data));
            redeemers_buf.extend(encode_array_header(2));
            redeemers_buf.extend(encode_uint(r.ex_units.mem));
            redeemers_buf.extend(encode_uint(r.ex_units.steps));
        }
        preimage.extend(&redeemers_buf);
    }

    // 2. Datums: per Haskell `SafeToHash (ScriptIntegrity era)` the datums term
    //    is OMITTED when the TxDats set is empty
    //    (`dBytes = if null (d ^. unTxDatsL) then mempty else originalBytes d`),
    //    even if the wire carried an empty `plutus_data` array (`0x80`). Gate the
    //    whole datums term on a non-empty decoded set; an empty set contributes
    //    nothing to the preimage.
    if !plutus_data.is_empty() {
        if let Some(raw) = raw_datums_cbor {
            preimage.extend_from_slice(raw);
        } else {
            let mut datums_buf = encode_tag(258);
            datums_buf.extend(encode_array_header(plutus_data.len()));
            for d in plutus_data {
                datums_buf.extend(encode_plutus_data(d));
            }
            preimage.extend(&datums_buf);
        }
    }

    // 3. Encode language views (cost models for languages used in the transaction)
    preimage.extend(encode_language_views(cost_models, has_v1, has_v2, has_v3));

    dugite_primitives::hash::blake2b_256(&preimage)
}

/// Compute `script_data_hash` from raw transaction CBOR.
///
/// Per the Cardano ledger spec the preimage is
/// `blake2b_256(redeemers_cbor || datums_cbor || language_views_cbor)`,
/// using the **original** wire-CBOR for `redeemers` and `datums` (definite vs
/// indefinite arrays, map vs array redeemer form, etc.) — small encoding
/// differences change the hash even when the structural values are identical.
///
/// Routes the tx through the in-house Conway decoder, which captures the
/// redeemers + plutus-data raw bytes via `KeepRaw::parse_with` into
/// `TransactionWitnessSet::raw_redeemers_cbor` / `raw_plutus_data_cbor`.
/// Returns `None` if the tx cannot be decoded or if neither redeemers nor
/// plutus data are present (in which case the ledger does not compute a
/// script_data_hash at all).
pub fn compute_script_data_hash_from_cbor(
    tx_cbor: &[u8],
    cost_models: &CostModels,
    has_v1: bool,
    has_v2: bool,
    has_v3: bool,
) -> Option<Hash32> {
    // Conway era_id = 6 in the HFC convention used by `decode_transaction`.
    let tx = crate::decode::decode_transaction(6, tx_cbor).ok()?;
    let ws = &tx.witness_set;

    if ws.raw_redeemers_cbor.is_none() && ws.raw_plutus_data_cbor.is_none() {
        return None;
    }

    let mut preimage = Vec::new();

    // 1. Redeemers: prefer raw wire-CBOR; fall back to empty-map sentinel.
    if let Some(raw) = ws.raw_redeemers_cbor.as_deref() {
        preimage.extend_from_slice(raw);
    } else {
        preimage.push(0xa0); // empty CBOR map
    }

    // 2. Datums: per Haskell `SafeToHash (ScriptIntegrity era)`,
    //    `dBytes = if null (d ^. unTxDatsL) then mempty else originalBytes d`.
    //    The datums term is OMITTED whenever the decoded TxDats set is empty,
    //    even if the witness set serialized an empty `plutus_data` array (`0x80`)
    //    on the wire. Gating on raw-bytes presence wrongly included that `0x80`
    //    and broke the hash for Alonzo mint/spend txs whose scripts take no
    //    datum (mainnet script_data_hash mismatch class).
    if !ws.plutus_data.is_empty() {
        if let Some(raw) = ws.raw_plutus_data_cbor.as_deref() {
            preimage.extend_from_slice(raw);
        }
    }

    // 3. Language views derived from the supplied cost models.
    preimage.extend(encode_language_views(cost_models, has_v1, has_v2, has_v3));

    Some(dugite_primitives::hash::blake2b_256(&preimage))
}

/// Encode cost models as "language views" for script data hash computation.
///
/// Per the Haskell cardano-ledger implementation:
/// - PlutusV1: key = bstr(0x00) "double-bagged", value = bstr(indef_array(...))
/// - PlutusV2: key = uint(1), value = array(...)
/// - PlutusV3: key = uint(2), value = array(...)
///
/// Entries are sorted by short-lex order on key bytes:
/// V2 (0x01, 1 byte) < V3 (0x02, 1 byte) < V1 (0x41 0x00, 2 bytes)
///
/// Only includes cost models for languages actually used in the transaction.
pub(crate) fn encode_language_views(
    cost_models: &CostModels,
    has_v1: bool,
    has_v2: bool,
    has_v3: bool,
) -> Vec<u8> {
    // Collect (key_bytes, value_bytes) pairs
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    if has_v1 {
        if let Some(v1) = &cost_models.plutus_v1 {
            // PlutusV1 key: "double-bagged" — serialize(serialize(0)) = bstr(0x00) = [0x41, 0x00]
            let key = encode_bytes(&encode_uint(0));
            // PlutusV1 value: bstr wrapping indefinite-length CBOR array
            let mut indef_arr = vec![0x9Fu8]; // indefinite-length array start
            for cost in v1 {
                indef_arr.extend(encode_int(*cost as i128));
            }
            indef_arr.push(0xFF); // break
            let value = encode_bytes(&indef_arr);
            entries.push((key, value));
        }
    }
    if has_v2 {
        if let Some(v2) = &cost_models.plutus_v2 {
            // PlutusV2 key: raw CBOR uint 1
            let key = encode_uint(1);
            // PlutusV2 value: definite-length CBOR array (raw, not byte-wrapped)
            let mut value = encode_array_header(v2.len());
            for cost in v2 {
                value.extend(encode_int(*cost as i128));
            }
            entries.push((key, value));
        }
    }
    if has_v3 {
        if let Some(v3) = &cost_models.plutus_v3 {
            // PlutusV3 key: raw CBOR uint 2
            let key = encode_uint(2);
            // PlutusV3 value: definite-length CBOR array (raw, not byte-wrapped)
            let mut value = encode_array_header(v3.len());
            for cost in v3 {
                value.extend(encode_int(*cost as i128));
            }
            entries.push((key, value));
        }
    }

    if entries.is_empty() {
        return encode_map_header(0);
    }

    // Sort by short-lex order on key bytes (shorter keys first, ties broken lexicographically)
    entries.sort_by(|(a, _), (b, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));

    let mut buf = encode_map_header(entries.len());
    for (key, value) in entries {
        buf.extend(key);
        buf.extend(value);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_script_data_hash_from_real_tx_cbor() {
        // Real failing tx from preview testnet (Alonzo-style array redeemers in Conway era)
        // Expected script_data_hash from tx body field 0b:
        let expected_hex = "7482063745239a453494a4700d4e9e481c745603355fa31fdc5cee2ca0c20d3d";
        let expected = Hash32::from_hex(expected_hex).unwrap();

        // The full tx CBOR (from Koios)
        let tx_cbor_hex = include_str!("../../test_data/script_data_hash_test_tx.hex");
        let tx_cbor = hex::decode(tx_cbor_hex.trim()).unwrap();

        // Use preview cost models (V2 only - this tx uses V2 scripts)
        let cost_models = CostModels {
            plutus_v1: None,
            plutus_v2: Some(vec![
                100788, 420, 1, 1, 1000, 173, 0, 1, 1000, 59957, 4, 1, 11183, 32, 201305, 8356, 4,
                16000, 100, 16000, 100, 16000, 100, 16000, 100, 16000, 100, 16000, 100, 100, 100,
                16000, 100, 94375, 32, 132994, 32, 61462, 4, 72010, 178, 0, 1, 22151, 32, 91189,
                769, 4, 2, 85848, 228465, 122, 0, 1, 1, 1000, 42921, 4, 2, 24548, 29498, 38, 1,
                898148, 27279, 1, 51775, 558, 1, 39184, 1000, 60594, 1, 141895, 32, 83150, 32,
                15299, 32, 76049, 1, 13169, 4, 22100, 10, 28999, 74, 1, 28999, 74, 1, 43285, 552,
                1, 44749, 541, 1, 33852, 32, 68246, 32, 72362, 32, 7243, 32, 7391, 32, 11546, 32,
                85848, 228465, 122, 0, 1, 1, 90434, 519, 0, 1, 74433, 32, 85848, 228465, 122, 0, 1,
                1, 85848, 228465, 122, 0, 1, 1, 955506, 213312, 0, 2, 270652, 22588, 4, 1457325,
                64566, 4, 20467, 1, 4, 0, 141992, 32, 100788, 420, 1, 1, 81663, 32, 59498, 32,
                20142, 32, 24588, 32, 20744, 32, 25933, 32, 24623, 32, 43053543, 10, 53384111,
                14333, 10, 43574283, 26308, 10,
            ]),
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };

        let result = compute_script_data_hash_from_cbor(&tx_cbor, &cost_models, false, true, false);

        assert_eq!(
            result,
            Some(expected),
            "Script data hash from real tx CBOR should match declared hash"
        );
    }

    /// Confirms the V3 `script_data_hash` divergence observed live at preprod
    /// ep292 (tx `bc000168…`, slot 124831208) is purely a cost-model COUNT
    /// issue, not an encoding bug. dugite's `encode_language_views` is correct
    /// for V3 — feeding the REAL ep292 V3 cost model (350 entries; the PV10
    /// Plomin hard fork expanded it from the 251-entry Conway genesis model)
    /// reproduces the on-chain hash exactly. The live divergence
    /// (dugite computed a85cfe40…) was caused by dugite still using its 251-entry
    /// genesis V3 cost model at ep292 — i.e. the V3 cost-model expansion was not
    /// applied. Fixing that (the protocol/cost-model update path) makes the live
    /// path produce this same hash.
    #[test]
    fn test_v3_script_data_hash_ep292_with_real_costmodel() {
        let expected =
            Hash32::from_hex("2852bd63c702b3e99d3ef9d5e9e8b1802fc12ccbfccb82c7ce7d1f77bbf7fd7e")
                .unwrap();
        let tx_cbor =
            hex::decode(include_str!("../../test_data/sdh_divergence_ep292_tx.hex").trim())
                .unwrap();
        let v3: Vec<i64> = include_str!("../../test_data/sdh_v3_costmodel_ep292.txt")
            .trim()
            .split(',')
            .map(|s| s.trim().parse::<i64>().unwrap())
            .collect();
        assert_eq!(v3.len(), 350, "real ep292 V3 cost model has 350 entries");
        let cost_models = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: Some(v3),
            plutus_v4: None,
            ..Default::default()
        };
        // V3-only tx (witness keys {0 vkey, 5 redeemers, 7 plutusV3}, no datums).
        let result = compute_script_data_hash_from_cbor(&tx_cbor, &cost_models, false, false, true);
        assert_eq!(
            result,
            Some(expected),
            "V3 script_data_hash must match on-chain when the correct 350-entry \
             cost model is used — proves the encoding is correct and the live \
             divergence is dugite using the stale 251-entry genesis V3 cost model"
        );
    }

    // The earlier `test_script_data_hash_survives_reencode` test was removed
    // because it relied on a byte-exact decode-then-re-encode round-trip
    // that the dugite encoder does not guarantee (it intentionally
    // canonicalises some CBOR shapes). The remaining
    // `test_script_data_hash_from_real_tx` already covers the only
    // invariant that matters: the hash matches the declared
    // `script_data_hash` field of the witness set on a real tx.

    /// Per Haskell `SafeToHash (ScriptIntegrity era)`
    /// (`dBytes = if null (d ^. unTxDatsL) then mempty else originalBytes d`),
    /// an empty witness datums set contributes NOTHING to the script_data_hash
    /// preimage — even when the wire carried an empty `plutus_data` array
    /// (`0x80`). So the hash computed with an empty `0x80` raw-datums blob must
    /// equal the hash computed with no datums at all (mainnet Alonzo tx
    /// c1dd5612… script_data_hash mismatch class).
    #[test]
    fn script_data_hash_omits_empty_datums() {
        use dugite_primitives::transaction::{ExUnits, PlutusData, Redeemer, RedeemerTag};
        let redeemers = vec![Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(num_bigint::BigInt::from(0)),
            ex_units: ExUnits {
                mem: 1000,
                steps: 2000,
            },
        }];
        let cm = CostModels {
            plutus_v1: Some(vec![1i64; 166]),
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let empty: Vec<PlutusData> = vec![];

        // raw datums = empty array 0x80 vs absent: must hash identically.
        let with_0x80 = compute_script_data_hash(
            &redeemers,
            &empty,
            &cm,
            true,
            false,
            false,
            None,
            Some(&[0x80]),
        );
        let with_none =
            compute_script_data_hash(&redeemers, &empty, &cm, true, false, false, None, None);
        assert_eq!(
            with_0x80, with_none,
            "empty plutus_data (0x80) must be omitted from the script_data_hash preimage"
        );

        // Sanity: a non-empty datum DOES change the hash.
        let one = vec![PlutusData::Integer(num_bigint::BigInt::from(7))];
        let with_datum =
            compute_script_data_hash(&redeemers, &one, &cm, true, false, false, None, None);
        assert_ne!(
            with_datum, with_none,
            "a non-empty datum must contribute to the script_data_hash"
        );
    }

    // ── ScriptRef variant tags ───────────────────────────────────────────────

    #[test]
    fn script_ref_native_tag_is_0() {
        let sr = ScriptRef::NativeScript(NativeScript::InvalidBefore(
            dugite_primitives::time::SlotNo(1),
        ));
        let cbor = encode_script_ref(&sr);
        // Outer array(2), then uint(0), then encoded native script.
        assert_eq!(cbor[0], 0x82);
        assert_eq!(cbor[1], 0x00);
    }

    #[test]
    fn script_ref_plutus_v1_tag_is_1() {
        let sr = ScriptRef::PlutusV1(vec![0xde, 0xad]);
        let cbor = encode_script_ref(&sr);
        assert_eq!(cbor[0], 0x82);
        assert_eq!(cbor[1], 0x01);
        // After variant tag: bstr length(2) + bytes.
        assert_eq!(cbor[2], 0x42);
        assert_eq!(&cbor[3..5], &[0xde, 0xad]);
    }

    #[test]
    fn script_ref_plutus_v2_tag_is_2() {
        let sr = ScriptRef::PlutusV2(vec![0x01]);
        let cbor = encode_script_ref(&sr);
        assert_eq!(cbor[1], 0x02);
    }

    #[test]
    fn script_ref_plutus_v3_tag_is_3() {
        let sr = ScriptRef::PlutusV3(vec![]);
        let cbor = encode_script_ref(&sr);
        assert_eq!(cbor[1], 0x03);
        // Empty plutus bytes → bstr(0) = 0x40.
        assert_eq!(cbor[2], 0x40);
    }

    // ── NativeScript variants ────────────────────────────────────────────────

    #[test]
    fn native_script_pubkey_truncates_to_28_bytes() {
        use dugite_primitives::hash::Hash32;
        // Build a Hash32 whose first 28 bytes are 0xAA and last 4 are 0xBB.
        let mut raw = [0xAAu8; 32];
        raw[28..].copy_from_slice(&[0xBB; 4]);
        let hash = Hash32::from_bytes(raw);
        let cbor = encode_native_script(&NativeScript::ScriptPubkey(hash));
        // array(2) + uint(0) + bstr(28) + 28 bytes
        assert_eq!(cbor[0], 0x82);
        assert_eq!(cbor[1], 0x00);
        // 0x58 0x1c = bstr(28).
        assert_eq!(cbor[2], 0x58);
        assert_eq!(cbor[3], 0x1c);
        // Verify the padding bytes were dropped (only 28 bytes follow).
        assert_eq!(&cbor[4..32], &[0xAA; 28]);
        assert_eq!(cbor.len(), 32);
    }

    #[test]
    fn native_script_all_encodes_inner_scripts() {
        use dugite_primitives::time::SlotNo;
        let inner = vec![
            NativeScript::InvalidBefore(SlotNo(7)),
            NativeScript::InvalidHereafter(SlotNo(11)),
        ];
        let cbor = encode_native_script(&NativeScript::ScriptAll(inner));
        // array(2) + uint(1) + array(2) + 2 inner native scripts (each 3 bytes)
        assert_eq!(cbor[0], 0x82);
        assert_eq!(cbor[1], 0x01);
        assert_eq!(cbor[2], 0x82); // array(2) of inner scripts
    }

    #[test]
    fn native_script_any_tag_is_2() {
        let cbor = encode_native_script(&NativeScript::ScriptAny(vec![]));
        assert_eq!(cbor[1], 0x02);
        assert_eq!(cbor[2], 0x80); // empty inner array
    }

    #[test]
    fn native_script_n_of_k_encodes_n_first() {
        let cbor = encode_native_script(&NativeScript::ScriptNOfK(3, vec![]));
        // array(3) + uint(3 = NOfK tag) + uint(3 = n) + array(0)
        assert_eq!(cbor[0], 0x83);
        assert_eq!(cbor[1], 0x03);
        assert_eq!(cbor[2], 0x03);
        assert_eq!(cbor[3], 0x80);
    }

    #[test]
    fn native_script_invalid_before_tag_is_4() {
        use dugite_primitives::time::SlotNo;
        let cbor = encode_native_script(&NativeScript::InvalidBefore(SlotNo(100)));
        assert_eq!(cbor[0], 0x82);
        assert_eq!(cbor[1], 0x04);
        assert_eq!(cbor[2], 0x18); // uint(info=24)
        assert_eq!(cbor[3], 100);
    }

    #[test]
    fn native_script_invalid_hereafter_tag_is_5() {
        use dugite_primitives::time::SlotNo;
        let cbor = encode_native_script(&NativeScript::InvalidHereafter(SlotNo(0)));
        assert_eq!(cbor[1], 0x05);
        assert_eq!(cbor[2], 0x00);
    }

    // ── encode_redeemer_tag for each variant ────────────────────────────────

    #[test]
    fn redeemer_tag_encodings_match_wire_values() {
        // The wire-format uints for each tag (per Conway spec).
        let cases = [
            (RedeemerTag::Spend, 0u64),
            (RedeemerTag::Mint, 1),
            (RedeemerTag::Cert, 2),
            (RedeemerTag::Reward, 3),
            (RedeemerTag::Vote, 4),
            (RedeemerTag::Propose, 5),
        ];
        for (tag, expected) in cases {
            let cbor = encode_redeemer_tag(&tag);
            // Small uints encode as a single byte (info=value when value < 24).
            assert_eq!(cbor, vec![expected as u8], "tag mismatch for {tag:?}");
        }
    }

    // ── encode_redeemer round-trips a structurally tagged blob ──────────────

    #[test]
    fn encode_redeemer_layout_is_array_of_four() {
        let r = Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(0u32.into()),
            ex_units: ExUnits { mem: 7, steps: 11 },
        };
        let cbor = encode_redeemer(&r);
        // Outer array(4)
        assert_eq!(cbor[0], 0x84);
        assert_eq!(cbor[1], 0x00); // tag = Spend → 0
        assert_eq!(cbor[2], 0x00); // index = 0
    }

    // ── encode_vkey_witness / encode_bootstrap_witness ──────────────────────

    #[test]
    fn vkey_witness_is_two_bytestrings() {
        let w = VKeyWitness {
            vkey: vec![0x01; 32],
            signature: vec![0x02; 64],
        };
        let cbor = encode_vkey_witness(&w);
        assert_eq!(cbor[0], 0x82); // array(2)
        assert_eq!(cbor[1], 0x58); // bstr(info=24)
        assert_eq!(cbor[2], 0x20); // length 32
    }

    #[test]
    fn bootstrap_witness_is_four_bytestrings() {
        let w = BootstrapWitness {
            vkey: vec![0x01; 32],
            signature: vec![0x02; 64],
            chain_code: vec![0x03; 32],
            attributes: vec![],
        };
        let cbor = encode_bootstrap_witness(&w);
        assert_eq!(cbor[0], 0x84); // array(4)
    }

    // ── encode_metadata_map ─────────────────────────────────────────────────

    #[test]
    fn metadata_map_orders_by_label() {
        let mut md = std::collections::BTreeMap::new();
        md.insert(674u64, TransactionMetadatum::Text("hello".to_string()));
        md.insert(0u64, TransactionMetadatum::Int(42));
        let cbor = encode_metadata_map(&md);
        // 0xa2 = map(2). Then keys in BTreeMap order: 0 first.
        assert_eq!(cbor[0], 0xa2);
        assert_eq!(cbor[1], 0x00); // label 0
                                   // After integer metadatum (1-2 bytes), next label is 674 = 0x19 0x02 0xa2.
                                   // We just assert the first key is the smaller label.
    }

    // ── encode_language_views: each language path ───────────────────────────

    #[test]
    fn language_views_empty_is_empty_map() {
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        // No `has_*` flags set → empty map.
        let cbor = encode_language_views(&cm, false, false, false);
        assert_eq!(cbor, vec![0xa0]);
    }

    #[test]
    fn language_views_v1_only_double_bagged_key() {
        let cm = CostModels {
            plutus_v1: Some(vec![1, 2, 3]),
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let cbor = encode_language_views(&cm, true, false, false);
        // Map(1), then key = bstr(0x00) = [0x41, 0x00] ("double-bagged"),
        // then value = bstr-wrapped indef-array.
        assert_eq!(cbor[0], 0xa1);
        assert_eq!(&cbor[1..3], &[0x41, 0x00]);
    }

    #[test]
    fn language_views_v2_only_array_form() {
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: Some(vec![10, 20]),
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let cbor = encode_language_views(&cm, false, true, false);
        // Map(1), key = uint(1), value = array(2)[uint(10), uint(20)].
        assert_eq!(cbor[0], 0xa1);
        assert_eq!(cbor[1], 0x01); // key = uint(1)
        assert_eq!(cbor[2], 0x82); // array(2)
    }

    #[test]
    fn language_views_v3_only_array_form() {
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: Some(vec![1]),
            plutus_v4: None,
            ..Default::default()
        };
        let cbor = encode_language_views(&cm, false, false, true);
        assert_eq!(cbor[1], 0x02); // key = uint(2)
    }

    #[test]
    fn language_views_short_lex_order_v2_v3_v1() {
        let cm = CostModels {
            plutus_v1: Some(vec![1]),
            plutus_v2: Some(vec![2]),
            plutus_v3: Some(vec![3]),
            plutus_v4: None,
            ..Default::default()
        };
        let cbor = encode_language_views(&cm, true, true, true);
        // map(3); first key uint(1)=V2 (1 byte), second uint(2)=V3 (1 byte),
        // third bstr(0x00)=V1 (2 bytes). Verify the key order on the wire.
        assert_eq!(cbor[0], 0xa3);
        assert_eq!(cbor[1], 0x01); // V2 first
    }

    #[test]
    fn language_views_has_flag_without_cost_model_is_noop() {
        // `has_v1=true` but no cost model → entry is skipped.
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let cbor = encode_language_views(&cm, true, true, true);
        assert_eq!(cbor, vec![0xa0]);
    }

    // ── compute_script_data_hash (in-memory inputs) ─────────────────────────

    #[test]
    fn compute_script_data_hash_empty_redeemers_uses_a0_sentinel() {
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        // Empty redeemers, no datums, no languages → preimage = [0xa0, 0xa0]
        // (empty-map sentinel + empty language-views map).
        let h = compute_script_data_hash(&[], &[], &cm, false, false, false, None, None);
        let expected = dugite_primitives::hash::blake2b_256(&[0xa0, 0xa0]);
        assert_eq!(h, expected);
    }

    #[test]
    fn compute_script_data_hash_raw_inputs_passthrough() {
        use dugite_primitives::transaction::PlutusData;
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let raw_red = [0xa1, 0x82, 0x00, 0x00, 0x82, 0x80, 0x82, 0x07, 0x0b];

        // Non-empty datums set: the raw datums wire bytes pass through verbatim.
        let raw_dat = [0x81, 0x00]; // array(1)[ 0 ]
        let datums = vec![PlutusData::Integer(num_bigint::BigInt::from(0))];
        let h1 = compute_script_data_hash(
            &[],
            &datums,
            &cm,
            false,
            false,
            false,
            Some(&raw_red),
            Some(&raw_dat),
        );
        let mut preimage = raw_red.to_vec();
        preimage.extend_from_slice(&raw_dat);
        preimage.extend(encode_language_views(&cm, false, false, false));
        assert_eq!(h1, dugite_primitives::hash::blake2b_256(&preimage));

        // Empty datums set: the raw `0x80` empty-array blob is OMITTED, even
        // though it is present on the wire (Haskell `SafeToHash (ScriptIntegrity
        // era)`: `if null (d ^. unTxDatsL) then mempty`).
        let h2 = compute_script_data_hash(
            &[],
            &[],
            &cm,
            false,
            false,
            false,
            Some(&raw_red),
            Some(&[0x80]),
        );
        let mut preimage2 = raw_red.to_vec();
        preimage2.extend(encode_language_views(&cm, false, false, false));
        assert_eq!(h2, dugite_primitives::hash::blake2b_256(&preimage2));
    }

    #[test]
    fn compute_script_data_hash_reencodes_redeemers_when_no_raw() {
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let r = Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(0u32.into()),
            ex_units: ExUnits { mem: 1, steps: 1 },
        };
        // Just exercise the re-encode branch — confirm we get *some* hash and
        // that it differs from the empty-redeemers hash.
        let h_with = compute_script_data_hash(
            std::slice::from_ref(&r),
            &[],
            &cm,
            false,
            false,
            false,
            None,
            None,
        );
        let h_empty = compute_script_data_hash(&[], &[], &cm, false, false, false, None, None);
        assert_ne!(h_with, h_empty);
    }

    #[test]
    fn compute_script_data_hash_reencodes_datums_when_no_raw() {
        let cm = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            plutus_v4: None,
            ..Default::default()
        };
        let d = PlutusData::Bytes(vec![0xab, 0xcd]);
        let h = compute_script_data_hash(
            &[],
            std::slice::from_ref(&d),
            &cm,
            false,
            false,
            false,
            None,
            None,
        );
        let h_empty = compute_script_data_hash(&[], &[], &cm, false, false, false, None, None);
        assert_ne!(h, h_empty);
    }
}
