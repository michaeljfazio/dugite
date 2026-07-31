use crate::cbor::*;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{blake2b_256, Hash32};
use dugite_primitives::transaction::*;

use super::certificate::{encode_certificate, encode_credential};
use super::governance::{encode_proposal_procedure, encode_voting_procedures};
use super::script::{
    encode_bootstrap_witness, encode_metadata_map, encode_native_script, encode_redeemer_tag,
    encode_script_ref, encode_vkey_witness,
};
use super::value::{encode_mint, encode_value};

/// Encode a set-typed field as CBOR tag 258 with a sorted definite-length array.
///
/// Per Conway CDDL: `set<a> = #6.258([* a])`. The canonical encoding wraps the
/// items in tag 258. Items are sorted lexicographically by their CBOR encoding
/// to produce canonical ordering.
fn encode_tagged_set<T, F>(items: &[T], encode_item: F) -> Vec<u8>
where
    F: Fn(&T) -> Vec<u8>,
{
    // Encode each item individually so we can sort them.
    let mut encoded_items: Vec<Vec<u8>> = items.iter().map(encode_item).collect();
    // Canonical CBOR set ordering: lexicographic on the CBOR encoding bytes.
    encoded_items.sort();

    // tag(258) followed by a variable-length array. Haskell `encodeSet` emits
    // `encodeTag setTag <> variableListLenEncoding (Set.size f) ...` from PV9
    // on — the tag wraps the array, so the definite-<=23 / indefinite->23
    // threshold still applies to the array itself (#938).
    let len = encoded_items.len();
    let mut buf = encode_tag(258);
    buf.extend(encode_array_open(len));
    for encoded in encoded_items {
        buf.extend(encoded);
    }
    encode_array_close(&mut buf, len);
    buf
}

/// Encode an `OSet`-typed field: CBOR tag 258 followed by a variable-length
/// array, **preserving insertion order** (#940).
///
/// `OSet` is an *ordered* set and is NOT encoded like `Set`. Its `EncCBOR`
/// instance (`libs/cardano-data/src/Data/OSet/Strict.hs`) is:
///
/// ```haskell
/// instance EncCBOR a => EncCBOR (OSet a) where
///   encCBOR (OSet seq _set) = encodeTag setTag <> encodeStrictSeq encCBOR seq
/// ```
///
/// Two differences from [`encode_tagged_set`] that both matter:
///
/// 1. **No sorting.** `encodeStrictSeq` runs over the insertion-ordered
///    sequence, so wire order is the transaction's own order. Sorting
///    certificates would reorder them, and certificate order is semantically
///    load-bearing (a registration must precede the delegation using it).
/// 2. **The tag is unconditional.** There is no `ifEncodingVersionAtLeast`
///    guard on `OSet`'s `setTag`, unlike `Set.Set`'s PV>=9-gated tag.
///
/// Used for Conway/Dijkstra `certificates` (key 4) and `proposal_procedures`
/// (key 20). Pre-Conway certificates are `StrictSeq`, not `OSet` — those take
/// [`encode_plain_array`].
fn encode_ordered_set<T, F>(items: &[T], encode_item: F) -> Vec<u8>
where
    F: Fn(&T) -> Vec<u8>,
{
    let len = items.len();
    let mut buf = encode_tag(258);
    buf.extend(encode_array_open(len));
    for item in items {
        buf.extend(encode_item(item));
    }
    encode_array_close(&mut buf, len);
    buf
}

/// Encode a sequence as a plain (untagged) array (pre-Conway eras).
///
/// Pre-Conway CDDL uses `[* item]` for inputs, certificates, collateral, and
/// reference inputs — NOT `set<item> = #6.258([* item])`. Items are encoded in
/// their original order without sorting, preserving the original transaction body.
///
/// Still variable-length framed: Haskell `encodeSet` below PV9 drops the 258
/// tag but keeps `variableListLenEncoding` (#938).
fn encode_plain_array<T, F>(items: &[T], encode_item: F) -> Vec<u8>
where
    F: Fn(&T) -> Vec<u8>,
{
    let mut buf = encode_array_open(items.len());
    for item in items {
        buf.extend(encode_item(item));
    }
    encode_array_close(&mut buf, items.len());
    buf
}

/// Encode a set-typed body field using the correct format for the given era.
///
/// - Conway and Dijkstra (and any later era): CBOR tag 258 with lexicographically
///   sorted items (`set<a> = #6.258([* a])` per Conway CDDL — Dijkstra inherits
///   Conway CDDL verbatim)
/// - Pre-Conway: plain definite-length array (`[* a]`)
///
/// # D5 / audit #544 — Dijkstra era missing from tag-258 path
///
/// The original code checked `era == Era::Conway` only. Dijkstra-era transactions
/// re-encoded with the `else` branch emitted unsorted plain arrays, producing a
/// different body hash than the canonical from the original wire bytes — a
/// chain-split for any relayed or re-encoded Dijkstra block.
fn encode_set_for_era<T, F>(era: Era, items: &[T], encode_item: F) -> Vec<u8>
where
    F: Fn(&T) -> Vec<u8>,
{
    // Conway, Dijkstra, and any future era that inherits Conway CDDL uses tag 258.
    if matches!(era, Era::Conway | Era::Dijkstra) {
        encode_tagged_set(items, encode_item)
    } else {
        encode_plain_array(items, encode_item)
    }
}

/// Encode a transaction output.
///
/// Two wire-format variants exist:
///
/// **Legacy (Shelley/Allegra/Mary/Alonzo era) — `output.is_legacy = true`**
/// Encoded as a CBOR array: `[address, value]` or `[address, value, datum_hash]`.
/// Conway-era transactions may embed legacy-format outputs for simple change
/// outputs to preserve encoding compatibility with existing tooling.
///
/// **Post-Alonzo (Babbage/Conway era) — `output.is_legacy = false`**
/// Encoded as a CBOR map with optional keys: `{0: address, 1: value, ?2: datum_option, ?3: script_ref}`.
///
/// The `is_legacy` flag is stored in bincode so it survives LSM round-trips.
pub fn encode_transaction_output(output: &TransactionOutput) -> Vec<u8> {
    if output.is_legacy {
        return encode_legacy_transaction_output(output);
    }
    encode_post_alonzo_transaction_output(output)
}

/// Encode a legacy (Shelley-era array format) transaction output.
///
/// Wire format: `[address_bytes, value]` or `[address_bytes, value, datum_hash]`
fn encode_legacy_transaction_output(output: &TransactionOutput) -> Vec<u8> {
    let has_datum_hash = matches!(&output.datum, OutputDatum::DatumHash(_));
    let len = if has_datum_hash { 3 } else { 2 };

    let mut buf = encode_array_header(len);
    buf.extend(encode_bytes(&output.address.to_bytes()));
    buf.extend(encode_value(&output.value));
    if let OutputDatum::DatumHash(h) = &output.datum {
        buf.extend(encode_hash32(h));
    }
    buf
}

/// Encode a post-Alonzo (Babbage/Conway map format) transaction output.
///
/// Map with keys: 0=address, 1=value, 2=datum_option, 3=script_ref
fn encode_post_alonzo_transaction_output(output: &TransactionOutput) -> Vec<u8> {
    let mut count = 2; // address + value are always present
    if output.datum != OutputDatum::None {
        count += 1;
    }
    if output.script_ref.is_some() {
        count += 1;
    }

    let mut buf = encode_map_header(count);

    // 0: address
    buf.extend(encode_uint(0));
    buf.extend(encode_bytes(&output.address.to_bytes()));

    // 1: value
    buf.extend(encode_uint(1));
    buf.extend(encode_value(&output.value));

    // 2: datum_option
    match &output.datum {
        OutputDatum::None => {}
        OutputDatum::DatumHash(h) => {
            buf.extend(encode_uint(2));
            // [0, datum_hash]
            buf.extend(encode_array_header(2));
            buf.extend(encode_uint(0));
            buf.extend(encode_hash32(h));
        }
        OutputDatum::InlineDatum { data, raw_cbor } => {
            buf.extend(encode_uint(2));
            // [1, #6.24(cbor_encoded_data)]
            buf.extend(encode_array_header(2));
            buf.extend(encode_uint(1));
            // Tag 24 (CBOR-encoded data item). Use the preserved raw bytes when
            // available so that encoding details (indefinite-length arrays inside
            // Constr/List, etc.) are reproduced exactly. Falling back to a fresh
            // encode_plutus_data() call would produce definite-length arrays which
            // differ from what many Plutus script builders emit, causing datum hash
            // mismatches in script context construction.
            buf.extend(encode_tag(24));
            let encoded_data = raw_cbor
                .as_deref()
                .map(|r| r.to_vec())
                .unwrap_or_else(|| encode_plutus_data(data));
            buf.extend(encode_bytes(&encoded_data));
        }
    }

    // 3: script_ref
    if let Some(script_ref) = &output.script_ref {
        buf.extend(encode_uint(3));
        // Tag 24 (CBOR-encoded data item)
        buf.extend(encode_tag(24));
        let script_cbor = encode_script_ref(script_ref);
        buf.extend(encode_bytes(&script_cbor));
    }

    buf
}

/// Encode a transaction witness set as CBOR map.
///
/// Map keys: 0=vkeywitnesses, 1=native_scripts, 2=bootstrap_witnesses,
///           3=plutus_v1, 4=plutus_data, 5=redeemers, 6=plutus_v2, 7=plutus_v3
/// Encode a transaction witness set (Conway format by default).
///
/// This is a compatibility wrapper that always uses Conway-era encoding
/// (map format for redeemers). For era-specific encoding, use
/// `encode_witness_set_for_era`.
pub fn encode_witness_set(ws: &TransactionWitnessSet) -> Vec<u8> {
    encode_witness_set_for_era(ws, Era::Conway)
}

/// Encode the `redeemers` witness term (witness-set key 5) as CBOR.
///
/// The wire form is era-dependent and `map_form` selects it:
/// - Conway/Dijkstra (`map_form = true`): MAP `{ [tag, index] => [data, ex_units], … }`
///   (Conway CDDL `nonempty_map<redeemer_key, redeemer_value>`).
/// - Alonzo/Babbage (`map_form = false`): LIST `[* [tag, index, data, ex_units]]`.
///
/// An empty slice therefore encodes as the era's empty container — `0xa0`
/// (empty map) in Conway/Dijkstra, `0x80` (empty list) pre-Conway — which is
/// exactly the empty-redeemers sentinel used by the script-integrity preimage
/// (see [`crate::compute_script_data_hash`]). This is the single canonical
/// redeemers encoder, shared by the witness-set builder, the script-data-hash
/// preimage, and `dugite-cli`'s offline tx builder so the three never diverge.
///
/// The redeemers are emitted in canonical order — ascending `(tag, index)` —
/// matching cardano-ledger, whose `Redeemers` is a `Map (PlutusPurpose AsIx)
/// (Data, ExUnits)` serialized via `Map.toAscList`. The `Ord` on the key is
/// `(purpose-constructor, index)`, and the purpose constructors are numbered
/// exactly as the redeemer tags (Spend=0, Mint=1, Cert=2, Reward=3, Vote=4,
/// Propose=5, Guarding=6), so `(tag, index)` reproduces it (#887).
pub fn encode_redeemers(redeemers: &[Redeemer], map_form: bool) -> Vec<u8> {
    // Canonical ascending `(tag, index)` order via an index permutation, so we
    // never clone the (potentially large) redeemer `data`.
    let mut order: Vec<usize> = (0..redeemers.len()).collect();
    order.sort_by_key(|&i| (redeemer_tag_ord(&redeemers[i].tag), redeemers[i].index));

    let mut buf = Vec::new();
    if map_form {
        // Conway/Dijkstra map: { [tag, index] => [data, ex_units], … }
        //
        // Haskell `Redeemers` at PV>=9 is a bare `encCBOR` on the underlying
        // `Map` — the generic instance — so `encodeMap` semantics apply
        // (#932): definite header <= 23 entries, indefinite above. An EMPTY
        // map (0 <= 23) stays the definite `0xa0` — the era-gated
        // script-integrity sentinel is untouched.
        buf.extend(encode_map_open(redeemers.len()));
        for &i in &order {
            let r = &redeemers[i];
            // Key: [tag, index]
            buf.extend(encode_array_header(2));
            buf.extend(encode_redeemer_tag(&r.tag));
            buf.extend(encode_uint(r.index as u64));
            // Value: [data, ex_units]
            buf.extend(encode_array_header(2));
            buf.extend(encode_plutus_data(&r.data));
            buf.extend(encode_array_header(2));
            buf.extend(encode_uint(r.ex_units.mem));
            buf.extend(encode_uint(r.ex_units.steps));
        }
        encode_map_close(&mut buf, redeemers.len());
    } else {
        // Pre-Conway list: [* [tag, index, data, ex_units]]
        buf.extend(encode_array_open(redeemers.len()));
        for &i in &order {
            let r = &redeemers[i];
            buf.extend(encode_array_header(4));
            buf.extend(encode_redeemer_tag(&r.tag));
            buf.extend(encode_uint(r.index as u64));
            buf.extend(encode_plutus_data(&r.data));
            buf.extend(encode_array_header(2));
            buf.extend(encode_uint(r.ex_units.mem));
            buf.extend(encode_uint(r.ex_units.steps));
        }
        encode_array_close(&mut buf, redeemers.len());
    }
    buf
}

/// Numeric ordering of a redeemer tag, matching both the CBOR tag value and the
/// `PlutusPurpose` constructor order in cardano-ledger (used to sort redeemers
/// and the redeemers map into canonical ascending-key order).
fn redeemer_tag_ord(tag: &RedeemerTag) -> u8 {
    match tag {
        RedeemerTag::Spend => 0,
        RedeemerTag::Mint => 1,
        RedeemerTag::Cert => 2,
        RedeemerTag::Reward => 3,
        RedeemerTag::Vote => 4,
        RedeemerTag::Propose => 5,
        RedeemerTag::Guarding => 6,
    }
}

/// Encode the `plutus_data` witness term (witness-set key 4 / the datums term
/// of the script-integrity preimage) as CBOR.
///
/// Canonicalizes to match cardano-ledger's `TxDats = Map DataHash (Data era)`,
/// serialized as `encodeWithSetTag . Map.elems`:
/// - The datums are ordered by ascending `DataHash` (`blake2b_256` of each
///   datum's encoding) — `Map.elems` yields ascending-key order.
/// - `use_set_tag` (true from Conway/PV9 on) prepends the set tag 258
///   (`encodeWithSetTag`); pre-Conway emits a bare array.
///
/// The caller must ensure `plutus_data` is non-empty — an empty `TxDats`
/// contributes nothing to the preimage and is omitted by the callers (#887).
pub fn encode_datums(plutus_data: &[PlutusData], use_set_tag: bool) -> Vec<u8> {
    // Order by ascending datum hash via an index permutation.
    let hashes: Vec<[u8; 32]> = plutus_data
        .iter()
        .map(|d| *blake2b_256(&encode_plutus_data(d)).as_bytes())
        .collect();
    let mut order: Vec<usize> = (0..plutus_data.len()).collect();
    order.sort_by(|&a, &b| hashes[a].cmp(&hashes[b]));

    let mut buf = Vec::new();
    if use_set_tag {
        buf.extend(encode_tag(258));
    }
    buf.extend(encode_array_open(plutus_data.len()));
    for &i in &order {
        buf.extend(encode_plutus_data(&plutus_data[i]));
    }
    encode_array_close(&mut buf, plutus_data.len());
    buf
}

/// Encode a transaction witness set using era-specific encoding rules.
///
/// - Conway: redeemers use map format `{ [tag, index] => [data, ex_units] }`
///   (per Conway CDDL `nonempty_map<redeemer_key, redeemer_value>`)
/// - Pre-Conway (Alonzo/Babbage): redeemers use array format
///   `[* [tag, index, data, ex_units]]`
pub(super) fn encode_witness_set_for_era(ws: &TransactionWitnessSet, era: Era) -> Vec<u8> {
    let mut count = 0;
    if !ws.vkey_witnesses.is_empty() {
        count += 1;
    }
    if !ws.native_scripts.is_empty() {
        count += 1;
    }
    if !ws.bootstrap_witnesses.is_empty() {
        count += 1;
    }
    if !ws.plutus_v1_scripts.is_empty() {
        count += 1;
    }
    if !ws.plutus_data.is_empty() {
        count += 1;
    }
    if !ws.redeemers.is_empty() {
        count += 1;
    }
    if !ws.plutus_v2_scripts.is_empty() {
        count += 1;
    }
    if !ws.plutus_v3_scripts.is_empty() {
        count += 1;
    }

    // Haskell `AlonzoTxWits` (reused by Conway) wraps witness-set keys
    // 0/1/2/3/6/7 in CBOR tag 258 from PV9 on — keys 0 and 2 because `Set`'s own
    // `encodeSet` emits it, keys 1/3/6/7 because they go through
    // `encodeWithSetTag` (#939):
    //
    //   encodeWithSetTag xs =
    //     ifEncodingVersionAtLeast (natVersion @9)
    //       (encodeTag setTag <> encCBOR xs) (encCBOR xs)
    //
    // Key 4 (plutus_data) gets the tag inside `encode_datums`; key 5 (redeemers)
    // is a map from PV9 on and never carries it.
    //
    // NOTE: Haskell iterates `Set` / `Map.elems`, i.e. Ord-sorted, whereas dugite
    // preserves wire order here. That ordering difference is deliberately NOT
    // addressed by this change — see #939.
    let use_set_tag = matches!(era, Era::Conway | Era::Dijkstra);

    let mut buf = encode_map_header(count);

    if !ws.vkey_witnesses.is_empty() {
        buf.extend(encode_uint(0));
        if use_set_tag {
            buf.extend(encode_tag(258));
        }
        buf.extend(encode_array_open(ws.vkey_witnesses.len()));
        for w in &ws.vkey_witnesses {
            buf.extend(encode_vkey_witness(w));
        }
        encode_array_close(&mut buf, ws.vkey_witnesses.len());
    }

    if !ws.native_scripts.is_empty() {
        buf.extend(encode_uint(1));
        if use_set_tag {
            buf.extend(encode_tag(258));
        }
        buf.extend(encode_array_open(ws.native_scripts.len()));
        for s in &ws.native_scripts {
            buf.extend(encode_native_script(s));
        }
        encode_array_close(&mut buf, ws.native_scripts.len());
    }

    if !ws.bootstrap_witnesses.is_empty() {
        buf.extend(encode_uint(2));
        if use_set_tag {
            buf.extend(encode_tag(258));
        }
        buf.extend(encode_array_open(ws.bootstrap_witnesses.len()));
        for w in &ws.bootstrap_witnesses {
            buf.extend(encode_bootstrap_witness(w));
        }
        encode_array_close(&mut buf, ws.bootstrap_witnesses.len());
    }

    if !ws.plutus_v1_scripts.is_empty() {
        buf.extend(encode_uint(3));
        if use_set_tag {
            buf.extend(encode_tag(258));
        }
        buf.extend(encode_array_open(ws.plutus_v1_scripts.len()));
        for s in &ws.plutus_v1_scripts {
            buf.extend(encode_bytes(s));
        }
        encode_array_close(&mut buf, ws.plutus_v1_scripts.len());
    }

    if !ws.plutus_data.is_empty() {
        buf.extend(encode_uint(4));
        // Canonical datums term — hash-sorted, tag-258 wrapped from Conway on.
        // Shared with the script-data-hash preimage so the two stay identical.
        buf.extend(encode_datums(
            &ws.plutus_data,
            matches!(era, Era::Conway | Era::Dijkstra),
        ));
    }

    if !ws.redeemers.is_empty() {
        buf.extend(encode_uint(5));
        // Single canonical redeemers encoder — map form in Conway/Dijkstra,
        // list form pre-Conway. Shared with the script-data-hash preimage.
        buf.extend(encode_redeemers(
            &ws.redeemers,
            matches!(era, Era::Conway | Era::Dijkstra),
        ));
    }

    if !ws.plutus_v2_scripts.is_empty() {
        buf.extend(encode_uint(6));
        if use_set_tag {
            buf.extend(encode_tag(258));
        }
        buf.extend(encode_array_open(ws.plutus_v2_scripts.len()));
        for s in &ws.plutus_v2_scripts {
            buf.extend(encode_bytes(s));
        }
        encode_array_close(&mut buf, ws.plutus_v2_scripts.len());
    }

    if !ws.plutus_v3_scripts.is_empty() {
        buf.extend(encode_uint(7));
        if use_set_tag {
            buf.extend(encode_tag(258));
        }
        buf.extend(encode_array_open(ws.plutus_v3_scripts.len()));
        for s in &ws.plutus_v3_scripts {
            buf.extend(encode_bytes(s));
        }
        encode_array_close(&mut buf, ws.plutus_v3_scripts.len());
    }

    buf
}

/// Encode auxiliary data.
///
/// If only metadata and no scripts: metadata map directly.
/// Otherwise: tag 259 with map {0: metadata, 1: native_scripts, 2: plutus_v1, 3: plutus_v2, 4: plutus_v3}
pub fn encode_auxiliary_data(aux: &AuxiliaryData) -> Vec<u8> {
    let has_scripts = !aux.native_scripts.is_empty()
        || !aux.plutus_v1_scripts.is_empty()
        || !aux.plutus_v2_scripts.is_empty()
        || !aux.plutus_v3_scripts.is_empty();

    if !has_scripts {
        // Simple metadata map
        return encode_metadata_map(&aux.metadata);
    }

    // Alonzo+ format: tag 259 { 0: metadata, 1: native_scripts, ... }
    let mut buf = encode_tag(259);
    let mut count = 0;
    if !aux.metadata.is_empty() {
        count += 1;
    }
    if !aux.native_scripts.is_empty() {
        count += 1;
    }
    if !aux.plutus_v1_scripts.is_empty() {
        count += 1;
    }
    if !aux.plutus_v2_scripts.is_empty() {
        count += 1;
    }
    if !aux.plutus_v3_scripts.is_empty() {
        count += 1;
    }

    buf.extend(encode_map_header(count));

    if !aux.metadata.is_empty() {
        buf.extend(encode_uint(0));
        buf.extend(encode_metadata_map(&aux.metadata));
    }
    if !aux.native_scripts.is_empty() {
        buf.extend(encode_uint(1));
        buf.extend(encode_array_open(aux.native_scripts.len()));
        for s in &aux.native_scripts {
            buf.extend(encode_native_script(s));
        }
        encode_array_close(&mut buf, aux.native_scripts.len());
    }
    if !aux.plutus_v1_scripts.is_empty() {
        buf.extend(encode_uint(2));
        buf.extend(encode_array_open(aux.plutus_v1_scripts.len()));
        for s in &aux.plutus_v1_scripts {
            buf.extend(encode_bytes(s));
        }
        encode_array_close(&mut buf, aux.plutus_v1_scripts.len());
    }
    if !aux.plutus_v2_scripts.is_empty() {
        buf.extend(encode_uint(3));
        buf.extend(encode_array_open(aux.plutus_v2_scripts.len()));
        for s in &aux.plutus_v2_scripts {
            buf.extend(encode_bytes(s));
        }
        encode_array_close(&mut buf, aux.plutus_v2_scripts.len());
    }
    if !aux.plutus_v3_scripts.is_empty() {
        buf.extend(encode_uint(4));
        buf.extend(encode_array_open(aux.plutus_v3_scripts.len()));
        for s in &aux.plutus_v3_scripts {
            buf.extend(encode_bytes(s));
        }
        encode_array_close(&mut buf, aux.plutus_v3_scripts.len());
    }

    buf
}

/// Encode a transaction body as CBOR map (Conway format by default).
///
/// This is a compatibility wrapper that always uses Conway-era encoding
/// (tag 258 for set fields). For era-specific encoding, use
/// `encode_transaction_body_for_era`.
///
/// Required keys: 0=inputs, 1=outputs, 2=fee
/// Optional keys: 3=ttl, 4=certs, 5=withdrawals, 7=aux_data_hash, 8=validity_start,
///                9=mint, 11=script_data_hash, 13=collateral, 14=required_signers,
///                15=network_id, 16=collateral_return, 17=total_collateral,
///                18=reference_inputs, 19=voting_procedures, 20=proposal_procedures,
///                21=treasury_value, 22=donation
pub fn encode_transaction_body(body: &TransactionBody) -> Vec<u8> {
    encode_transaction_body_for_era(body, Era::Conway)
}

/// Encode a transaction body as CBOR map using era-specific encoding rules.
///
/// - Conway: inputs, certificates, collateral, and reference_inputs are
///   encoded as CBOR tag 258 sets (`#6.258([* item])`) with items sorted
///   lexicographically by their CBOR encoding.
/// - Pre-Conway: those fields are encoded as plain definite-length arrays.
pub(super) fn encode_transaction_body_for_era(body: &TransactionBody, era: Era) -> Vec<u8> {
    // Count fields
    let mut count = 3; // inputs, outputs, fee always present
    if body.ttl.is_some() {
        count += 1;
    }
    if !body.certificates.is_empty() {
        count += 1;
    }
    if !body.withdrawals.is_empty() {
        count += 1;
    }
    if body.auxiliary_data_hash.is_some() {
        count += 1;
    }
    if body.validity_interval_start.is_some() {
        count += 1;
    }
    if !body.mint.is_empty() {
        count += 1;
    }
    if body.script_data_hash.is_some() {
        count += 1;
    }
    if !body.collateral.is_empty() {
        count += 1;
    }
    // Key 14: required_signers (Conway) OR guards (Dijkstra+).
    //
    // - Era < Dijkstra: emit when `required_signers` is non-empty.
    // - Era >= Dijkstra: emit when EITHER `guards` is non-empty OR
    //   `required_signers` is non-empty (the latter is synthesised as
    //   `Credential::VerificationKey` guards so legacy CLI/mempool flows
    //   that only populated `required_signers` keep working on Dijkstra).
    //   Issue #475 Phase 3.5.
    let emit_key14_dijkstra =
        era >= Era::Dijkstra && (!body.guards.is_empty() || !body.required_signers.is_empty());
    let emit_key14_legacy = era < Era::Dijkstra && !body.required_signers.is_empty();
    if emit_key14_dijkstra || emit_key14_legacy {
        count += 1;
    }
    if body.network_id.is_some() {
        count += 1;
    }
    if body.collateral_return.is_some() {
        count += 1;
    }
    if body.total_collateral.is_some() {
        count += 1;
    }
    if !body.reference_inputs.is_empty() {
        count += 1;
    }
    if !body.voting_procedures.is_empty() {
        count += 1;
    }
    if !body.proposal_procedures.is_empty() {
        count += 1;
    }
    if body.treasury_value.is_some() {
        count += 1;
    }
    if body.donation.is_some() {
        count += 1;
    }
    // Dijkstra TxBody key 23: sub_transactions. The Haskell encoder emits
    // `Omit null (Key 23 $ To dtbrSubTransactions)`, i.e. it ONLY appears
    // when non-empty. We mirror that exactly: never write the key on a
    // pre-Dijkstra body, and never write it on a Dijkstra body that
    // happens to carry an empty list (canonical wire shape requires the
    // omission, otherwise the Haskell side would round-trip to a
    // structurally distinct CBOR map).
    let emit_sub_transactions = era >= Era::Dijkstra && !body.sub_transactions.is_empty();
    if emit_sub_transactions {
        count += 1;
    }
    // Dijkstra TxBody key 26: account_balance_intervals (issue #475 Phase 3.3).
    // Same `Omit null` discipline as key 23: only emitted on Dijkstra+ bodies
    // that actually declare at least one interval.
    let emit_account_balance_intervals =
        era >= Era::Dijkstra && !body.account_balance_intervals.is_empty();
    if emit_account_balance_intervals {
        count += 1;
    }
    // Dijkstra TxBody key 25: direct_deposits (issue #475 Phase 3.4).
    // Same `Omit null` discipline as key 23/26: only emitted on Dijkstra+
    // bodies that carry a non-empty deposit map. Wire-symmetric with
    // withdrawals (key 5).
    let emit_direct_deposits = era >= Era::Dijkstra && !body.direct_deposits.is_empty();
    if emit_direct_deposits {
        count += 1;
    }

    let mut buf = encode_map_header(count);

    // 0: inputs
    // Conway CDDL: set<transaction_input> = #6.258([* transaction_input])
    // Pre-Conway CDDL: [* transaction_input]  (plain array, no tag 258)
    buf.extend(encode_uint(0));
    buf.extend(encode_set_for_era(era, &body.inputs, encode_tx_input));

    // 1: outputs
    buf.extend(encode_uint(1));
    buf.extend(encode_array_open(body.outputs.len()));
    for output in &body.outputs {
        buf.extend(encode_transaction_output(output));
    }
    encode_array_close(&mut buf, body.outputs.len());

    // 2: fee
    buf.extend(encode_uint(2));
    buf.extend(encode_uint(body.fee.0));

    // 3: ttl
    if let Some(ttl) = body.ttl {
        buf.extend(encode_uint(3));
        buf.extend(encode_uint(ttl.0));
    }

    // 4: certificates
    // Conway CDDL: nonempty_oset<certificate> = #6.258([+ certificate])
    // Pre-Conway CDDL: [* certificate]  (plain array, no tag 258)
    //
    // Conway's `ctbrCerts` is an OSet, NOT a Set — insertion-ordered, so the
    // items must never be sorted (certificate order is semantically
    // load-bearing). Pre-Conway `stbCerts :: StrictSeq (TxCert era)` is an
    // untagged, equally order-preserving array. #940.
    if !body.certificates.is_empty() {
        buf.extend(encode_uint(4));
        if matches!(era, Era::Conway | Era::Dijkstra) {
            buf.extend(encode_ordered_set(&body.certificates, encode_certificate));
        } else {
            buf.extend(encode_plain_array(&body.certificates, encode_certificate));
        }
    }

    // 5: withdrawals
    //
    // Haskell `Withdrawals` is `deriving newtype EncCBOR` over
    // `Map AccountAddress Coin` — generic Map instance — so `encodeMap`
    // semantics apply (#932): definite <= 23 entries, indefinite above.
    if !body.withdrawals.is_empty() {
        buf.extend(encode_uint(5));
        buf.extend(encode_map_open(body.withdrawals.len()));
        for (addr, amount) in &body.withdrawals {
            buf.extend(encode_bytes(addr));
            buf.extend(encode_uint(amount.0));
        }
        encode_map_close(&mut buf, body.withdrawals.len());
    }

    // 7: auxiliary_data_hash
    if let Some(hash) = &body.auxiliary_data_hash {
        buf.extend(encode_uint(7));
        buf.extend(encode_hash32(hash));
    }

    // 8: validity_interval_start
    if let Some(start) = body.validity_interval_start {
        buf.extend(encode_uint(8));
        buf.extend(encode_uint(start.0));
    }

    // 9: mint
    if !body.mint.is_empty() {
        buf.extend(encode_uint(9));
        buf.extend(encode_mint(&body.mint));
    }

    // 11: script_data_hash
    if let Some(hash) = &body.script_data_hash {
        buf.extend(encode_uint(11));
        buf.extend(encode_hash32(hash));
    }

    // 13: collateral
    // Conway CDDL: set<transaction_input> = #6.258([* transaction_input])
    // Pre-Conway CDDL: [* transaction_input]  (plain array, no tag 258)
    if !body.collateral.is_empty() {
        buf.extend(encode_uint(13));
        buf.extend(encode_set_for_era(era, &body.collateral, encode_tx_input));
    }

    // 14: required_signers (Conway) OR guards (Dijkstra+).
    //
    // - Era < Dijkstra: nonempty_set<addr_keyhash> per CDDL — each entry
    //   is a bare bstr(28) (lop off the Hash32 padding).
    // - Era >= Dijkstra: OSet (Credential Guard) per upstream
    //   `Cardano.Ledger.Dijkstra.TxBody`. Each entry is the standard
    //   `[type, hash28]` Credential encoding (0 = key, 1 = script). If
    //   the in-memory body only populates `required_signers`, we synthesise
    //   key-hash credentials for it so the wire output is well-typed.
    //   Issue #475 Phase 3.5.
    if emit_key14_legacy {
        buf.extend(encode_uint(14));
        buf.extend(encode_array_open(body.required_signers.len()));
        for hash in &body.required_signers {
            buf.extend(encode_bytes(&hash.as_bytes()[..28]));
        }
        encode_array_close(&mut buf, body.required_signers.len());
    } else if emit_key14_dijkstra {
        buf.extend(encode_uint(14));
        // Compose the on-wire set from `guards` when present; otherwise fall
        // back to the legacy `required_signers` projection.
        let mut entries: Vec<dugite_primitives::credentials::Credential> = if body.guards.is_empty()
        {
            body.required_signers
                .iter()
                .map(|h32| {
                    let mut bytes = [0u8; 28];
                    bytes.copy_from_slice(&h32.as_bytes()[..28]);
                    dugite_primitives::credentials::Credential::VerificationKey(
                        dugite_primitives::hash::Hash::from_bytes(bytes),
                    )
                })
                .collect()
        } else {
            body.guards.clone()
        };
        // Sort for canonical OSet encoding (matches Haskell's OSet
        // ordering: derived `Ord` on `Credential` is VK < Script, then by
        // hash bytes — which is exactly the `BTreeMap`/`Ord` derive that
        // `Credential` already carries in dugite-primitives).
        entries.sort();
        entries.dedup();
        buf.extend(encode_array_open(entries.len()));
        for cred in &entries {
            buf.extend(super::certificate::encode_credential(cred));
        }
        encode_array_close(&mut buf, entries.len());
    }

    // 15: network_id
    if let Some(nid) = body.network_id {
        buf.extend(encode_uint(15));
        buf.extend(encode_uint(nid as u64));
    }

    // 16: collateral_return
    if let Some(output) = &body.collateral_return {
        buf.extend(encode_uint(16));
        buf.extend(encode_transaction_output(output));
    }

    // 17: total_collateral
    if let Some(total) = body.total_collateral {
        buf.extend(encode_uint(17));
        buf.extend(encode_uint(total.0));
    }

    // 18: reference_inputs
    // Conway CDDL: set<transaction_input> = #6.258([* transaction_input])
    // Pre-Conway (Babbage) CDDL: [* transaction_input]  (plain array, no tag 258)
    if !body.reference_inputs.is_empty() {
        buf.extend(encode_uint(18));
        buf.extend(encode_set_for_era(
            era,
            &body.reference_inputs,
            encode_tx_input,
        ));
    }

    // 19: voting_procedures
    if !body.voting_procedures.is_empty() {
        buf.extend(encode_uint(19));
        buf.extend(encode_voting_procedures(&body.voting_procedures));
    }

    // 20: proposal_procedures
    if !body.proposal_procedures.is_empty() {
        buf.extend(encode_uint(20));
        // OSet, like certs: unconditional tag 258 + order-preserving
        // variable-length array (#940). Conway-only field, so no era branch.
        buf.extend(encode_ordered_set(
            &body.proposal_procedures,
            encode_proposal_procedure,
        ));
    }

    // 21: treasury_value
    if let Some(treasury) = body.treasury_value {
        buf.extend(encode_uint(21));
        buf.extend(encode_uint(treasury.0));
    }

    // 22: donation
    if let Some(donation) = body.donation {
        buf.extend(encode_uint(22));
        buf.extend(encode_uint(donation.0));
    }

    // 23: sub_transactions (Dijkstra+) — OMap TxId (Tx SubTx era).
    //
    // Wire shape: a bare ARRAY OF VALUES, not a map. Haskell's `OMap` never
    // encodes its keys — they are reconstructed on decode from each value via
    // `HasOKey.toOKey` (#936). `libs/cardano-data/src/Data/OMap/Strict.hs`:
    //
    //   instance (EncCBOR v, Ord k) => EncCBOR (OMap k v) where
    //     encCBOR omap = encodeStrictSeq encCBOR (toStrictSeq omap)
    //
    // `encodeStrictSeq` is `variableListLenEncoding`, hence the open/close
    // pair rather than a fixed definite header (#938).
    //
    // When a sub-tx was decoded from chain we round-trip its raw body bytes
    // verbatim, so the reconstructed id is byte-exact; when it was
    // constructed in-memory we synthesise a body via `encode_sub_tx_body`.
    if emit_sub_transactions {
        let len = body.sub_transactions.len();
        buf.extend(encode_uint(23));
        buf.extend(encode_array_open(len));
        for sub in &body.sub_transactions {
            match &sub.raw_body_cbor {
                Some(bytes) => buf.extend_from_slice(bytes),
                None => buf.extend(encode_sub_tx_body(sub)),
            }
        }
        encode_array_close(&mut buf, len);
    }

    // 25: direct_deposits (Dijkstra+) — atomic ADA flow into reward
    // accounts. Wire-symmetric with withdrawals (key 5):
    //   map { reward_account_bytes => coin }
    // The encoder uses the canonical map ordering BTreeMap provides; this
    // matches the Haskell `encodeFoldable . Map.toAscList` discipline.
    // Issue #475 Phase 3.4. Haskell `DirectDeposits` is `deriving newtype
    // EncCBOR` over `Map AccountAddress Coin` (same shape as Withdrawals),
    // so `encodeMap` semantics apply (#932).
    if emit_direct_deposits {
        buf.extend(encode_uint(25));
        buf.extend(encode_map_open(body.direct_deposits.len()));
        for (addr, amount) in &body.direct_deposits {
            buf.extend(encode_bytes(addr));
            buf.extend(encode_uint(amount.0));
        }
        encode_map_close(&mut buf, body.direct_deposits.len());
    }

    // 26: account_balance_intervals (Dijkstra+) — per-account balance
    // predicate gating apply on reward-account balances. Wire shape:
    //   map { stake_credential => [ coin / null, coin / null ] }
    // The Haskell decoder rejects `[null, null]`; we mirror that in the
    // decoder, so a defaulted `AccountBalanceInterval { lower: None, upper: None }`
    // would round-trip to a decode failure. We don't proactively skip
    // such entries here because the only way to construct one in-memory
    // is via the public `AccountBalanceInterval::is_degenerate()` check,
    // which callers should run first.
    // Haskell `AccountBalanceIntervals` is `deriving newtype EncCBOR` over
    // `Map AccountId (AccountBalanceInterval era)` — generic Map instance —
    // so `encodeMap` semantics apply (#932).
    if emit_account_balance_intervals {
        buf.extend(encode_uint(26));
        buf.extend(encode_map_open(body.account_balance_intervals.len()));
        for (cred, iv) in &body.account_balance_intervals {
            buf.extend(encode_credential(cred));
            buf.extend(encode_account_balance_interval(iv));
        }
        encode_map_close(&mut buf, body.account_balance_intervals.len());
    }

    buf
}

/// Encode a single `AccountBalanceInterval` as a 2-element CBOR array
/// `[lower, upper]`, with `null` for missing bounds.
fn encode_account_balance_interval(
    iv: &dugite_primitives::transaction::AccountBalanceInterval,
) -> Vec<u8> {
    let mut buf = encode_array_header(2);
    match iv.lower {
        Some(c) => buf.extend(encode_uint(c.0)),
        None => buf.extend(encode_null()),
    }
    match iv.upper {
        Some(c) => buf.extend(encode_uint(c.0)),
        None => buf.extend(encode_null()),
    }
    buf
}

/// Encode a Dijkstra SubTx body as the upstream `DijkstraSubTxBodyRaw`
/// CBOR map (subset of keys we currently model — keys 0/1/3/7/8/18). Other
/// keys are not emitted by the in-memory path; if you need them, decode
/// the body once and let the round-trip path reuse `raw_body_cbor`.
fn encode_sub_tx_body(sub: &dugite_primitives::transaction::SubTransaction) -> Vec<u8> {
    let mut count = 2; // inputs + outputs (always present)
    if sub.ttl.is_some() {
        count += 1;
    }
    if sub.auxiliary_data_hash.is_some() {
        count += 1;
    }
    if sub.validity_interval_start.is_some() {
        count += 1;
    }
    if !sub.reference_inputs.is_empty() {
        count += 1;
    }

    let mut buf = encode_map_header(count);
    // 0: inputs (Dijkstra: tag 258 set semantics)
    buf.extend(encode_uint(0));
    buf.extend(encode_set_for_era(
        Era::Dijkstra,
        &sub.inputs,
        crate::cbor::encode_tx_input,
    ));
    // 1: outputs
    buf.extend(encode_uint(1));
    buf.extend(encode_array_open(sub.outputs.len()));
    for out in &sub.outputs {
        buf.extend(encode_transaction_output(out));
    }
    encode_array_close(&mut buf, sub.outputs.len());
    // 3: ttl
    if let Some(ttl) = sub.ttl {
        buf.extend(encode_uint(3));
        buf.extend(encode_uint(ttl.0));
    }
    // 7: auxiliary_data_hash
    if let Some(h) = &sub.auxiliary_data_hash {
        buf.extend(encode_uint(7));
        buf.extend(crate::cbor::encode_bytes(h.as_bytes()));
    }
    // 8: validity_interval_start
    if let Some(s) = sub.validity_interval_start {
        buf.extend(encode_uint(8));
        buf.extend(encode_uint(s.0));
    }
    // 18: reference_inputs
    if !sub.reference_inputs.is_empty() {
        buf.extend(encode_uint(18));
        buf.extend(encode_set_for_era(
            Era::Dijkstra,
            &sub.reference_inputs,
            crate::cbor::encode_tx_input,
        ));
    }
    buf
}

/// Encode a complete transaction.
///
/// The wire shape depends on `tx.era`:
/// - **Pre-Dijkstra (Byron … Conway):** `[body, witness_set, is_valid, auxiliary_data]`
///   — a 4-element array, where `is_valid` is the explicit author-signaled
///   Phase-2 outcome.
/// - **Dijkstra and later (CIP-0167):** `[body, witness_set, auxiliary_data]`
///   — a 3-element array. The author no longer signals validity; the ledger
///   determines it dynamically from the Phase-2 script outcome and applies
///   collateral consumption on failure. `tx.is_valid` is **ignored** by this
///   encoder for Dijkstra+, mirroring the Haskell `OmitC dtIsValid` in
///   `Cardano.Ledger.Dijkstra.Tx.toCBORForMempoolSubmission`.
///
/// Body and witness-set encoding remain era-aware:
/// - Conway/Dijkstra: tag 258 for set fields in body; map format for redeemers
/// - Pre-Conway: plain arrays for set fields; array format for redeemers
pub fn encode_transaction(tx: &Transaction) -> Vec<u8> {
    if tx.era >= Era::Dijkstra {
        encode_dijkstra_transaction(tx)
    } else {
        encode_pre_dijkstra_transaction(tx)
    }
}

/// Encode a pre-Dijkstra (Byron … Conway) standalone transaction.
///
/// Wire shape: `[body, witness_set, is_valid, auxiliary_data]` — 4 elements.
fn encode_pre_dijkstra_transaction(tx: &Transaction) -> Vec<u8> {
    let mut buf = encode_array_header(4);
    buf.extend(encode_transaction_body_for_era(&tx.body, tx.era));
    buf.extend(encode_witness_set_for_era(&tx.witness_set, tx.era));
    buf.extend(encode_bool(tx.is_valid));
    match &tx.auxiliary_data {
        Some(aux) => buf.extend(encode_auxiliary_data(aux)),
        None => buf.extend(encode_null()),
    }
    buf
}

/// Encode a Dijkstra (CIP-0167) standalone transaction.
///
/// Wire shape: `[body, witness_set, auxiliary_data]` — 3 elements. The
/// `is_valid` flag is omitted from the wire form per CIP-0167; on the receive
/// side, validity is determined dynamically by Phase-2 script evaluation, not
/// signaled by the author. See [`encode_transaction`] for the rationale.
pub fn encode_dijkstra_transaction(tx: &Transaction) -> Vec<u8> {
    let mut buf = encode_array_header(3);
    buf.extend(encode_transaction_body_for_era(&tx.body, tx.era));
    buf.extend(encode_witness_set_for_era(&tx.witness_set, tx.era));
    match &tx.auxiliary_data {
        Some(aux) => buf.extend(encode_auxiliary_data(aux)),
        None => buf.extend(encode_null()),
    }
    buf
}

/// Compute the transaction hash from the body encoding (blake2b-256 of CBOR body).
///
/// This overload re-encodes the body from its parsed fields using Conway-era
/// encoding (tag 258 for set fields).  Use this for forged transactions where
/// the body is constructed in memory and `raw_body_cbor` is `None`.
///
/// For decoded transactions, prefer [`compute_transaction_hash_from_tx`] which
/// uses the preserved original wire bytes when available.
pub fn compute_transaction_hash(body: &TransactionBody) -> Hash32 {
    let body_cbor = encode_transaction_body(body);
    blake2b_256(&body_cbor)
}

/// Compute the transaction hash from a complete decoded transaction.
///
/// # D11 / audit #544 — tx hash invariance
///
/// When a transaction is decoded from wire CBOR (e.g., from a block received
/// over the network), the in-house decoder computes the hash over the original raw bytes and
/// stores it in `tx.hash`.  Our `encode_transaction_body` re-encodes from parsed
/// fields, which may differ from the original bytes for:
///
/// - Non-canonical input orderings in pre-Conway transactions
/// - Dijkstra-era transactions decoded by older code paths
/// - Any encoding detail that the in-house decoder captures via KeepRaw but we cannot reproduce
///
/// This function uses `tx.raw_body_cbor` when available — the exact bytes that
/// produced `tx.hash` — and falls back to re-encoding only for forged transactions
/// where `raw_body_cbor` is `None`.
///
/// Haskell cardano-node always hashes the original wire bytes; it never re-encodes.
/// This function matches that behaviour.
pub fn compute_transaction_hash_from_tx(tx: &Transaction) -> Hash32 {
    if let Some(raw) = &tx.raw_body_cbor {
        // Use the preserved original body bytes: guaranteed to match tx.hash.
        blake2b_256(raw)
    } else {
        // Forged transaction: re-encode from fields using era-aware encoding.
        let body_cbor = encode_transaction_body_for_era(&tx.body, tx.era);
        blake2b_256(&body_cbor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::compute_script_data_hash;
    use dugite_primitives::{
        address::{Address, EnterpriseAddress},
        credentials::Credential,
        hash::{Hash28, Hash32},
        network::NetworkId,
        time::SlotNo,
        transaction::{
            AuxiliaryData, ExUnits, NativeScript, OutputDatum, PlutusData, Redeemer, RedeemerTag,
            ScriptRef, TransactionBody, TransactionInput, TransactionMetadatum, TransactionOutput,
            TransactionWitnessSet, VKeyWitness,
        },
        value::{Lovelace, Value},
    };
    use std::collections::BTreeMap;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// A simple enterprise address on Testnet backed by an all-zero key hash.
    fn test_address() -> Address {
        Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(Hash28::ZERO),
        })
    }

    /// Build a minimal ADA-only Value.
    fn ada(lovelace: u64) -> Value {
        Value {
            coin: Lovelace(lovelace),
            multi_asset: BTreeMap::new(),
        }
    }

    /// A single dummy TransactionInput used wherever a body needs at least one input.
    fn dummy_input() -> TransactionInput {
        TransactionInput {
            transaction_id: Hash32::ZERO,
            index: 0,
        }
    }

    /// Build the minimal TransactionBody (inputs, outputs, fee).
    fn minimal_body() -> TransactionBody {
        TransactionBody {
            inputs: vec![dummy_input()],
            outputs: vec![TransactionOutput {
                address: test_address(),
                value: ada(1_000_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            }],
            fee: Lovelace(170_000),
            ttl: None,
            certificates: vec![],
            withdrawals: BTreeMap::new(),
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: BTreeMap::new(),
            script_data_hash: None,
            collateral: vec![],
            required_signers: vec![],
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: vec![],
            update: None,
            voting_procedures: BTreeMap::new(),
            proposal_procedures: vec![],
            treasury_value: None,
            donation: None,
            sub_transactions: vec![],
            account_balance_intervals: vec![],
            direct_deposits: BTreeMap::new(),
            guards: vec![],
        }
    }

    /// An empty witness set.
    fn empty_witness_set() -> TransactionWitnessSet {
        TransactionWitnessSet {
            vkey_witnesses: vec![],
            native_scripts: vec![],
            bootstrap_witnesses: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            plutus_data: vec![],
            redeemers: vec![],
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            original_script_data_hash: None,
        }
    }

    // ── encode_transaction_output (legacy) ───────────────────────────────────

    /// Legacy ADA-only output: array(2) [address_bytes, coin]
    #[test]
    fn test_legacy_output_ada_only() {
        let output = TransactionOutput {
            address: test_address(),
            value: ada(2_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: true,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // First byte: array(2) = 0x82
        assert_eq!(encoded[0], 0x82, "legacy ADA-only must be array(2)");
        // Total length: 1 (array hdr) + len(addr bytes) + len(value)
        // address bytes for enterprise testnet + zero key = 29 bytes → encode_bytes starts 0x58, 29
        assert_eq!(encoded[1], 0x58, "address bytes length-prefix");
        assert_eq!(encoded[2], 29, "enterprise address is 29 bytes");
    }

    /// Legacy output with datum hash: array(3) [address, value, datum_hash]
    #[test]
    fn test_legacy_output_with_datum_hash() {
        let h = Hash32::from_bytes([0xab; 32]);
        let output = TransactionOutput {
            address: test_address(),
            value: ada(1_000_000),
            datum: OutputDatum::DatumHash(h),
            script_ref: None,
            is_legacy: true,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // First byte: array(3) = 0x83
        assert_eq!(
            encoded[0], 0x83,
            "legacy output with datum hash must be array(3)"
        );
    }

    // ── encode_transaction_output (post-Alonzo) ──────────────────────────────

    /// Minimal post-Alonzo output: map(2) {0: address, 1: value}
    #[test]
    fn test_post_alonzo_output_minimal() {
        let output = TransactionOutput {
            address: test_address(),
            value: ada(1_000_000),
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // First byte: map(2) = 0xa2
        assert_eq!(
            encoded[0], 0xa2,
            "post-Alonzo minimal output must be map(2)"
        );
        // Key 0 must immediately follow
        assert_eq!(encoded[1], 0x00, "first map key must be 0 (address)");
    }

    /// Post-Alonzo output with datum hash: map(3) with key 2 → [0, hash]
    #[test]
    fn test_post_alonzo_output_with_datum_hash() {
        let h = Hash32::from_bytes([0xcd; 32]);
        let output = TransactionOutput {
            address: test_address(),
            value: ada(500_000),
            datum: OutputDatum::DatumHash(h),
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // map(3) = 0xa3
        assert_eq!(encoded[0], 0xa3, "output with datum hash must be map(3)");
    }

    /// Post-Alonzo output with inline datum: map(3) with key 2 → [1, tag(24)(bytes)]
    #[test]
    fn test_post_alonzo_output_with_inline_datum() {
        let output = TransactionOutput {
            address: test_address(),
            value: ada(500_000),
            datum: OutputDatum::InlineDatum {
                data: PlutusData::Integer(num_bigint::BigInt::from(42i64)),
                raw_cbor: None,
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // map(3) = 0xa3
        assert_eq!(encoded[0], 0xa3, "output with inline datum must be map(3)");
        // Scan for key 2
        let key2_pos = encoded.iter().position(|&b| b == 0x02);
        assert!(
            key2_pos.is_some(),
            "map must contain key 2 for datum_option"
        );
    }

    /// Post-Alonzo output with inline datum uses raw_cbor when provided.
    #[test]
    fn test_post_alonzo_output_inline_datum_uses_raw_cbor() {
        // raw_cbor = CBOR for integer 99 = 0x18 0x63
        let raw = vec![0x18u8, 0x63u8];
        let output = TransactionOutput {
            address: test_address(),
            value: ada(500_000),
            datum: OutputDatum::InlineDatum {
                data: PlutusData::Integer(num_bigint::BigInt::from(1i64)), // ignored because raw_cbor is set
                raw_cbor: Some(raw.clone()),
            },
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // The raw bytes 0x18 0x63 must appear inside the encoding (inside the tag(24) bstr)
        let pos = encoded
            .windows(2)
            .position(|w| w == [0x18, 0x63])
            .expect("raw_cbor bytes must appear in encoding");
        let _ = pos; // just asserting presence
    }

    /// Post-Alonzo output with script_ref: map(3) with key 3
    #[test]
    fn test_post_alonzo_output_with_script_ref() {
        let output = TransactionOutput {
            address: test_address(),
            value: ada(1_000_000),
            datum: OutputDatum::None,
            script_ref: Some(ScriptRef::PlutusV2(vec![0xde, 0xad])),
            is_legacy: false,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // map(3) = 0xa3
        assert_eq!(encoded[0], 0xa3, "output with script_ref must be map(3)");
        // key 3 must appear
        assert!(
            encoded.contains(&0x03),
            "map must contain key 3 for script_ref"
        );
    }

    /// Post-Alonzo output with datum hash AND script_ref: map(4)
    #[test]
    fn test_post_alonzo_output_with_all_optional_fields() {
        let h = Hash32::from_bytes([0x01; 32]);
        let output = TransactionOutput {
            address: test_address(),
            value: ada(1_000_000),
            datum: OutputDatum::DatumHash(h),
            script_ref: Some(ScriptRef::PlutusV2(vec![0x01, 0x02])),
            is_legacy: false,
            raw_cbor: None,
        };
        let encoded = encode_transaction_output(&output);
        // map(4) = 0xa4
        assert_eq!(
            encoded[0], 0xa4,
            "output with datum + script_ref must be map(4)"
        );
    }

    // ── era-specific set encoding ────────────────────────────────────────────

    /// Conway body: inputs encoded with tag(258) prefix bytes 0xd9 0x01 0x02
    #[test]
    fn test_conway_inputs_use_tag_258() {
        let body = minimal_body();
        let encoded = encode_transaction_body_for_era(&body, Era::Conway);

        // After map header (0xa3) and key 0 (0x00), we should see tag(258) = 0xd9 0x01 0x02
        // Map header: 0xa3; key 0: 0x00 → positions 0, 1
        // Then immediately: 0xd9, 0x01, 0x02
        assert_eq!(encoded[0], 0xa3, "minimal body map(3)");
        assert_eq!(encoded[1], 0x00, "key 0 = inputs");
        assert_eq!(encoded[2], 0xd9, "tag prefix byte 1");
        assert_eq!(encoded[3], 0x01, "tag prefix byte 2");
        assert_eq!(encoded[4], 0x02, "tag prefix byte 3 — completes tag(258)");
    }

    /// Babbage body: inputs encoded as plain array (no tag 258)
    #[test]
    fn test_babbage_inputs_use_plain_array() {
        let body = minimal_body();
        let encoded = encode_transaction_body_for_era(&body, Era::Babbage);

        // After map(3) = 0xa3 and key 0 = 0x00, the array header for 1 input = 0x81
        assert_eq!(encoded[0], 0xa3, "minimal body map(3)");
        assert_eq!(encoded[1], 0x00, "key 0 = inputs");
        // No tag: next byte is array(1) = 0x81
        assert_eq!(encoded[2], 0x81, "plain array(1) for Babbage inputs");
    }

    /// D5 / audit #544: Dijkstra body must also encode inputs with tag(258).
    /// Dijkstra inherits Conway CDDL verbatim — plain arrays produce a wrong body hash.
    #[test]
    fn test_dijkstra_inputs_use_tag_258() {
        let body = minimal_body();
        let encoded = encode_transaction_body_for_era(&body, Era::Dijkstra);

        assert_eq!(encoded[0], 0xa3, "minimal body map(3)");
        assert_eq!(encoded[1], 0x00, "key 0 = inputs");
        // tag(258) must appear immediately after key 0
        assert_eq!(
            encoded[2], 0xd9,
            "Dijkstra inputs: tag prefix byte 1 (0xd9)"
        );
        assert_eq!(
            encoded[3], 0x01,
            "Dijkstra inputs: tag prefix byte 2 (0x01)"
        );
        assert_eq!(
            encoded[4], 0x02,
            "Dijkstra inputs: tag prefix byte 3 (0x02)"
        );
    }

    /// D5 / audit #544: Dijkstra witness set must use Conway map format for redeemers.
    #[test]
    fn test_dijkstra_witness_redeemers_map_format() {
        let mut ws = empty_witness_set();
        ws.redeemers.push(Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(num_bigint::BigInt::from(1i64)),
            ex_units: ExUnits {
                mem: 1000,
                steps: 2000,
            },
        });
        let encoded = encode_witness_set_for_era(&ws, Era::Dijkstra);
        // map(1) = 0xa1
        assert_eq!(
            encoded[0], 0xa1,
            "Dijkstra witness set with redeemers: map(1)"
        );
        assert_eq!(encoded[1], 0x05, "redeemer key must be 5");
        // Dijkstra uses Conway map format: map(1) = 0xa1 for one redeemer
        assert_eq!(
            encoded[2], 0xa1,
            "Dijkstra redeemers must use Conway map format, not Babbage array format"
        );
    }

    /// D5 roundtrip: Dijkstra-era encode/decode hash invariant.
    /// Encodes a body as Dijkstra, hashes it, and verifies it differs from
    /// the Babbage-encoded version (which uses plain arrays).
    #[test]
    fn test_dijkstra_body_hash_differs_from_babbage() {
        use dugite_primitives::hash::blake2b_256;
        let body = minimal_body();
        let dijkstra_encoded = encode_transaction_body_for_era(&body, Era::Dijkstra);
        let babbage_encoded = encode_transaction_body_for_era(&body, Era::Babbage);
        // The Dijkstra encoding uses tag(258) for inputs; the Babbage encoding uses
        // plain array.  They must produce different bytes and therefore different hashes.
        assert_ne!(
            dijkstra_encoded, babbage_encoded,
            "Dijkstra encoding must differ from Babbage encoding"
        );
        let h_dijkstra = blake2b_256(&dijkstra_encoded);
        let h_conway = blake2b_256(&encode_transaction_body_for_era(&body, Era::Conway));
        // Dijkstra body hash must match Conway body hash (same CDDL)
        assert_eq!(
            h_dijkstra, h_conway,
            "Dijkstra and Conway body must have same hash (same CDDL)"
        );
    }

    // ── witness set redeemer format ──────────────────────────────────────────

    /// Conway witness set: redeemer at key 5 encoded as map (0xa1 for single redeemer)
    #[test]
    fn test_conway_witness_redeemers_map_format() {
        let mut ws = empty_witness_set();
        ws.redeemers.push(Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(num_bigint::BigInt::from(1i64)),
            ex_units: ExUnits {
                mem: 1000,
                steps: 2000,
            },
        });
        let encoded = encode_witness_set_for_era(&ws, Era::Conway);
        // map(1) = 0xa1
        assert_eq!(encoded[0], 0xa1, "witness set with redeemers only: map(1)");
        // key 5 = 0x05
        assert_eq!(encoded[1], 0x05, "redeemer key must be 5");
        // Conway map: map(1) = 0xa1 for one redeemer
        assert_eq!(encoded[2], 0xa1, "Conway redeemers encoded as map(1)");
    }

    /// Babbage witness set: redeemer at key 5 encoded as array (0x81 for single redeemer)
    #[test]
    fn test_babbage_witness_redeemers_array_format() {
        let mut ws = empty_witness_set();
        ws.redeemers.push(Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Integer(num_bigint::BigInt::from(1i64)),
            ex_units: ExUnits {
                mem: 1000,
                steps: 2000,
            },
        });
        let encoded = encode_witness_set_for_era(&ws, Era::Babbage);
        // map(1) = 0xa1
        assert_eq!(encoded[0], 0xa1, "witness set with redeemers only: map(1)");
        assert_eq!(encoded[1], 0x05, "redeemer key must be 5");
        // Babbage array: array(1) = 0x81 for one redeemer
        assert_eq!(encoded[2], 0x81, "Babbage redeemers encoded as array(1)");
    }

    // ── #887: canonical redeemers / datums ordering + tag-258 ────────────────

    fn rdmr(tag: RedeemerTag, index: u32) -> Redeemer {
        Redeemer {
            tag,
            index,
            data: PlutusData::Integer(num_bigint::BigInt::from(index as i64)),
            ex_units: ExUnits { mem: 1, steps: 2 },
        }
    }

    /// #887: `encode_redeemers` emits the map in canonical ascending
    /// `(tag, index)` order regardless of input order (matches Haskell's
    /// `Map.toAscList` over `PlutusPurpose`).
    #[test]
    fn encode_redeemers_sorts_by_tag_then_index() {
        // Deliberately unsorted input: (Mint,0), (Spend,3), (Spend,1).
        let unsorted = vec![
            rdmr(RedeemerTag::Mint, 0),
            rdmr(RedeemerTag::Spend, 3),
            rdmr(RedeemerTag::Spend, 1),
        ];
        let sorted = vec![
            rdmr(RedeemerTag::Spend, 1),
            rdmr(RedeemerTag::Spend, 3),
            rdmr(RedeemerTag::Mint, 0),
        ];
        assert_eq!(
            encode_redeemers(&unsorted, true),
            encode_redeemers(&sorted, true),
            "map form must be canonical-ordered"
        );
        assert_eq!(
            encode_redeemers(&unsorted, false),
            encode_redeemers(&sorted, false),
            "list form must be canonical-ordered too"
        );
        // Empty slice → era's empty container (the sentinel).
        assert_eq!(encode_redeemers(&[], true), vec![0xa0]);
        assert_eq!(encode_redeemers(&[], false), vec![0x80]);
    }

    /// #887: `encode_datums` sorts by datum hash and applies the tag-258 set
    /// wrapper from Conway on (bare array pre-Conway).
    #[test]
    fn encode_datums_sorts_by_hash_and_tags_conway() {
        let a = PlutusData::Bytes(vec![0x01]);
        let b = PlutusData::Bytes(vec![0x02, 0x03]);
        let c = PlutusData::Integer(num_bigint::BigInt::from(9i64));
        let forward = vec![a.clone(), b.clone(), c.clone()];
        let shuffled = vec![c, a, b];

        // Order-independent (canonical hash order).
        assert_eq!(
            encode_datums(&forward, true),
            encode_datums(&shuffled, true),
            "datums must sort by hash regardless of input order"
        );

        // Conway/Dijkstra: tag 258 prefix (0xd9 0x01 0x02).
        let conway = encode_datums(&forward, true);
        assert_eq!(
            &conway[..3],
            &[0xd9, 0x01, 0x02],
            "Conway datums must be tag-258"
        );
        assert_eq!(conway[3], 0x83, "…wrapping an array(3)");

        // Pre-Conway: bare array, no tag.
        let pre = encode_datums(&forward, false);
        assert_eq!(pre[0], 0x83, "pre-Conway datums are a bare array(3)");
    }

    /// #887 core invariant: a synthetic Conway tx built via the witness-set
    /// builder has key-4 (datums) and key-5 (redeemers) bytes identical to the
    /// terms the script-integrity hash re-encodes — so its `script_data_hash`
    /// matches its own witnesses. Previously the two diverged (witness datums
    /// were a bare array while the hash used tag-258).
    #[test]
    fn witness_set_and_script_data_hash_agree_on_terms() {
        use crate::encode::{encode_datums, encode_redeemers};
        let mut ws = empty_witness_set();
        ws.redeemers = vec![rdmr(RedeemerTag::Mint, 0), rdmr(RedeemerTag::Spend, 2)];
        ws.plutus_data = vec![
            PlutusData::Bytes(vec![0xaa]),
            PlutusData::Integer(num_bigint::BigInt::from(1i64)),
        ];

        let wire = encode_witness_set_for_era(&ws, Era::Conway);

        // Extract key-4 and key-5 raw values out of the witness-set map.
        let mut dec = minicbor::Decoder::new(&wire);
        let n = dec.map().unwrap().unwrap();
        let mut key4 = None;
        let mut key5 = None;
        for _ in 0..n {
            let k = dec.u32().unwrap();
            let start = dec.position();
            dec.skip().unwrap();
            let val = wire[start..dec.position()].to_vec();
            match k {
                4 => key4 = Some(val),
                5 => key5 = Some(val),
                _ => {}
            }
        }
        // The builder's terms equal the canonical primitives …
        assert_eq!(key4.unwrap(), encode_datums(&ws.plutus_data, true));
        assert_eq!(key5.unwrap(), encode_redeemers(&ws.redeemers, true));

        // … and the hash re-encodes the *same* bytes (raw = None path), so a
        // self-built tx's script_data_hash matches its own witness set.
        let cm = CostModels::default();
        let via_slices = compute_script_data_hash(
            &ws.redeemers,
            &ws.plutus_data,
            &cm,
            false,
            false,
            false,
            None,
            None,
            true,
        );
        let mut preimage = encode_redeemers(&ws.redeemers, true);
        preimage.extend(encode_datums(&ws.plutus_data, true));
        preimage.push(0xa0); // empty langviews
        assert_eq!(via_slices.as_bytes(), blake2b_256(&preimage).as_bytes());
    }

    // ── transaction body ─────────────────────────────────────────────────────

    /// Minimal body has exactly 3 keys (0, 1, 2) → map(3)
    #[test]
    fn test_transaction_body_minimal_map3() {
        let body = minimal_body();
        let encoded = encode_transaction_body(&body);
        // map(3) = 0xa3
        assert_eq!(encoded[0], 0xa3, "minimal body must be map(3)");
    }

    /// Body with TTL gains key 3 → map(4)
    #[test]
    fn test_transaction_body_with_ttl() {
        let mut body = minimal_body();
        body.ttl = Some(SlotNo(999_999));
        let encoded = encode_transaction_body(&body);
        // map(4) = 0xa4
        assert_eq!(encoded[0], 0xa4, "body with TTL must be map(4)");
    }

    /// Body with validity_interval_start gains key 8 → map(4)
    #[test]
    fn test_transaction_body_with_validity_start() {
        let mut body = minimal_body();
        body.validity_interval_start = Some(SlotNo(100));
        let encoded = encode_transaction_body(&body);
        assert_eq!(encoded[0], 0xa4, "body with validity start must be map(4)");
    }

    // ── full transaction encoding ────────────────────────────────────────────

    /// Full transaction: array(4) = 0x84
    #[test]
    fn test_encode_transaction_array4() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        // array(4) = 0x84
        assert_eq!(encoded[0], 0x84, "transaction must be array(4)");
    }

    /// is_valid=false encodes as CBOR false (0xf4)
    #[test]
    fn test_encode_transaction_is_valid_false() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: false,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        // Scan for 0xf4 (CBOR false) — it should appear as the 3rd element
        assert!(
            encoded.contains(&0xf4),
            "is_valid=false must encode as CBOR false (0xf4)"
        );
        // Verify 0xf5 (true) is NOT present
        assert!(
            !encoded.contains(&0xf5),
            "is_valid=false must not contain CBOR true (0xf5)"
        );
    }

    /// is_valid=true encodes as CBOR true (0xf5)
    #[test]
    fn test_encode_transaction_is_valid_true() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        assert!(
            encoded.contains(&0xf5),
            "is_valid=true must encode as CBOR true (0xf5)"
        );
    }

    /// Transaction without auxiliary data: last element is CBOR null (0xf6)
    #[test]
    fn test_encode_transaction_no_aux_data_null() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        // Last byte must be 0xf6 (null)
        assert_eq!(
            *encoded.last().unwrap(),
            0xf6,
            "no auxiliary data must produce trailing null (0xf6)"
        );
    }

    // ── auxiliary data ───────────────────────────────────────────────────────

    /// Simple auxiliary data (metadata only, no scripts): plain metadata map — no tag
    #[test]
    fn test_aux_data_metadata_only_no_tag() {
        let mut metadata = BTreeMap::new();
        metadata.insert(674_u64, TransactionMetadatum::Text("msg".to_string()));
        let aux = AuxiliaryData {
            metadata,
            native_scripts: vec![],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            raw_cbor: None,
        };
        let encoded = encode_auxiliary_data(&aux);
        // Simple metadata: map(1). No tag 259 prefix.
        // 0xd9 would be a 2-byte tag prefix — must NOT appear as first byte
        assert_ne!(
            encoded[0], 0xd9,
            "metadata-only aux data must not use tag 259 prefix"
        );
        // Must be a map header (major type 5 = 0xa0-0xbf)
        assert!(
            encoded[0] & 0xe0 == 0xa0,
            "metadata-only aux data must start with a map header"
        );
    }

    /// Auxiliary data with native scripts: tag(259) = 0xd9 0x01 0x03
    #[test]
    fn test_aux_data_with_scripts_uses_tag_259() {
        let aux = AuxiliaryData {
            metadata: BTreeMap::new(),
            native_scripts: vec![NativeScript::InvalidBefore(SlotNo(100))],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            raw_cbor: None,
        };
        let encoded = encode_auxiliary_data(&aux);
        // tag(259) = 0xd9 0x01 0x03
        assert_eq!(encoded[0], 0xd9, "aux with scripts: tag byte 1 (0xd9)");
        assert_eq!(encoded[1], 0x01, "aux with scripts: tag byte 2 (0x01)");
        assert_eq!(
            encoded[2], 0x03,
            "aux with scripts: tag byte 3 (0x03 = 259)"
        );
    }

    // ── empty witness set ────────────────────────────────────────────────────

    /// Empty witness set: map(0) = 0xa0
    #[test]
    fn test_empty_witness_set() {
        let ws = empty_witness_set();
        let encoded = encode_witness_set(&ws);
        assert_eq!(
            encoded,
            vec![0xa0],
            "empty witness set must be map(0) = 0xa0"
        );
    }

    // ── witness set with vkeys ───────────────────────────────────────────────

    /// Witness set with one vkey: map(1) with key 0
    #[test]
    fn test_witness_set_with_vkeys_map1_key0() {
        let mut ws = empty_witness_set();
        ws.vkey_witnesses.push(VKeyWitness {
            vkey: vec![0u8; 32],
            signature: vec![0u8; 64],
        });
        let encoded = encode_witness_set(&ws);
        // map(1) = 0xa1
        assert_eq!(encoded[0], 0xa1, "witness set with vkeys must be map(1)");
        // key 0 for vkey_witnesses
        assert_eq!(encoded[1], 0x00, "vkey_witnesses map key must be 0");
    }

    // ── compute_transaction_hash ─────────────────────────────────────────────

    /// Hash must be deterministic (same body → same hash)
    #[test]
    fn test_compute_transaction_hash_deterministic() {
        let body = minimal_body();
        let h1 = compute_transaction_hash(&body);
        let h2 = compute_transaction_hash(&body);
        assert_eq!(h1, h2, "transaction hash must be deterministic");
    }

    /// Hash must be non-zero for a non-empty body
    #[test]
    fn test_compute_transaction_hash_non_zero() {
        let body = minimal_body();
        let h = compute_transaction_hash(&body);
        assert_ne!(
            h,
            Hash32::ZERO,
            "transaction hash of real body must be non-zero"
        );
    }

    /// Two different bodies must produce different hashes
    #[test]
    fn test_compute_transaction_hash_differs_for_different_bodies() {
        let body1 = minimal_body();
        let mut body2 = minimal_body();
        body2.fee = Lovelace(999_999);
        let h1 = compute_transaction_hash(&body1);
        let h2 = compute_transaction_hash(&body2);
        assert_ne!(h1, h2, "different bodies must produce different hashes");
    }

    // ── D11: compute_transaction_hash_from_tx ────────────────────────────────

    /// D11: When raw_body_cbor is None (forged tx), falls back to re-encoding.
    #[test]
    fn test_compute_hash_from_tx_no_raw_body_matches_reencoded() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body: body.clone(),
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let from_tx = compute_transaction_hash_from_tx(&tx);
        let from_body = compute_transaction_hash(&body);
        assert_eq!(
            from_tx, from_body,
            "without raw_body_cbor, must fall back to re-encode = same as compute_transaction_hash"
        );
    }

    /// D11: When raw_body_cbor is Some, must hash the raw bytes directly.
    /// This ensures tx.hash (computed by the in-house decoder from wire bytes) is preserved.
    #[test]
    fn test_compute_hash_from_tx_uses_raw_body_cbor() {
        use dugite_primitives::hash::blake2b_256;
        // Craft raw_body_cbor that is NOT a valid transaction body — just a known byte sequence.
        // compute_transaction_hash_from_tx must hash these exact bytes.
        let raw_body: Vec<u8> = vec![0xde, 0xad, 0xbe, 0xef];
        let expected_hash = blake2b_256(&raw_body);

        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: expected_hash,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: Some(raw_body),
            raw_witness_cbor: None,
        };
        let from_tx = compute_transaction_hash_from_tx(&tx);
        assert_eq!(
            from_tx, expected_hash,
            "with raw_body_cbor set, must hash raw bytes (not re-encode)"
        );
        // Also verify it differs from the re-encode hash
        let from_body = compute_transaction_hash(&tx.body);
        assert_ne!(
            from_tx, from_body,
            "raw_body hash must differ from re-encoded hash when raw differs from re-encoded"
        );
    }

    // ── Dijkstra CIP-0167: isValid removed from wire ─────────────────────────

    /// Dijkstra standalone tx is array(3): [body, witness, aux_data].
    /// No is_valid bool element appears on the wire.
    #[test]
    fn test_encode_dijkstra_transaction_array3_no_is_valid() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Dijkstra,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        // array(3) header = 0x83
        assert_eq!(
            encoded[0], 0x83,
            "Dijkstra transaction must be array(3) per CIP-0167"
        );
        // The wire MUST NOT contain a CBOR bool (0xf4 false or 0xf5 true) —
        // the is_valid byte is omitted.
        assert!(
            !encoded.contains(&0xf4),
            "Dijkstra tx wire MUST NOT carry is_valid=false (0xf4)"
        );
        assert!(
            !encoded.contains(&0xf5),
            "Dijkstra tx wire MUST NOT carry is_valid=true (0xf5)"
        );
        // The last byte is the trailing null for absent aux_data (0xf6).
        assert_eq!(
            *encoded.last().unwrap(),
            0xf6,
            "Dijkstra tx without aux data must end with CBOR null"
        );
    }

    /// CIP-0167: even when the in-memory `Transaction.is_valid == false`, the
    /// Dijkstra encoder must NOT emit a wire bool. Author-supplied validity
    /// is irrelevant in Dijkstra — it's derived dynamically at apply time.
    #[test]
    fn test_encode_dijkstra_transaction_is_valid_false_still_omits_byte() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Dijkstra,
            body,
            witness_set: empty_witness_set(),
            is_valid: false,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        assert_eq!(encoded[0], 0x83, "Dijkstra tx is still array(3)");
        assert!(
            !encoded.contains(&0xf4) && !encoded.contains(&0xf5),
            "Dijkstra wire MUST omit the is_valid bool even when is_valid=false"
        );
    }

    /// Conway (era < Dijkstra) MUST still emit array(4) with the is_valid byte.
    /// This pins that the era dispatch doesn't accidentally regress pre-Dijkstra
    /// encoding.
    #[test]
    fn test_encode_conway_transaction_still_array4() {
        let body = minimal_body();
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        assert_eq!(
            encoded[0], 0x84,
            "Conway tx wire shape is unchanged (array(4))"
        );
        assert!(
            encoded.contains(&0xf5),
            "Conway is_valid=true must encode as CBOR true (0xf5)"
        );
    }

    /// Round-trip Dijkstra: encode then decode via the in-house dispatch
    /// (`decode_transaction(7, ..)` → `decode_dijkstra_tx_standalone`).
    /// Body fields must survive, era must be Dijkstra, and is_valid defaults
    /// to true (CIP-0167 dynamic semantics — no on-wire signal).
    #[test]
    fn test_dijkstra_transaction_round_trip_through_dispatch() {
        let mut body = minimal_body();
        body.fee = Lovelace(444_555);
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Dijkstra,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        // HFC era id 7 == Dijkstra in `decode_transaction`.
        let decoded =
            crate::decode::decode_transaction(7, &encoded).expect("Dijkstra tx must decode");
        assert_eq!(
            decoded.era,
            Era::Dijkstra,
            "era must round-trip as Dijkstra"
        );
        assert_eq!(
            decoded.body.fee,
            Lovelace(444_555),
            "fee survives round-trip"
        );
        assert_eq!(decoded.body.inputs.len(), tx.body.inputs.len());
        assert_eq!(decoded.body.outputs.len(), tx.body.outputs.len());
        // is_valid defaults to true on Dijkstra-decoded txs (no wire signal).
        assert!(decoded.is_valid);
        // raw_body_cbor must be preserved for hash-stability invariants.
        assert!(decoded.raw_body_cbor.is_some());
    }

    /// A 4-element CBOR array offered as Dijkstra must fail to decode — the
    /// Conway shape is rejected, the loosening would mask malformed input.
    #[test]
    fn test_decode_dijkstra_rejects_array4_conway_shape() {
        // Build a Conway-shaped (array(4)) tx and ask the Dijkstra dispatch
        // to decode it. The decoder MUST refuse rather than silently accept
        // the legacy shape — defense in depth per Dugite's adversarial-input
        // posture.
        let body = minimal_body();
        let conway_tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let conway_encoded = encode_transaction(&conway_tx);
        assert_eq!(
            conway_encoded[0], 0x84,
            "sanity: Conway encoded as array(4)"
        );
        // Dispatch as Dijkstra → must error.
        let err = crate::decode::decode_transaction(7, &conway_encoded)
            .expect_err("Dijkstra decoder must reject array(4) Conway shape");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("array(3)")
                || msg.contains("array(4)")
                || msg.to_lowercase().contains("dijkstra"),
            "error should explain the array-shape mismatch, got: {msg}"
        );
    }

    // ── Dijkstra TxBody key 26: account_balance_intervals (issue #475 Phase 3.3) ──

    /// Per-account balance interval predicates round-trip through the
    /// Dijkstra encoder / decoder. Pins:
    ///
    /// 1. Encoder emits a `map { stake_credential => [ coin/null, coin/null ] }`
    ///    keyed under TxBody key 26 ONLY when era >= Dijkstra AND the list is
    ///    non-empty (matches Haskell `Omit null`).
    /// 2. Decoder recovers the same ordered list back into
    ///    `TransactionBody.account_balance_intervals` for a Dijkstra body.
    /// 3. The Conway encoder NEVER emits key 26 even when the field is
    ///    populated in-memory.
    /// 4. The decoder rejects `[null, null]` matching the Haskell
    ///    `DecoderErrorCustom "AccountBalanceInterval" "Both interval bounds cannot be nil."`
    #[test]
    fn account_balance_intervals_roundtrip_dijkstra() {
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::era::Era;
        use dugite_primitives::transaction::AccountBalanceInterval;

        let key_cred = Credential::VerificationKey(Hash28::from_bytes([0xAA; 28]));
        let script_cred = Credential::Script(Hash28::from_bytes([0xBB; 28]));

        // Three interval shapes:
        //  - both bounds: [100, 200)
        //  - lower only:  >= 1_000_000
        //  - upper only:  < 5
        let intervals = vec![
            (
                key_cred.clone(),
                AccountBalanceInterval::closed_open(Lovelace(100), Lovelace(200)),
            ),
            (
                script_cred.clone(),
                AccountBalanceInterval::at_least(Lovelace(1_000_000)),
            ),
            (
                Credential::VerificationKey(Hash28::from_bytes([0xCC; 28])),
                AccountBalanceInterval::below(Lovelace(5)),
            ),
        ];

        let mut body = minimal_body();
        body.account_balance_intervals = intervals.clone();

        // (1) Dijkstra encoder MUST emit key 26.
        let cbor = encode_transaction_body_for_era(&body, Era::Dijkstra);
        // Sentinel: key 26 = 0x18 0x1a (uint major-0 followed by single-byte 26).
        assert!(
            cbor.windows(2).any(|w| w == [0x18, 0x1a]),
            "Dijkstra encoder must emit TxBody key 26 (0x18 0x1a sentinel) when \
             account_balance_intervals is non-empty"
        );

        // (2) Round-trip through the Dijkstra-routed decoder.
        // We construct a full standalone Dijkstra tx so the public
        // `decode_transaction(7, ..)` entry point exercises this path
        // end-to-end (era_id=7 = Dijkstra).
        let tx = Transaction {
            era: Era::Dijkstra,
            hash: Hash32::ZERO, // recomputed by decoder
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let tx_cbor = encode_transaction(&tx);
        let decoded = crate::decode::decode_transaction(7, &tx_cbor)
            .expect("Dijkstra tx with account_balance_intervals must round-trip");
        assert_eq!(decoded.era, Era::Dijkstra);
        assert_eq!(
            decoded.body.account_balance_intervals, intervals,
            "decoded account_balance_intervals must match the encoded input exactly"
        );

        // (3) Conway encoder MUST NOT emit key 26 even with the field populated.
        let mut conway_body = minimal_body();
        conway_body.account_balance_intervals = intervals.clone();
        let conway_cbor = encode_transaction_body_for_era(&conway_body, Era::Conway);
        assert!(
            !conway_cbor.windows(2).any(|w| w == [0x18, 0x1a]),
            "Conway encoder MUST NOT emit TxBody key 26 (account_balance_intervals \
             is Dijkstra-only) — found 0x18 0x1a sentinel in Conway output"
        );

        // (4) Empty list on Dijkstra: key 26 omitted (matches `Omit null`).
        let mut empty_body = minimal_body();
        empty_body.account_balance_intervals = vec![]; // explicit
        let empty_cbor = encode_transaction_body_for_era(&empty_body, Era::Dijkstra);
        assert!(
            !empty_cbor.windows(2).any(|w| w == [0x18, 0x1a]),
            "Dijkstra encoder MUST omit key 26 when account_balance_intervals is empty"
        );
    }

    /// The decoder must reject `[null, null]` AccountBalanceInterval (mirrors
    /// Haskell `DecoderErrorCustom "AccountBalanceInterval" "Both interval bounds cannot be nil."`).
    /// We construct a hand-rolled CBOR body that puts key 26 = map(1) with a
    /// single entry whose interval is `[null, null]`, and assert the
    /// decoder errors out before reaching the rest of the body.
    #[test]
    fn account_balance_intervals_decoder_rejects_both_null() {
        use crate::cbor::*;

        // Build: body = map { 0: inputs, 1: outputs, 2: fee, 26: { cred => [null, null] } }
        let mut buf = encode_map_header(4);
        // 0: inputs — empty set (tag 258)
        buf.extend(encode_uint(0));
        buf.extend(encode_tag(258));
        buf.extend(encode_array_header(0));
        // 1: outputs — empty array
        buf.extend(encode_uint(1));
        buf.extend(encode_array_header(0));
        // 2: fee
        buf.extend(encode_uint(2));
        buf.extend(encode_uint(0));
        // 26: map(1) { vkey-cred => [null, null] }
        buf.extend(encode_uint(26));
        buf.extend(encode_map_header(1));
        // credential = [0, hash28]
        buf.extend(encode_array_header(2));
        buf.extend(encode_uint(0));
        buf.extend(encode_bytes(&[0u8; 28]));
        // interval = [null, null]
        buf.extend(encode_array_header(2));
        buf.extend(encode_null());
        buf.extend(encode_null());

        // Wrap in a Dijkstra standalone tx: array(3)[body, ws, null].
        let mut tx_cbor = encode_array_header(3);
        tx_cbor.extend(&buf);
        // empty witness set: map(0)
        tx_cbor.extend(encode_map_header(0));
        // null aux
        tx_cbor.extend(encode_null());

        let err = crate::decode::decode_transaction(7, &tx_cbor)
            .expect_err("decoder must reject [null, null] AccountBalanceInterval");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("AccountBalanceInterval") || msg.contains("both bounds"),
            "error should mention AccountBalanceInterval / both bounds, got: {msg}"
        );
    }

    // ── Dijkstra TxBody key 25: direct_deposits (issue #475 Phase 3.4) ──

    /// Direct deposits (the inverse of withdrawals) round-trip through the
    /// Dijkstra encoder / decoder. Pins:
    ///
    /// 1. Encoder emits a `map { reward_account_bytes => coin }` keyed
    ///    under TxBody key 25 ONLY when era >= Dijkstra AND the map is
    ///    non-empty (matches Haskell `Omit null`).
    /// 2. Decoder recovers the same map into
    ///    `TransactionBody.direct_deposits` for a Dijkstra body.
    /// 3. The Conway encoder NEVER emits key 25 even when the field is
    ///    populated in-memory.
    /// 4. Empty map on Dijkstra: key 25 omitted (matches `Omit null`).
    ///
    /// Sentinel: TxBody key 25 encodes as the CBOR sequence `0x18 0x19`
    /// (uint major-0 with single-byte payload 25).
    #[test]
    fn direct_deposits_roundtrip_dijkstra() {
        use dugite_primitives::era::Era;

        // Two reward accounts with distinct credentials:
        //  - keyhash credential: header byte 0xE0 (mainnet, key-typed)
        //  - script credential:  header byte 0xF0 (mainnet, script-typed)
        let mut ra_key = vec![0xE0_u8];
        ra_key.extend_from_slice(&[0xA1; 28]);
        let mut ra_script = vec![0xF0_u8];
        ra_script.extend_from_slice(&[0xB2; 28]);

        let deposits: BTreeMap<Vec<u8>, Lovelace> = [
            (ra_key.clone(), Lovelace(1_500_000)),
            (ra_script.clone(), Lovelace(2_500_000)),
        ]
        .into_iter()
        .collect();

        let mut body = minimal_body();
        body.direct_deposits = deposits.clone();

        // (1) Dijkstra encoder MUST emit key 25.
        let cbor = encode_transaction_body_for_era(&body, Era::Dijkstra);
        assert!(
            cbor.windows(2).any(|w| w == [0x18, 0x19]),
            "Dijkstra encoder must emit TxBody key 25 (0x18 0x19 sentinel) when \
             direct_deposits is non-empty"
        );

        // (2) Round-trip through the Dijkstra-routed decoder via the public
        //     `decode_transaction(7, ..)` (era_id=7 = Dijkstra).
        let tx = Transaction {
            era: Era::Dijkstra,
            hash: Hash32::ZERO, // recomputed by decoder
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let tx_cbor = encode_transaction(&tx);
        let decoded = crate::decode::decode_transaction(7, &tx_cbor)
            .expect("Dijkstra tx with direct_deposits must round-trip");
        assert_eq!(decoded.era, Era::Dijkstra);
        assert_eq!(
            decoded.body.direct_deposits, deposits,
            "decoded direct_deposits must match the encoded input exactly"
        );

        // (3) Conway encoder MUST NOT emit key 25 even with the field populated.
        let mut conway_body = minimal_body();
        conway_body.direct_deposits = deposits.clone();
        let conway_cbor = encode_transaction_body_for_era(&conway_body, Era::Conway);
        assert!(
            !conway_cbor.windows(2).any(|w| w == [0x18, 0x19]),
            "Conway encoder MUST NOT emit TxBody key 25 (direct_deposits is \
             Dijkstra-only) — found 0x18 0x19 sentinel in Conway output"
        );

        // (4) Empty map on Dijkstra: key 25 omitted (matches `Omit null`).
        let mut empty_body = minimal_body();
        empty_body.direct_deposits = BTreeMap::new(); // explicit
        let empty_cbor = encode_transaction_body_for_era(&empty_body, Era::Dijkstra);
        assert!(
            !empty_cbor.windows(2).any(|w| w == [0x18, 0x19]),
            "Dijkstra encoder MUST omit key 25 when direct_deposits is empty"
        );
    }

    // ── Dijkstra TxBody key 14: guards (issue #475 Phase 3.5) ──

    /// Credential-based `guards` (TxBody key 14) round-trip through the
    /// Dijkstra encoder / decoder. Pins:
    ///
    /// 1. Dijkstra encoder emits key 14 as an OSet of `[type, hash28]`
    ///    credentials when `body.guards` is non-empty.
    /// 2. Decoder recovers the full credential list into
    ///    `TransactionBody.guards` (mixed KeyHash + Script).
    /// 3. The Conway encoder still emits key 14 as bare keyhashes
    ///    (`required_signers`) — the semantic upgrade is Dijkstra-only.
    /// 4. Decoder is backward-compatible: a Dijkstra body whose key-14
    ///    elements are bare 28-byte bstrs still decodes (each becomes
    ///    `Credential::VerificationKey`), matching upstream's
    ///    `decodeGuards` per-element peek.
    /// 5. `required_signers` stays populated with the VK subset of
    ///    `guards`, so legacy Conway-era consumers keep working when fed a
    ///    Dijkstra body.
    #[test]
    fn guards_roundtrip_dijkstra() {
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::era::Era;
        use dugite_primitives::hash::Hash28;

        let vk_a = Credential::VerificationKey(Hash28::from_bytes([0x11; 28]));
        let vk_b = Credential::VerificationKey(Hash28::from_bytes([0x22; 28]));
        let sc_a = Credential::Script(Hash28::from_bytes([0xAA; 28]));
        let sc_b = Credential::Script(Hash28::from_bytes([0xBB; 28]));

        let guards = vec![vk_a.clone(), sc_a.clone(), vk_b.clone(), sc_b.clone()];

        let mut body = minimal_body();
        body.guards = guards.clone();

        // (1) Dijkstra encoder emits key 14 (sentinel byte 0x0E).
        let cbor = encode_transaction_body_for_era(&body, Era::Dijkstra);
        assert!(
            cbor.contains(&0x0E),
            "Dijkstra encoder must emit TxBody key 14 (0x0E sentinel) when \
             guards is non-empty"
        );
        // Each entry must be the `[type, hash28]` array form (0x82 = array(2));
        // bare bstrs would not contain the type discriminator. We expect 4
        // occurrences of `0x82 0x00 0x58 0x1C ..28..` or `0x82 0x01 0x58 0x1C
        // ..28..`. Sufficient sanity: at least two `0x01` discriminators
        // (for the two script credentials) preceded by `0x82`.
        let mut script_entries = 0;
        for win in cbor.windows(2) {
            if win == [0x82, 0x01] {
                script_entries += 1;
            }
        }
        assert!(
            script_entries >= 2,
            "Dijkstra key 14 must carry script credentials in [type=1, h28] \
             form; found {script_entries} array(2)+disc=1 windows in {cbor:02x?}"
        );

        // (2) Round-trip through the Dijkstra-routed decoder.
        let tx = Transaction {
            era: Era::Dijkstra,
            hash: Hash32::ZERO,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let tx_cbor = encode_transaction(&tx);
        let decoded = crate::decode::decode_transaction(7, &tx_cbor)
            .expect("Dijkstra tx with guards must round-trip");
        // Decoded `guards` may be sorted (canonical OSet ordering); compare
        // as sorted vectors.
        let mut want = guards.clone();
        want.sort();
        let mut got = decoded.body.guards.clone();
        got.sort();
        assert_eq!(got, want, "decoded guards must match input set");
        // (5) required_signers projection: only the VK subset.
        let mut want_vk: Vec<Hash28> = guards
            .iter()
            .filter_map(|c| match c {
                Credential::VerificationKey(h) => Some(*h),
                _ => None,
            })
            .collect();
        want_vk.sort();
        let mut got_vk: Vec<Hash28> = decoded
            .body
            .required_signers
            .iter()
            .map(|h32| {
                let mut bytes = [0u8; 28];
                bytes.copy_from_slice(&h32.as_bytes()[..28]);
                Hash28::from_bytes(bytes)
            })
            .collect();
        got_vk.sort();
        assert_eq!(
            got_vk, want_vk,
            "required_signers must hold the VK subset of guards"
        );

        // (3) Conway encoder still emits bare keyhashes (legacy shape):
        // populate required_signers only, encode in Conway, check no
        // `[0x82, 0x01]` (script-credential array form) appears.
        let mut conway_body = minimal_body();
        conway_body.required_signers = vec![Hash28::from_bytes([0x11; 28]).to_hash32_padded()];
        let conway_cbor = encode_transaction_body_for_era(&conway_body, Era::Conway);
        assert!(
            conway_cbor.contains(&0x0E),
            "Conway encoder must still emit key 14 when required_signers populated"
        );
        // The Conway shape is bare bstr(28) — no array-of-2 wrapping per entry.
        for win in conway_cbor.windows(2) {
            assert_ne!(
                win,
                [0x82, 0x01],
                "Conway key 14 must NOT carry script-credential array(2) forms"
            );
        }
    }

    /// Decoder backward compatibility: a Dijkstra body whose key-14
    /// elements are bare bstr(28) (legacy Conway-shaped) still decodes —
    /// each entry materialises as `Credential::VerificationKey`. Mirrors
    /// upstream `decodeGuards`'s per-element token-type peek.
    #[test]
    fn guards_decoder_accepts_bare_keyhash_form() {
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::era::Era;
        use dugite_primitives::hash::Hash28;

        // Hand-craft a Dijkstra tx CBOR carrying key 14 as bare bstr(28):
        // body = { 0: inputs, 1: outputs, 2: fee, 14: [bstr(28)] }
        // Use the minimal_body encoder for keys 0/1/2 then splice in 14.
        let mut body = minimal_body();
        body.required_signers = vec![Hash28::from_bytes([0x77; 28]).to_hash32_padded()];
        body.guards = vec![]; // explicitly empty — encoder will fall back to
                              // synthesising guards from required_signers.
        let body_cbor = encode_transaction_body_for_era(&body, Era::Dijkstra);

        // Wrap as Dijkstra standalone tx (array(3) per CIP-0167) and decode.
        let mut tx_cbor = encode_array_header(3);
        tx_cbor.extend(body_cbor);
        tx_cbor.extend(encode_map_header(0)); // empty witness set
        tx_cbor.extend(encode_null()); // null aux data
        let decoded = crate::decode::decode_transaction(7, &tx_cbor)
            .expect("Dijkstra tx with bare-keyhash key 14 must decode");
        assert_eq!(decoded.body.guards.len(), 1);
        assert!(matches!(
            &decoded.body.guards[0],
            Credential::VerificationKey(h) if h.as_bytes() == &[0x77; 28]
        ));
        assert_eq!(decoded.body.required_signers.len(), 1);
    }

    /// `RequireGuard` (native script tag 6) round-trips through the
    /// Conway/Dijkstra witness-set encoder/decoder for both KeyHash and
    /// Script credential payloads.
    ///
    /// Native script tag 6 is **Dijkstra-only** — the Alonzo/Babbage
    /// decoders do NOT accept it (and that rejection is itself a useful
    /// invariant, asserted at the bottom of this test).
    #[test]
    fn require_guard_native_script_roundtrip() {
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::era::Era;
        use dugite_primitives::hash::Hash28;

        for cred in [
            Credential::VerificationKey(Hash28::from_bytes([0x33; 28])),
            Credential::Script(Hash28::from_bytes([0x44; 28])),
        ] {
            let script = NativeScript::RequireGuard(cred.clone());
            let cbor = encode_native_script(&script);
            // Layout: array(2) [uint 6, credential[type, h28]].
            assert_eq!(cbor[0], 0x82, "outer array(2)");
            assert_eq!(cbor[1], 0x06, "native script tag 6 (RequireGuard)");
            assert_eq!(cbor[2], 0x82, "inner credential array(2)");

            // Round-trip via a real Dijkstra tx whose witness set carries
            // the RequireGuard script. The Conway/Dijkstra `read_native_script`
            // is exercised through the public `decode_transaction` entry.
            let body = minimal_body();
            let mut ws = empty_witness_set();
            ws.native_scripts = vec![script.clone()];
            let tx = Transaction {
                era: Era::Dijkstra,
                hash: Hash32::ZERO,
                body,
                witness_set: ws,
                is_valid: true,
                auxiliary_data: None,
                raw_cbor: None,
                raw_body_cbor: None,
                raw_witness_cbor: None,
            };
            let tx_cbor = encode_transaction(&tx);
            let decoded = crate::decode::decode_transaction(7, &tx_cbor)
                .expect("Dijkstra tx with RequireGuard witness must round-trip");
            assert_eq!(decoded.witness_set.native_scripts.len(), 1);
            match &decoded.witness_set.native_scripts[0] {
                NativeScript::RequireGuard(c) => assert_eq!(c, &cred),
                other => panic!("expected RequireGuard, got {other:?}"),
            }
        }

        // Negative: Alonzo decoder must REJECT tag 6 (pre-Dijkstra
        // native scripts know nothing about RequireGuard).
        let cred = dugite_primitives::credentials::Credential::VerificationKey(
            dugite_primitives::hash::Hash28::from_bytes([0x55; 28]),
        );
        let cbor = encode_native_script(&NativeScript::RequireGuard(cred));
        let mut reader = crate::decode::reader::Reader::new(&cbor);
        let err = crate::decode::era_alonzo::read_native_script_from_cbor(&mut reader)
            .expect_err("Alonzo native script decoder must reject tag 6");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("unknown type 6"),
            "Alonzo native script rejection message should mention 'unknown type 6', got: {msg}"
        );
    }

    /// Redeemer tag 6 = `Guarding` round-trips through the encoder/decoder.
    #[test]
    fn redeemer_tag_guarding_is_six() {
        let cbor = encode_redeemer_tag(&RedeemerTag::Guarding);
        assert_eq!(cbor, vec![0x06], "Guarding tag must encode as bare 0x06");
    }

    // ── #932: Haskell `encodeMap` semantics for tx-body Map fields ──────────
    //
    // cardano-ledger-binary `encodeMap` (encoding version >= 2) emits a
    // DEFINITE-length map header for <= 23 entries and an INDEFINITE map
    // (0xbf ... 0xff) for > 23. `Withdrawals` (tx-body key 5),
    // `DirectDeposits` (Dijkstra key 25), `AccountBalanceIntervals`
    // (Dijkstra key 26) and the Conway map-form `Redeemers` are all plain
    // `Map`s on the Haskell side (generic `EncCBOR (Map k v)` instance;
    // oracle-verified 2026-07-31), so their synthetic re-encodes must follow
    // the same threshold. Expected bytes below are built EXPLICITLY (not via
    // the shared production helper) so a helper bug cannot self-verify.

    /// `n` distinct 29-byte reward-account keys, 1 lovelace each.
    fn withdrawals_n(n: usize) -> BTreeMap<Vec<u8>, Lovelace> {
        (0..n)
            .map(|i| {
                let mut addr = vec![0xe1u8; 29];
                addr[1] = (i / 256) as u8;
                addr[2] = (i % 256) as u8;
                (addr, Lovelace(1))
            })
            .collect()
    }

    /// Explicitly-built `{reward_account => coin}` map bytes in the definite
    /// or indefinite form (also reused for Dijkstra direct_deposits, which is
    /// wire-symmetric with withdrawals).
    fn raw_account_coin_map(w: &BTreeMap<Vec<u8>, Lovelace>, indefinite: bool) -> Vec<u8> {
        let mut buf = if indefinite {
            vec![0xbf]
        } else {
            encode_map_header(w.len())
        };
        for (addr, amount) in w {
            buf.extend(encode_bytes(addr));
            buf.extend(encode_uint(amount.0));
        }
        if indefinite {
            buf.push(0xff);
        }
        buf
    }

    /// 23 withdrawals: definite map(23) header (0xb7), no break byte.
    /// Withdrawals (key 5) is the last emitted field of this body, so the
    /// encoded body must END with the exact expected map bytes.
    #[test]
    fn withdrawals_23_entries_definite_header() {
        let mut body = minimal_body();
        body.withdrawals = withdrawals_n(23);
        let enc = encode_transaction_body_for_era(&body, Era::Conway);
        let expected = raw_account_coin_map(&body.withdrawals, false);
        assert_eq!(expected[0], 0xb7, "sanity: 23-entry definite header");
        assert!(
            enc.ends_with(&expected),
            "23-entry withdrawals must stay a definite map(23)"
        );
    }

    /// 24 withdrawals: indefinite map (0xbf ... 0xff).
    #[test]
    fn withdrawals_24_entries_indefinite() {
        let mut body = minimal_body();
        body.withdrawals = withdrawals_n(24);
        let enc = encode_transaction_body_for_era(&body, Era::Conway);
        let expected = raw_account_coin_map(&body.withdrawals, true);
        assert!(
            enc.ends_with(&expected),
            "24-entry withdrawals must open indefinite (0xbf) and close with break"
        );
        assert_eq!(*enc.last().unwrap(), 0xff, "break byte must terminate map");
    }

    /// 256 withdrawals: the indefinite form saves exactly 1 byte over the
    /// 3-byte definite header (0xb9 0x01 0x00) — the #930 divergence class.
    #[test]
    fn withdrawals_256_entries_indefinite_saves_one_byte() {
        let mut body = minimal_body();
        body.withdrawals = withdrawals_n(256);
        let enc = encode_transaction_body_for_era(&body, Era::Conway);
        let indefinite = raw_account_coin_map(&body.withdrawals, true);
        let definite = raw_account_coin_map(&body.withdrawals, false);
        assert_eq!(
            indefinite.len() + 1,
            definite.len(),
            "indefinite form must be exactly 1 byte shorter at 256 entries"
        );
        assert!(enc.ends_with(&indefinite));
    }

    /// `n` Spend redeemers with ascending indexes (already canonical order).
    fn spend_redeemers_n(n: usize) -> Vec<Redeemer> {
        (0..n)
            .map(|i| Redeemer {
                tag: RedeemerTag::Spend,
                index: i as u32,
                data: PlutusData::Integer(num_bigint::BigInt::from(1i64)),
                ex_units: ExUnits { mem: 1, steps: 1 },
            })
            .collect()
    }

    /// Explicit expected bytes for the Conway MAP-form redeemers term.
    fn raw_redeemers_map(rs: &[Redeemer], indefinite: bool) -> Vec<u8> {
        let mut buf = if indefinite {
            vec![0xbf]
        } else {
            encode_map_header(rs.len())
        };
        for r in rs {
            buf.extend(encode_array_header(2));
            buf.push(0x00); // RedeemerTag::Spend
            buf.extend(encode_uint(r.index as u64));
            buf.extend(encode_array_header(2));
            buf.extend(encode_plutus_data(&r.data));
            buf.extend(encode_array_header(2));
            buf.extend(encode_uint(r.ex_units.mem));
            buf.extend(encode_uint(r.ex_units.steps));
        }
        if indefinite {
            buf.push(0xff);
        }
        buf
    }

    /// Conway map-form redeemers at the 23/24 boundary: definite map(23)
    /// vs indefinite (0xbf ... 0xff). Haskell: `Redeemers` PV>=9 branch is a
    /// bare `encCBOR` on the Map — generic instance — `encodeMap`.
    #[test]
    fn redeemers_map_form_23_vs_24_entries_header_switch() {
        let rs23 = spend_redeemers_n(23);
        assert_eq!(
            encode_redeemers(&rs23, true),
            raw_redeemers_map(&rs23, false),
            "23-entry redeemers map must stay definite"
        );

        let rs24 = spend_redeemers_n(24);
        assert_eq!(
            encode_redeemers(&rs24, true),
            raw_redeemers_map(&rs24, true),
            "24-entry redeemers map must switch to indefinite"
        );
    }

    /// 256-entry map-form redeemers: 1-byte saving vs the definite header.
    #[test]
    fn redeemers_map_form_256_entries_indefinite_saves_one_byte() {
        let rs = spend_redeemers_n(256);
        let enc = encode_redeemers(&rs, true);
        let indefinite = raw_redeemers_map(&rs, true);
        let definite = raw_redeemers_map(&rs, false);
        assert_eq!(indefinite.len() + 1, definite.len());
        assert_eq!(enc, indefinite, "must be 1 byte shorter than definite");
    }

    /// #932 must NOT disturb the empty-redeemers sentinel: an empty map has
    /// 0 <= 23 entries, so the Conway form stays the definite `0xa0` and the
    /// pre-Conway list form stays `0x80` — exactly the era-gated
    /// script-integrity sentinel (see `compute_script_data_hash`).
    #[test]
    fn redeemers_empty_sentinel_unchanged_by_encode_map_threshold() {
        assert_eq!(
            encode_redeemers(&[], true),
            vec![0xa0],
            "Conway empty-redeemers sentinel must remain definite empty map"
        );
        assert_eq!(
            encode_redeemers(&[], false),
            vec![0x80],
            "pre-Conway empty-redeemers sentinel must remain empty list"
        );
    }

    /// Dijkstra direct_deposits (key 25) follows `encodeMap` semantics —
    /// Haskell `DirectDeposits` is `deriving newtype EncCBOR` over
    /// `Map AccountAddress Coin`, exactly like `Withdrawals`.
    #[test]
    fn direct_deposits_23_vs_24_entries_header_switch() {
        let mut body23 = minimal_body();
        body23.direct_deposits = withdrawals_n(23);
        let enc23 = encode_transaction_body_for_era(&body23, Era::Dijkstra);
        let expected23 = raw_account_coin_map(&body23.direct_deposits, false);
        assert!(
            enc23.ends_with(&expected23),
            "23-entry direct_deposits must stay definite"
        );

        let mut body24 = minimal_body();
        body24.direct_deposits = withdrawals_n(24);
        let enc24 = encode_transaction_body_for_era(&body24, Era::Dijkstra);
        let expected24 = raw_account_coin_map(&body24.direct_deposits, true);
        assert!(
            enc24.ends_with(&expected24),
            "24-entry direct_deposits must switch to indefinite"
        );
        assert_eq!(*enc24.last().unwrap(), 0xff);
    }

    /// Dijkstra account_balance_intervals (key 26) follows `encodeMap` —
    /// Haskell `AccountBalanceIntervals` is `deriving newtype EncCBOR` over
    /// `Map AccountId (AccountBalanceInterval era)`.
    #[test]
    fn account_balance_intervals_23_vs_24_entries_header_switch() {
        use dugite_primitives::transaction::AccountBalanceInterval;

        fn intervals_n(n: usize) -> Vec<(Credential, AccountBalanceInterval)> {
            (0..n)
                .map(|i| {
                    let mut b = [0u8; 28];
                    b[0] = (i / 256) as u8;
                    b[1] = (i % 256) as u8;
                    (
                        Credential::VerificationKey(Hash28::from_bytes(b)),
                        AccountBalanceInterval {
                            lower: Some(Lovelace(1)),
                            upper: None,
                        },
                    )
                })
                .collect()
        }

        let mut body23 = minimal_body();
        body23.account_balance_intervals = intervals_n(23);
        let enc23 = encode_transaction_body_for_era(&body23, Era::Dijkstra);
        // key 26 is the LAST field of this body; each entry is
        // credential(32 bytes: 0x82 0x00 0x58 0x1c + 28) + [1, null](3 bytes:
        // 0x82 0x01 0xf6) = 35 bytes. With 23 entries the map is definite: 0xb7.
        let map_start = enc23.len() - 23 * 35 - 1;
        assert_eq!(
            enc23[map_start], 0xb7,
            "23-entry account_balance_intervals must stay definite"
        );

        let mut body24 = minimal_body();
        body24.account_balance_intervals = intervals_n(24);
        let enc24 = encode_transaction_body_for_era(&body24, Era::Dijkstra);
        let map_start24 = enc24.len() - 1 - 24 * 35 - 1;
        assert_eq!(
            enc24[map_start24], 0xbf,
            "24-entry account_balance_intervals must open indefinite"
        );
        assert_eq!(*enc24.last().unwrap(), 0xff, "break byte must close map");
    }

    /// Decode-roundtrip: a Conway tx whose withdrawals map (24 entries) and
    /// witness-set redeemers map (25 entries) are both encoded INDEFINITE
    /// must decode back identically through the era-appropriate dispatch.
    #[test]
    fn indefinite_tx_maps_roundtrip_through_conway_decoder() {
        let mut body = minimal_body();
        body.withdrawals = withdrawals_n(24);
        let mut ws = empty_witness_set();
        ws.redeemers = spend_redeemers_n(25);
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Conway,
            body,
            witness_set: ws,
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        let decoded =
            crate::decode::decode_transaction(6, &encoded).expect("Conway tx must decode");
        assert_eq!(
            decoded.body.withdrawals, tx.body.withdrawals,
            "indefinite withdrawals map must round-trip"
        );
        assert_eq!(
            decoded.witness_set.redeemers.len(),
            25,
            "indefinite redeemers map must round-trip"
        );
    }

    /// Decode-roundtrip: Dijkstra direct_deposits with 24 entries
    /// (indefinite on the wire) survives the Dijkstra dispatch.
    #[test]
    fn indefinite_direct_deposits_roundtrip_through_dijkstra_decoder() {
        let mut body = minimal_body();
        body.direct_deposits = withdrawals_n(24);
        let tx = dugite_primitives::transaction::Transaction {
            hash: Hash32::ZERO,
            era: Era::Dijkstra,
            body,
            witness_set: empty_witness_set(),
            is_valid: true,
            auxiliary_data: None,
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        };
        let encoded = encode_transaction(&tx);
        let decoded =
            crate::decode::decode_transaction(7, &encoded).expect("Dijkstra tx must decode");
        assert_eq!(
            decoded.body.direct_deposits, tx.body.direct_deposits,
            "indefinite direct_deposits map must round-trip"
        );
    }

    // ── #938: variableListLenEncoding boundary (definite <=23 / indefinite >23) ──
    //
    // Haskell `variableListLenEncoding` (cardano-ledger-binary Encoder.hs) uses
    // `lengthThreshold = 23`, exactly like `variableMapLenEncoding` (#930/#932).
    // Every variable-length collection encoder funnels through it:
    // `encodeStrictSeq` (outputs), `encodeSet` (inputs, after the 258 tag),
    // `encodeList`, `encodeFoldableEncoder` (block body segments).

    /// The raw helper: the four sizes that bracket every CBOR header width.
    #[test]
    fn array_open_close_boundary() {
        for (n, head) in [
            (0usize, vec![0x80]),
            (23, vec![0x97]),
            (24, vec![0x9f]),
            (255, vec![0x9f]),
            (256, vec![0x9f]),
        ] {
            assert_eq!(encode_array_open(n), head, "open header at n={n}");
            let mut buf = Vec::new();
            encode_array_close(&mut buf, n);
            let expect_break = if n > 23 { vec![0xff] } else { Vec::new() };
            assert_eq!(buf, expect_break, "close byte at n={n}");
        }
    }

    /// Above 255 the indefinite form is genuinely SHORTER than the definite
    /// one — this is the byte that #930 over-counted for maps (a false
    /// `OutputValueTooLarge` reject). Same arithmetic for arrays.
    #[test]
    fn array_indefinite_is_one_byte_shorter_at_256() {
        // definite 256 = 0x99 0x01 0x00 (3 bytes); indefinite = 0x9f + 0xff (2).
        assert_eq!(encode_array_header(256).len(), 3);
        let mut framing = encode_array_open(256);
        encode_array_close(&mut framing, 256);
        assert_eq!(framing.len(), 2);

        // 24..=255 is a wash: definite 0x98 nn (2) vs 0x9f .. 0xff (2).
        assert_eq!(encode_array_header(255).len(), 2);
        let mut framing = encode_array_open(255);
        encode_array_close(&mut framing, 255);
        assert_eq!(framing.len(), 2);
    }

    fn body_with_outputs(n: usize) -> TransactionBody {
        let mut body = minimal_body();
        body.outputs = (0..n)
            .map(|i| TransactionOutput {
                address: test_address(),
                value: ada(1_000_000 + i as u64),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            })
            .collect();
        body
    }

    /// `ctbOutputs :: StrictSeq (Sized (TxOut era))` with `Key 1 (To ctbrOutputs)`
    /// — `encodeStrictSeq`, so the outputs array crosses the threshold too.
    #[test]
    fn tx_body_outputs_cross_the_23_24_threshold() {
        for (n, indefinite) in [(23usize, false), (24, true), (256, true)] {
            let enc = encode_transaction_body_for_era(&body_with_outputs(n), Era::Conway);
            // Locate key 1's value: the body map is integer-keyed and key 1
            // follows key 0's (tag-258) input set.
            let pos = find_key_value_start(&enc, 1);
            if indefinite {
                assert_eq!(enc[pos], 0x9f, "outputs must be indefinite at n={n}");
            } else {
                assert_eq!(enc[pos], 0x80 | 23, "outputs must be definite at n={n}");
            }
            // Round-trips regardless of framing.
            let decoded = crate::decode::era_conway::decode_conway_tx_body(
                &mut crate::decode::reader::Reader::new(&enc),
                Era::Conway,
            )
            .expect("body must decode");
            assert_eq!(decoded.outputs.len(), n, "output count at n={n}");
        }
    }

    /// `encodeSet` emits `encodeTag 258` and THEN a variable-length array, so
    /// the threshold applies inside the tag.
    #[test]
    fn tx_body_input_set_is_tagged_then_variable_length() {
        for (n, indefinite) in [(23usize, false), (24, true)] {
            let mut body = minimal_body();
            body.inputs = (0..n)
                .map(|i| TransactionInput {
                    transaction_id: Hash32::ZERO,
                    index: i as u32,
                })
                .collect();
            let enc = encode_transaction_body_for_era(&body, Era::Conway);
            let pos = find_key_value_start(&enc, 0);
            // tag 258 == 0xd9 0x01 0x02
            assert_eq!(&enc[pos..pos + 3], &[0xd9, 0x01, 0x02], "set tag at n={n}");
            if indefinite {
                assert_eq!(enc[pos + 3], 0x9f, "input set indefinite at n={n}");
            } else {
                assert_eq!(enc[pos + 3], 0x80 | 23, "input set definite at n={n}");
            }
            let decoded = crate::decode::era_conway::decode_conway_tx_body(
                &mut crate::decode::reader::Reader::new(&enc),
                Era::Conway,
            )
            .expect("body must decode");
            assert_eq!(decoded.inputs.len(), n, "input count at n={n}");
        }
    }

    /// Scan an integer-keyed CBOR map for `key` and return the offset of its
    /// value. Only handles the small uint keys used by tx bodies.
    fn find_key_value_start(enc: &[u8], key: u8) -> usize {
        let mut r = crate::decode::reader::Reader::new(enc);
        let n = r.read_map_header().expect("map header").expect("definite");
        for _ in 0..n {
            let k = r.read_uint().expect("key");
            let pos = r.position();
            if k == key as u64 {
                return pos;
            }
            r.skip().expect("skip value");
        }
        panic!("key {key} not found");
    }

    // ── #940: Conway certs / proposal_procedures are OSet, not Set ──

    /// `OSet` encodes `encodeTag setTag <> encodeStrictSeq encCBOR seq` over the
    /// INSERTION-ordered sequence. Sorting would reorder certificates, and
    /// certificate order is semantically load-bearing.
    #[test]
    fn conway_certificates_keep_wire_order_and_are_tagged() {
        use dugite_primitives::transaction::Certificate;

        // Two certs whose CBOR encodings sort the OPPOSITE way to how they are
        // supplied, so any sort is immediately visible.
        let hi = Credential::VerificationKey(Hash28::from_bytes([0xff; 28]));
        let lo = Credential::VerificationKey(Hash28::from_bytes([0x00; 28]));
        let body_certs = vec![
            Certificate::StakeRegistration(hi.clone()),
            Certificate::StakeRegistration(lo.clone()),
        ];

        let mut body = minimal_body();
        body.certificates = body_certs.clone();
        let enc = encode_transaction_body_for_era(&body, Era::Conway);
        let pos = find_key_value_start(&enc, 4);

        // Unconditional tag 258 — OSet's setTag has no version gate.
        assert_eq!(
            &enc[pos..pos + 3],
            &[0xd9, 0x01, 0x02],
            "certs must be tagged"
        );
        assert_eq!(enc[pos + 3], 0x82, "definite array of 2");

        // Order preserved: the 0xff cert must still come first.
        let first = encode_certificate(&body_certs[0]);
        assert_eq!(
            &enc[pos + 4..pos + 4 + first.len()],
            &first[..],
            "certificate order must NOT be sorted"
        );

        let decoded = crate::decode::era_conway::decode_conway_tx_body(
            &mut crate::decode::reader::Reader::new(&enc),
            Era::Conway,
        )
        .expect("body must decode");
        assert_eq!(decoded.certificates, body_certs, "order must round-trip");
    }

    /// Pre-Conway certs are `StrictSeq`: untagged, and equally order-preserving.
    #[test]
    fn pre_conway_certificates_are_untagged_and_ordered() {
        use dugite_primitives::transaction::Certificate;

        let hi = Credential::VerificationKey(Hash28::from_bytes([0xff; 28]));
        let lo = Credential::VerificationKey(Hash28::from_bytes([0x00; 28]));
        let certs = vec![
            Certificate::StakeRegistration(hi),
            Certificate::StakeRegistration(lo),
        ];
        let mut body = minimal_body();
        body.certificates = certs.clone();

        for era in [Era::Shelley, Era::Alonzo, Era::Babbage] {
            let enc = encode_transaction_body_for_era(&body, era);
            let pos = find_key_value_start(&enc, 4);
            assert_ne!(
                &enc[pos..pos + 3],
                &[0xd9, 0x01, 0x02],
                "{era:?} certs must NOT carry tag 258"
            );
            assert_eq!(enc[pos], 0x82, "{era:?} definite array of 2");
            let first = encode_certificate(&certs[0]);
            assert_eq!(
                &enc[pos + 1..pos + 1 + first.len()],
                &first[..],
                "{era:?} certificate order must be preserved"
            );
        }
    }

    /// proposal_procedures is an OSet too — it was emitting a bare array.
    #[test]
    fn proposal_procedures_are_tagged() {
        let mut body = minimal_body();
        body.proposal_procedures = vec![dugite_primitives::transaction::ProposalProcedure {
            deposit: Lovelace(100_000_000_000),
            return_addr: vec![0xE0; 29],
            gov_action: dugite_primitives::transaction::GovAction::InfoAction,
            anchor: dugite_primitives::transaction::Anchor {
                url: "https://example.com".to_string(),
                data_hash: Hash32::ZERO,
            },
        }];
        let enc = encode_transaction_body_for_era(&body, Era::Conway);
        let pos = find_key_value_start(&enc, 20);
        assert_eq!(
            &enc[pos..pos + 3],
            &[0xd9, 0x01, 0x02],
            "proposal_procedures must carry tag 258"
        );
        assert_eq!(enc[pos + 3], 0x81, "definite array of 1");
    }

    // ── #939: Conway witness-set keys 0/1/2/3/6/7 carry the 258 set tag ──

    /// `encodeWithSetTag` is gated on `natVersion @9`, so Conway/Dijkstra emit
    /// the tag and every earlier era must not.
    #[test]
    fn witness_set_collections_are_tag_258_from_conway_on() {
        let ws = TransactionWitnessSet {
            vkey_witnesses: vec![VKeyWitness {
                vkey: vec![0xAA; 32],
                signature: vec![0xBB; 64],
            }],
            native_scripts: vec![NativeScript::ScriptPubkey(Hash32::ZERO)],
            plutus_v1_scripts: vec![vec![0x01, 0x02]],
            plutus_v2_scripts: vec![vec![0x03, 0x04]],
            plutus_v3_scripts: vec![vec![0x05, 0x06]],
            ..empty_witness_set()
        };

        for era in [Era::Conway, Era::Dijkstra] {
            let enc = encode_witness_set_for_era(&ws, era);
            for key in [0u8, 1, 3, 6, 7] {
                let pos = find_key_value_start(&enc, key);
                assert_eq!(
                    &enc[pos..pos + 3],
                    &[0xd9, 0x01, 0x02],
                    "{era:?} witness key {key} must carry tag 258"
                );
            }
        }

        for era in [
            Era::Shelley,
            Era::Allegra,
            Era::Mary,
            Era::Alonzo,
            Era::Babbage,
        ] {
            let enc = encode_witness_set_for_era(&ws, era);
            for key in [0u8, 1, 3, 6, 7] {
                let pos = find_key_value_start(&enc, key);
                assert_ne!(
                    &enc[pos..pos + 3],
                    &[0xd9, 0x01, 0x02],
                    "{era:?} predates PV9 and must NOT carry tag 258 on key {key}"
                );
            }
        }
    }

    /// The tag must survive a decode/re-encode cycle rather than being dropped.
    #[test]
    fn tagged_witness_set_round_trips() {
        let ws = TransactionWitnessSet {
            vkey_witnesses: vec![VKeyWitness {
                vkey: vec![0xAA; 32],
                signature: vec![0xBB; 64],
            }],
            ..empty_witness_set()
        };
        let enc = encode_witness_set_for_era(&ws, Era::Conway);
        let decoded = crate::decode::era_conway::decode_conway_witness_set(
            &mut crate::decode::reader::Reader::new(&enc),
            Era::Conway,
        )
        .expect("tagged witness set must decode");
        assert_eq!(decoded.vkey_witnesses.len(), 1);
        assert_eq!(
            encode_witness_set_for_era(&decoded, Era::Conway),
            enc,
            "re-encode must be byte-identical"
        );
    }

    // ── #936: Dijkstra sub_transactions is an OMap == a bare ARRAY of values ──

    fn sub_tx(marker: u64) -> dugite_primitives::transaction::SubTransaction {
        dugite_primitives::transaction::SubTransaction {
            outputs: vec![TransactionOutput {
                address: test_address(),
                value: ada(1_000_000 + marker),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            }],
            ..Default::default()
        }
    }

    /// Haskell `EncCBOR (OMap k v) = encodeStrictSeq encCBOR (toStrictSeq omap)`
    /// — the keys are NEVER on the wire. Key 23's value must therefore be an
    /// array, and each id must be reconstructed as the hash of its body bytes.
    #[test]
    fn sub_transactions_encode_as_bare_value_array_and_round_trip() {
        let mut body = minimal_body();
        body.sub_transactions = vec![sub_tx(1), sub_tx(2), sub_tx(3)];

        let enc = encode_transaction_body_for_era(&body, Era::Dijkstra);
        let pos = find_key_value_start(&enc, 23);
        assert_eq!(
            enc[pos] & 0xe0,
            0x80,
            "sub_transactions must be a CBOR array (major type 4), not a map"
        );
        assert_eq!(enc[pos], 0x83, "3 sub-txs => definite array of 3");

        let decoded = crate::decode::era_conway::decode_conway_tx_body(
            &mut crate::decode::reader::Reader::new(&enc),
            Era::Dijkstra,
        )
        .expect("Dijkstra body must decode");
        assert_eq!(decoded.sub_transactions.len(), 3);

        // Keys are reconstructed via `toOKey` == blake2b_256 of the body bytes.
        for sub in &decoded.sub_transactions {
            let raw = sub.raw_body_cbor.as_ref().expect("raw body preserved");
            assert_eq!(
                sub.tx_id,
                dugite_primitives::hash::blake2b_256(raw),
                "sub-tx id must be the hash of its own body bytes"
            );
        }
        // Distinct bodies => distinct ids.
        assert_ne!(
            decoded.sub_transactions[0].tx_id,
            decoded.sub_transactions[1].tx_id
        );
    }

    /// `decodeOMap` uses `decodeListLikeEnforceNoDuplicates`: a repeated
    /// reconstructed key is a hard decode failure, not a last-wins merge.
    #[test]
    fn sub_transactions_reject_duplicates() {
        let mut body = minimal_body();
        // The SAME sub-tx twice => identical body bytes => identical id.
        body.sub_transactions = vec![sub_tx(9), sub_tx(9)];

        let enc = encode_transaction_body_for_era(&body, Era::Dijkstra);
        let result = crate::decode::era_conway::decode_conway_tx_body(
            &mut crate::decode::reader::Reader::new(&enc),
            Era::Dijkstra,
        );
        let err = format!(
            "{:?}",
            result.expect_err("duplicate sub-tx must be rejected")
        );
        assert!(
            err.contains("duplicate sub-transaction id"),
            "expected the OMap duplicate error, got: {err}"
        );
    }

    /// The OMap array obeys the same `variableListLenEncoding` threshold as
    /// every other collection (#938), and both header forms decode.
    #[test]
    fn sub_transactions_cross_the_23_24_threshold() {
        for (n, indefinite) in [(23usize, false), (24, true)] {
            let mut body = minimal_body();
            body.sub_transactions = (0..n).map(|i| sub_tx(i as u64)).collect();

            let enc = encode_transaction_body_for_era(&body, Era::Dijkstra);
            let pos = find_key_value_start(&enc, 23);
            if indefinite {
                assert_eq!(enc[pos], 0x9f, "sub_transactions indefinite at n={n}");
            } else {
                assert_eq!(enc[pos], 0x80 | 23, "sub_transactions definite at n={n}");
            }
            let decoded = crate::decode::era_conway::decode_conway_tx_body(
                &mut crate::decode::reader::Reader::new(&enc),
                Era::Dijkstra,
            )
            .expect("body must decode");
            assert_eq!(decoded.sub_transactions.len(), n, "count at n={n}");
        }
    }
}
