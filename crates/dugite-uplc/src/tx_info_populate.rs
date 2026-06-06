//! Translation helpers: `dugite_primitives` → `crate::script_context`.
//!
//! Phase-2 evaluation needs to lift a decoded `dugite_primitives::Transaction`
//! into the per-version `TxInfoV1/V2/V3` shape Plutus validators observe.
//! Each field has a small structural translation:
//!
//! | dugite-primitives           | dugite-uplc::script_context        |
//! |-----------------------------|------------------------------------|
//! | `Address`                   | [`script_context::Address`]        |
//! | `Value` / mint              | [`PlutusValue`]                    |
//! | `Withdrawal`                | `(StakingCredential, BigInt)`      |
//! | `(SlotNo, SlotNo)` interval | [`PosixTimeRange`]                 |
//! | `TransactionInput`          | [`TxOutRef`]                       |
//! | `TransactionOutput`         | [`TxOut`]                          |
//!
//! This module currently lands the **field-level** translation helpers
//! plus their unit tests. The per-version `TxInfo` builders that
//! consume these helpers land in UPLC-9 part 3b.

use crate::data::Data;
use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::script_context::{
    Address as PlAddress, AssetEntry, Credential as PlCredential, OutputDatum as PlOutputDatum,
    PlutusValue, PosixTimeRange, PubKeyHash, ScriptHash, StakingCredential, TxId, TxInInfo, TxOut,
    TxOutRef,
};
use dugite_primitives::address::Address as PrimAddress;
use dugite_primitives::credentials::{Credential as PrimCred, Pointer as PrimPointer};
use dugite_primitives::hash::blake2b_224_tagged;
use dugite_primitives::transaction::{
    OutputDatum as PrimOutputDatum, PlutusData as PrimPlutusData, ScriptRef as PrimScriptRef,
    TransactionInput as PrimTxIn, TransactionOutput as PrimTxOut, Withdrawal as PrimWithdrawal,
};
use dugite_primitives::value::Value as PrimValue;
use num_bigint::BigInt;
use std::collections::BTreeMap;

/// Plutus / native script hash domain tags (matches the Haskell reference
/// in `Cardano.Ledger.Hashes`):
///
/// | tag | language        |
/// |-----|-----------------|
/// |  0  | NativeScript    |
/// |  1  | PlutusV1        |
/// |  2  | PlutusV2        |
/// |  3  | PlutusV3        |
///
/// The hash domain is always `blake2b_224(tag || preimage)`.
/// For Plutus scripts, `preimage` is the **raw script bytes** (the
/// inner content of the CBOR bstr, NOT CBOR-encoded). For native
/// scripts, `preimage` is the CBOR-encoded native script.
const SCRIPT_TAG_NATIVE: u8 = 0;
const SCRIPT_TAG_PLUTUS_V1: u8 = 1;
const SCRIPT_TAG_PLUTUS_V2: u8 = 2;
const SCRIPT_TAG_PLUTUS_V3: u8 = 3;
/// PlutusV4 hash-prefix byte (Dijkstra, issue #475 Phase 5).
const SCRIPT_TAG_PLUTUS_V4: u8 = 4;

/// Translate a dugite primitive [`PrimCred`] into the Plutus
/// `Credential` shape Plutus scripts observe.
///
/// The two cases map straight across: `VerificationKey(h) → PubKey(h)`,
/// `Script(h) → Script(h)`. Both wrap a 28-byte hash so the byte copy
/// is exact.
pub fn credential_to_plutus(cred: &PrimCred) -> PlCredential {
    match cred {
        PrimCred::VerificationKey(h) => PlCredential::PubKey(hash28_to_array(h)),
        PrimCred::Script(h) => PlCredential::Script(hash28_to_array(h)),
    }
}

/// Translate a dugite primitive [`PrimAddress`] into the Plutus
/// `Address` shape.
///
/// Returns an error for Byron addresses: Plutus validators cannot
/// observe Byron addresses because they do not carry payment/stake
/// credentials in the post-Shelley sense. cardano-node treats Byron
/// outputs as un-spendable by Plutus scripts; mirroring that here
/// avoids producing a Plutus address that lies about the underlying
/// credentials.
pub fn address_to_plutus(addr: &PrimAddress) -> Result<PlAddress, PhaseTwoError> {
    match addr {
        PrimAddress::Base(base) => Ok(PlAddress {
            payment: credential_to_plutus(&base.payment),
            staking: Some(StakingCredential::Hash(credential_to_plutus(&base.stake))),
        }),
        PrimAddress::Enterprise(ent) => Ok(PlAddress {
            payment: credential_to_plutus(&ent.payment),
            staking: None,
        }),
        PrimAddress::Pointer(ptr) => Ok(PlAddress {
            payment: credential_to_plutus(&ptr.payment),
            staking: Some(pointer_to_plutus(&ptr.pointer)),
        }),
        // Reward addresses don't appear as tx outputs and can't host a
        // Plutus script. If we somehow see one in this translation
        // surface a typed error rather than synthesising a fake
        // payment credential.
        PrimAddress::Reward(_) => Err(PhaseTwoError::Internal(
            "address_to_plutus: reward address cannot host a tx output".to_string(),
        )),
        PrimAddress::Byron(_) => Err(PhaseTwoError::Internal(
            "address_to_plutus: Byron address cannot be observed by Plutus".to_string(),
        )),
    }
}

/// Translate a primitive pointer (slot, tx_index, cert_index) into the
/// Plutus `StakingCredential::Pointer` shape.
fn pointer_to_plutus(p: &PrimPointer) -> StakingCredential {
    StakingCredential::Pointer {
        slot: p.slot,
        tx: p.tx_index,
        cert: p.cert_index,
    }
}

/// Translate a primitive `Value` (coin + multi-asset BTreeMap) into a
/// `PlutusValue`. The ADA entry is emitted under the canonical empty
/// policy + empty asset-name, matching `PlutusV3.V1.Value`'s
/// `singleton "" "" lovelace` convention.
///
/// Policies and asset names are emitted in BTreeMap iteration order,
/// which is lexicographic by byte string — identical to the canonical
/// CBOR order Plutus validators expect.
pub fn value_to_plutus(value: &PrimValue) -> PlutusValue {
    let mut policies: Vec<(ScriptHash, Vec<AssetEntry>)> = Vec::new();
    let lovelace = BigInt::from(value.coin.0);
    // ADA policy = `0x00..00` (28 zero bytes); asset name = empty.
    policies.push(([0u8; 28], vec![(Vec::new(), lovelace)]));
    for (policy_id, assets) in &value.multi_asset {
        let mut asset_entries: Vec<AssetEntry> = Vec::with_capacity(assets.len());
        for (asset_name, amount) in assets {
            asset_entries.push((asset_name.0.clone(), BigInt::from(*amount)));
        }
        policies.push((hash28_to_array(policy_id), asset_entries));
    }
    PlutusValue { policies }
}

/// Translate a mint map (i64 amounts; can be negative for burns) into
/// a `PlutusValue`. Unlike [`value_to_plutus`], **no ADA entry is
/// emitted** — minting/burning ADA is impossible.
pub fn mint_to_plutus(
    mint: &BTreeMap<
        dugite_primitives::hash::Hash28,
        BTreeMap<dugite_primitives::value::AssetName, i64>,
    >,
) -> PlutusValue {
    let mut policies: Vec<(ScriptHash, Vec<AssetEntry>)> = Vec::with_capacity(mint.len());
    for (policy_id, assets) in mint {
        let mut asset_entries: Vec<AssetEntry> = Vec::with_capacity(assets.len());
        for (asset_name, amount) in assets {
            asset_entries.push((asset_name.0.clone(), BigInt::from(*amount)));
        }
        policies.push((hash28_to_array(policy_id), asset_entries));
    }
    PlutusValue { policies }
}

/// Convert a [`PrimWithdrawal`] into the Plutus `(StakingCredential, BigInt)`
/// shape that V1/V2 TxInfo expose in their `wdrl` field.
///
/// The withdrawal's `reward_account` is a 29-byte reward-address blob:
/// `[ header_byte; key_or_script_hash(28) ]`. We parse it via
/// [`PrimAddress::from_bytes`] and unwrap the stake credential. Plutus
/// validators only observe the credential, not the network byte.
pub fn withdrawal_to_plutus(
    w: &PrimWithdrawal,
) -> Result<(StakingCredential, BigInt), PhaseTwoError> {
    let addr = PrimAddress::from_bytes(&w.reward_account).map_err(|e| {
        PhaseTwoError::Internal(format!("withdrawal_to_plutus: reward_account: {e}"))
    })?;
    let stake = match addr {
        PrimAddress::Reward(r) => r.stake,
        other => {
            return Err(PhaseTwoError::Internal(format!(
                "withdrawal_to_plutus: expected Reward address, got {other:?}"
            )));
        }
    };
    let cred = credential_to_plutus(&stake);
    Ok((StakingCredential::Hash(cred), BigInt::from(w.amount.0)))
}

/// Byron-era slot length in milliseconds. cardano-ledger converts a slot to
/// POSIXTime via the full multi-era `EpochInfo` (`slotToPOSIXTime` →
/// `epochInfoSlotToUTCTime`), which is piecewise: each slot is resolved in the
/// era whose `[boundSlot, eraEnd)` contains it, using THAT era's slot length.
/// Byron's `eraSlotLength` is `genesisSlotLength` (Byron `ProtocolParameters.
/// slotDuration`), which is 20_000 ms on every Cardano network that has a Byron
/// era (mainnet + preprod); Byron-less networks (preview, Conway-genesis
/// devnets) have `slot_zero_offset == 0`, so no slot ever falls below the pivot
/// and this constant is unused there.
const BYRON_SLOT_LENGTH_MS: i128 = 20_000;

/// Convert a slot number to POSIX milliseconds using the supplied
/// [`SlotConfig`], matching cardano-ledger's `slotToPOSIXTime` byte-exact
/// (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/TxInfo.hs`).
///
/// Plutus' `TxInfo.valid_range` is expressed in POSIX milliseconds since the
/// Unix epoch. The conversion is PIECEWISE across eras, but for any Cardano
/// network both the Byron line and the Shelley+ line meet exactly at the pivot
/// `(slot_zero_offset, network_start)` — Shelley starts at the slot/time Byron
/// ends — so a single signed expression covers both regimes:
///
/// ```text
/// posix_ms = network_start_unix_seconds * 1000
///          + (slot - slot_zero_offset) * slot_length_for_era(slot)
/// ```
///
/// where `slot_length_for_era` is the Shelley slot length (`slot_length_ms`)
/// at/after the pivot and the Byron slot length ([`BYRON_SLOT_LENGTH_MS`])
/// before it. For a Byron slot `(slot - slot_zero_offset)` is negative, so the
/// product walks backward from `network_start` at the Byron rate — exactly what
/// the Haskell multi-era `EpochInfo` produces (e.g. preprod slot 1000 →
/// `1655769600000 - 85400*20000 = 1654061600000`). The previous single-era form
/// rejected `slot < slot_zero_offset`, which incorrectly failed every tx whose
/// `validity_interval_start`/`ttl` references a pre-Shelley slot.
pub fn slot_to_posix_ms(slot: u64, sc: &SlotConfig) -> Result<i64, PhaseTwoError> {
    // Mirror Haskell `Ouroboros.Consensus.HardFork.History.Qry.guardEnd`:
    // a slot `s` is past the horizon iff `s >= boundSlot eraEnd`. When
    // `safe_zone_horizon_slot` is `None`, the era is treated as unbounded
    // (mirrors `EraEnd EraUnbounded` / `UnsafeIndefiniteSafeZone`).
    if let Some(horizon) = sc.safe_zone_horizon_slot {
        if slot >= horizon {
            return Err(PhaseTwoError::TimeTranslationPastHorizon { slot, horizon });
        }
    }
    // Piecewise slot length: Byron rate below the Shelley pivot, Shelley rate
    // at/after it. `slot_zero_offset` IS the Shelley start slot; everything
    // before it is the Byron era (uniform 20s slots).
    let slot_len_ms: i128 = if slot >= sc.slot_zero_offset {
        sc.slot_length_ms as i128
    } else {
        BYRON_SLOT_LENGTH_MS
    };
    let rel = (slot as i128) - (sc.slot_zero_offset as i128);
    let delta_ms = rel * slot_len_ms;
    let start_ms = (sc.network_start_unix_seconds as i128) * 1_000;
    let total = start_ms
        .checked_add(delta_ms)
        .ok_or_else(|| PhaseTwoError::Internal("slot_to_posix_ms: i128 overflow".to_string()))?;
    i64::try_from(total).map_err(|_| {
        PhaseTwoError::Internal(format!("slot_to_posix_ms: result {total} overflows i64"))
    })
}

/// Translate a slot-based `(validity_start, ttl)` tuple into the Plutus
/// [`PosixTimeRange`].
///
/// `validity_start = None` leaves the lower bound open (`-∞`).
/// `ttl = None` leaves the upper bound open (`+∞`). Both bounds, when
/// present, are converted via [`slot_to_posix_ms`].
pub fn valid_range_to_posix(
    validity_start: Option<u64>,
    ttl: Option<u64>,
    sc: &SlotConfig,
) -> Result<PosixTimeRange, PhaseTwoError> {
    let lower = validity_start
        .map(|s| slot_to_posix_ms(s, sc))
        .transpose()?;
    let upper = ttl.map(|s| slot_to_posix_ms(s, sc)).transpose()?;
    Ok(PosixTimeRange { lower, upper })
}

/// Translate a primitive `TransactionInput` into the Plutus `TxOutRef`.
pub fn input_to_outref(input: &dugite_primitives::transaction::TransactionInput) -> TxOutRef {
    TxOutRef {
        tx_id: tx_hash_to_array(&input.transaction_id),
        idx: input.index as u64,
    }
}

/// Translate a primitive `TransactionId` (`Hash32`) into the Plutus
/// `TxId` byte array.
pub fn tx_hash_to_array(h: &dugite_primitives::hash::Hash<32>) -> TxId {
    h.0
}

/// Translate a 28-byte primitive hash into the Plutus 28-byte array.
fn hash28_to_array(h: &dugite_primitives::hash::Hash<28>) -> [u8; 28] {
    h.0
}

/// Translate a list of required-signer key hashes into the Plutus
/// `signatories` field of `TxInfo`.
pub fn required_signers_to_plutus(signers: &[dugite_primitives::hash::Hash28]) -> Vec<PubKeyHash> {
    signers.iter().map(hash28_to_array).collect()
}

/// Recursively translate a primitive [`PrimPlutusData`] value into the
/// crate-local [`Data`]. The two enums are structurally identical
/// (`Constr` / `Map` / `List` / `Integer→I` / `Bytes→B`); this is a
/// pure structural rewrite with no semantic interpretation.
///
/// Recursion is bounded only by the input depth. Adversarial inputs
/// reach this path via tx witness sets (datums) and tx outputs (inline
/// datums); the upstream CBOR decoder has already enforced its own
/// depth cap before producing the typed `PrimPlutusData`, so we don't
/// re-cap here.
pub fn plutus_data_to_data(p: &PrimPlutusData) -> Data {
    match p {
        PrimPlutusData::Constr(tag, fields) => {
            Data::Constr(*tag, fields.iter().map(plutus_data_to_data).collect())
        }
        PrimPlutusData::Map(entries) => Data::Map(
            entries
                .iter()
                .map(|(k, v)| (plutus_data_to_data(k), plutus_data_to_data(v)))
                .collect(),
        ),
        PrimPlutusData::List(items) => Data::List(items.iter().map(plutus_data_to_data).collect()),
        PrimPlutusData::Integer(n) => Data::I(n.clone()),
        PrimPlutusData::Bytes(b) => Data::B(b.clone()),
    }
}

/// Translate a primitive [`PrimOutputDatum`] into the Plutus
/// [`script_context::OutputDatum`] shape.
///
/// `DatumHash(h)` is byte-copied. `InlineDatum { data, .. }` recursively
/// translates `data` via [`plutus_data_to_data`]; the `raw_cbor` side
/// channel is dropped at this layer — Plutus validators only observe
/// the structural `Data` value, not its on-the-wire byte form.
pub fn output_datum_to_plutus(d: &PrimOutputDatum) -> PlOutputDatum {
    match d {
        PrimOutputDatum::None => PlOutputDatum::None,
        PrimOutputDatum::DatumHash(h) => PlOutputDatum::Hash(h.0),
        PrimOutputDatum::InlineDatum { data, .. } => {
            PlOutputDatum::Inline(plutus_data_to_data(data))
        }
    }
}

/// Compute the 28-byte hash of a [`PrimScriptRef`] using the Cardano
/// `blake2b_224(tag || preimage)` script-hash domain.
///
/// | variant         | tag  | preimage                       |
/// |-----------------|------|--------------------------------|
/// | `NativeScript`  | 0x00 | canonical CBOR of the script    |
/// | `PlutusV1(bs)`  | 0x01 | `bs` verbatim                   |
/// | `PlutusV2(bs)`  | 0x02 | `bs` verbatim                   |
/// | `PlutusV3(bs)`  | 0x03 | `bs` verbatim                   |
///
/// The native-script CBOR is produced by
/// [`dugite_serialization::encode::encode_native_script`], which emits
/// the canonical post-Mary encoding. Hash byte-for-byte equality with
/// cardano-node is verified by the existing
/// `dugite-cli::transaction::Policyid` path which uses the same
/// helpers.
pub fn script_ref_hash(s: &PrimScriptRef) -> ScriptHash {
    match s {
        PrimScriptRef::NativeScript(ns) => {
            let cbor = dugite_serialization::encode::encode_native_script(ns);
            blake2b_224_tagged(SCRIPT_TAG_NATIVE, &cbor).0
        }
        PrimScriptRef::PlutusV1(bytes) => blake2b_224_tagged(SCRIPT_TAG_PLUTUS_V1, bytes).0,
        PrimScriptRef::PlutusV2(bytes) => blake2b_224_tagged(SCRIPT_TAG_PLUTUS_V2, bytes).0,
        PrimScriptRef::PlutusV3(bytes) => blake2b_224_tagged(SCRIPT_TAG_PLUTUS_V3, bytes).0,
        // PlutusV4 (Dijkstra, language tag 4, hash prefix `\x04`).
        //
        // Initial Dijkstra rollout: same wire shape and hash discipline as V3
        // (prefix-tagged blake2b-224 over the flat program bytes), only the
        // language tag differs. Full TxInfo translation for V4 ships as a
        // follow-on under issue #475 Phase 5 (decoder now emits V4 from the
        // Conway script_ref reader; runtime evaluation still gated).
        PrimScriptRef::PlutusV4(bytes) => blake2b_224_tagged(SCRIPT_TAG_PLUTUS_V4, bytes).0,
    }
}

/// Translate a primitive [`PrimTxOut`] into the Plutus [`TxOut`].
///
/// `reference_script` is populated from `out.script_ref` via
/// [`script_ref_hash`] when present, matching what Plutus validators
/// observe via `TxOut.referenceScript` in V2+ contexts.
pub fn output_to_plutus(out: &PrimTxOut) -> Result<TxOut, PhaseTwoError> {
    Ok(TxOut {
        address: address_to_plutus(&out.address)?,
        value: value_to_plutus(&out.value),
        datum: output_datum_to_plutus(&out.datum),
        reference_script: out.script_ref.as_ref().map(script_ref_hash),
    })
}

/// Resolve a primitive [`PrimTxIn`] against a list of already-decoded
/// resolved-UTxO entries (the `(TransactionInput, TransactionOutput,
/// raw_cbor)` triples produced by
/// [`crate::phase_two::decode_phase_two_inputs`]).
///
/// Returns [`PhaseTwoError::UtxoDecode`] if `input` does not appear in
/// `resolved` — meaning the ledger handed the evaluator a tx whose
/// inputs reference UTxOs it did not also supply, which always
/// indicates a caller-side bug rather than adversarial input.
pub fn input_to_txininfo(
    input: &PrimTxIn,
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<TxInInfo, PhaseTwoError> {
    for (resolved_in, resolved_out, _raw) in resolved {
        if resolved_in.transaction_id == input.transaction_id && resolved_in.index == input.index {
            return Ok(TxInInfo {
                out_ref: input_to_outref(input),
                resolved: output_to_plutus(resolved_out)?,
            });
        }
    }
    Err(PhaseTwoError::UtxoDecode(format!(
        "input_to_txininfo: tx input {tx}@{idx} not in resolved-utxo map",
        tx = hex::encode(input.transaction_id.0),
        idx = input.index,
    )))
}

/// Sort a slice of inputs into the canonical `Set TxIn` order that
/// cardano-ledger uses for `inputsTxBodyL`, `refInputsTxBodyL`, etc.
///
/// Haskell's `Ord TxIn` compares `TxId` (the raw 32 hash bytes,
/// big-endian memcmp) then `TxIx` (Word16, numeric). Rust's derived
/// `Ord` on [`PrimTxIn`] does exactly this: `transaction_id: Hash<32>`
/// is compared via `[u8; 32]`'s lexicographic byte order, then
/// `index: u32` numerically. Using `.sort()` on a Vec<PrimTxIn>
/// therefore produces byte-exact alignment with
/// `Set.lookupIndex`/`Set.elemAt` in the Haskell ledger.
///
/// This sort must be applied before any redeemer-index lookup
/// (`alonzoRedeemerPointer` / `conwayRedeemerPointer`) and before
/// building `txInfoInputs` / `txInfoReferenceInputs` in the Plutus
/// `ScriptContext`.
pub fn sort_inputs(inputs: &[PrimTxIn]) -> Vec<PrimTxIn> {
    let mut sorted = inputs.to_vec();
    sorted.sort();
    sorted
}

/// Resolve a slice of inputs **in the order given** into a
/// `Vec<TxInInfo>`.
///
/// **Call site responsibility:** callers that construct `txInfoInputs`
/// or `txInfoReferenceInputs` (i.e., all three `populate_tx_info_vN`
/// functions) must pass a sorted slice produced by [`sort_inputs`].
/// The sorted order is the canonical `Set TxIn` order that Haskell's
/// `inputsTxBodyL` presents, and is required for Plutus `Spending`
/// redeemer index resolution to be byte-exact with cardano-ledger's
/// `alonzoRedeemerPointer` / `conwayRedeemerPointer`.
pub fn inputs_to_txininfos(
    inputs: &[PrimTxIn],
    resolved: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Result<Vec<TxInInfo>, PhaseTwoError> {
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        out.push(input_to_txininfo(input, resolved)?);
    }
    Ok(out)
}

/// Translate a 32-byte padded required-signer hash back to its
/// 28-byte Plutus `PubKeyHash`.
///
/// `dugite_primitives::TransactionBody::required_signers` stores each
/// 28-byte addr_keyhash padded to 32 bytes via
/// `Hash28::to_hash32_padded()` (the trailing 4 bytes are zero). The
/// padding is purely an internal representation choice; the on-chain
/// value is the 28-byte prefix. Plutus validators observe the 28-byte
/// `PubKeyHash` directly, so we unpad here.
fn padded_signer_to_pubkeyhash(h: &dugite_primitives::hash::Hash<32>) -> PubKeyHash {
    let mut out = [0u8; 28];
    out.copy_from_slice(&h.0[..28]);
    out
}

/// Translate the full `required_signers` field (`Vec<Hash<32>>` —
/// each entry is a 28-byte addr_keyhash padded to 32 bytes) into the
/// Plutus `signatories` list (`Vec<PubKeyHash>`).
pub fn required_signers_to_plutus_padded(
    signers: &[dugite_primitives::hash::Hash<32>],
) -> Vec<PubKeyHash> {
    signers.iter().map(padded_signer_to_pubkeyhash).collect()
}

/// Translate the resolved-UTxO triples that
/// [`crate::phase_two::decode_phase_two_inputs`] produces back into
/// the `(PrimTxIn, PrimTxOut, Vec<u8>)` shape this module's helpers
/// expect. The translation is a no-op `clone` — kept as a named
/// helper so call sites read clearly.
pub fn resolved_utxos(
    triples: &[(PrimTxIn, PrimTxOut, Vec<u8>)],
) -> Vec<(PrimTxIn, PrimTxOut, Vec<u8>)> {
    triples.to_vec()
}

/// Compute the blake2b_256 hash of the canonical CBOR encoding of
/// `data`. This is the same datum-hash domain used by the ledger to
/// match `OutputDatum::DatumHash(h)` references back to inline-datum
/// payloads in the tx witness set.
fn datum_hash(data: &Data) -> Result<[u8; 32], PhaseTwoError> {
    let cbor = data
        .to_cbor()
        .map_err(|e| PhaseTwoError::Internal(format!("datum_hash: data.to_cbor: {e}")))?;
    Ok(dugite_primitives::hash::blake2b_256(&cbor).0)
}

/// Extract the witness-set's `plutus_data` list into the
/// `Vec<([u8; 32], Data)>` shape TxInfoV3/V2 expose. Each entry is
/// `(datum_hash(d), d)`.
///
/// Datum hashes **must** be computed over each witness datum's *original*
/// CBOR bytes, never a re-encoding. Haskell memoises the raw bytes
/// (`MemoBytes`/`Data`) and hashes those. On-chain datums are frequently
/// non-canonical (general `Constr` tag-102 form for small indices,
/// definite-length field arrays, non-minimal integers, …) that a structural
/// re-encode cannot reproduce — so a re-hash diverges from the on-chain datum
/// hash, and any script that does `findDatum`/`findDatumHash` over `txInfoData`
/// silently fails to resolve the entry (an `error` with no trace). We therefore
/// hash the preserved per-element raw spans, mirroring the Phase-1 datum-witness
/// check in `dugite-ledger`; we fall back to a canonical re-encode only when no
/// original bytes are available (e.g. datums the node constructs itself).
///
/// Per CLAUDE.md / `lib.rs` §1, this never panics on malformed Data —
/// the upstream CBOR decoder produced typed `PlutusData` values, so
/// the only failure mode is the in-house `Data::to_cbor` encoder
/// itself, which we surface as `PhaseTwoError::Internal`.
pub fn datums_to_plutus(
    plutus_data: &[PrimPlutusData],
    raw_plutus_data_cbor: Option<&[u8]>,
) -> Result<Vec<([u8; 32], Data)>, PhaseTwoError> {
    let spans = raw_plutus_data_cbor
        .and_then(dugite_serialization::plutus_data_element_spans)
        .filter(|spans| spans.len() == plutus_data.len());
    let mut out: Vec<([u8; 32], Data)> = Vec::with_capacity(plutus_data.len());
    for (i, d) in plutus_data.iter().enumerate() {
        let translated = plutus_data_to_data(d);
        let h = match &spans {
            Some(spans) => dugite_primitives::hash::blake2b_256(&spans[i]).0,
            None => datum_hash(&translated)?,
        };
        out.push((h, translated));
    }
    // `txInfoData` is built from the witness `TxDats = Map DataHash (Data era)`
    // in cardano-ledger, so the datum entries are ordered ascending by datum
    // hash (lexicographic over the 32 raw hash bytes — `Ord DataHash`), NOT in
    // witness-set wire order. A script that folds/looks-up the datum map pays a
    // different ExUnit cost when the order differs, so emit it sorted to match
    // cardano-node byte-exact. Cf. cardano-ledger `transTxInfoData` over
    // `Map.toList (unTxDats …)`. (V1 AssocList and V2 AssocMap both consume
    // this ordering.)
    out.sort_by_key(|(hash, _)| *hash);
    Ok(out)
}

/// Translate the V1/V2 `withdrawals` map into the
/// `Vec<(StakingCredential, BigInt)>` shape Plutus TxInfo exposes.
/// Iteration order matches the BTreeMap iteration order (lex by
/// reward_account bytes), which is the canonical wire order.
pub fn withdrawals_to_plutus(
    wdrl: &BTreeMap<Vec<u8>, dugite_primitives::value::Lovelace>,
) -> Result<Vec<(StakingCredential, BigInt)>, PhaseTwoError> {
    let mut out = Vec::with_capacity(wdrl.len());
    for (reward_account, amount) in wdrl {
        // Reuse withdrawal_to_plutus by reconstructing a Withdrawal here so we
        // share the exact reward-address parsing/validation path.
        let w = PrimWithdrawal {
            reward_account: reward_account.clone(),
            amount: *amount,
        };
        out.push(withdrawal_to_plutus(&w)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{
        BaseAddress, EnterpriseAddress, PointerAddress, RewardAddress,
    };
    use dugite_primitives::credentials::Pointer as PrimPointer;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::transaction::{PlutusData as PrimPD, TransactionInput, Withdrawal};
    use dugite_primitives::value::{AssetName, Lovelace};

    /// `txInfoData` keys MUST be computed over each witness datum's *original*
    /// CBOR bytes, never a re-encoding — otherwise a script that resolves a
    /// datum by hash (`findDatum`) over `txInfoData` silently fails (an `error`
    /// term with no trace). On-chain datums are routinely encoded in the
    /// general `Constr` tag-102 form for small constructor indices, which a
    /// canonical re-encode (compact tag-121+) cannot reproduce.
    ///
    /// Regression for the mainnet Alonzo no-trace "script returned Error term"
    /// divergence: `Constr 1 []` stored as tag-102 (`d8 66 82 01 80`) must hash
    /// to `blake2b256(d8668201_80)`, NOT `blake2b256(d87a80)` (the compact form).
    #[test]
    fn txinfo_data_hashed_over_original_tag102_bytes_not_reencode() {
        use dugite_primitives::hash::blake2b_256;
        // Witness plutus_data array = array(1) of one tag-102 `Constr 1 []`.
        let tag102: [u8; 5] = [0xd8, 0x66, 0x82, 0x01, 0x80];
        let mut raw = vec![0x81u8]; // array(1)
        raw.extend_from_slice(&tag102);

        // Typed value the decoder produces from the tag-102 form.
        let datum = PrimPD::Constr(1, vec![]);

        let out = datums_to_plutus(std::slice::from_ref(&datum), Some(&raw)).unwrap();
        assert_eq!(out.len(), 1);

        let original_hash = blake2b_256(&tag102).0;
        let reencode_hash = blake2b_256(&[0xd8, 0x7a, 0x80]).0; // compact Constr 1 []
        assert_ne!(
            original_hash, reencode_hash,
            "test premise: the two encodings must hash differently"
        );
        assert_eq!(
            out[0].0, original_hash,
            "txInfoData must key on the original tag-102 bytes"
        );
        assert_ne!(
            out[0].0, reencode_hash,
            "txInfoData must NOT re-encode the datum before hashing"
        );

        // With no original bytes available, fall back to the canonical re-encode.
        let fb = datums_to_plutus(std::slice::from_ref(&datum), None).unwrap();
        assert_eq!(
            fb[0].0, reencode_hash,
            "fallback path hashes the canonical re-encode"
        );
    }

    /// Encode a reward address as the 29-byte blob `Withdrawal.reward_account`
    /// expects: `header_byte | hash28`. Header bit 4 = is-script, bits 0-3 =
    /// network. Bits 5-7 are `0b111` for reward addresses.
    fn encode_reward_addr_blob(mainnet: bool, is_script: bool, hash: [u8; 28]) -> Vec<u8> {
        // Reward addresses use the high nibble `0b1110` (key) or `0b1111`
        // (script), with the low nibble holding the network id.
        let net = if mainnet { 0x01u8 } else { 0x00u8 };
        let header = if is_script { 0xf0 } else { 0xe0 } | net;
        let mut v = Vec::with_capacity(29);
        v.push(header);
        v.extend_from_slice(&hash);
        v
    }

    fn h28(b: u8) -> dugite_primitives::hash::Hash28 {
        Hash::<28>([b; 28])
    }

    fn h32(b: u8) -> dugite_primitives::hash::Hash<32> {
        Hash::<32>([b; 32])
    }

    fn key_cred(b: u8) -> PrimCred {
        PrimCred::VerificationKey(h28(b))
    }

    fn script_cred(b: u8) -> PrimCred {
        PrimCred::Script(h28(b))
    }

    // ────────────────────────────────────────────────────────────
    // credential / address translation
    // ────────────────────────────────────────────────────────────

    #[test]
    fn credential_to_plutus_round_trips_pubkey() {
        let pl = credential_to_plutus(&key_cred(0x11));
        assert!(matches!(pl, PlCredential::PubKey(h) if h == [0x11u8; 28]));
    }

    #[test]
    fn credential_to_plutus_round_trips_script() {
        let pl = credential_to_plutus(&script_cred(0x22));
        assert!(matches!(pl, PlCredential::Script(h) if h == [0x22u8; 28]));
    }

    #[test]
    fn address_to_plutus_base_includes_staking_hash() {
        let addr = PrimAddress::Base(BaseAddress {
            network: NetworkId::Mainnet,
            payment: key_cred(1),
            stake: key_cred(2),
        });
        let pl = address_to_plutus(&addr).unwrap();
        assert!(matches!(pl.payment, PlCredential::PubKey(h) if h == [1u8; 28]));
        assert!(matches!(
            pl.staking,
            Some(StakingCredential::Hash(PlCredential::PubKey(h))) if h == [2u8; 28]
        ));
    }

    #[test]
    fn address_to_plutus_enterprise_has_no_staking() {
        let addr = PrimAddress::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: script_cred(3),
        });
        let pl = address_to_plutus(&addr).unwrap();
        assert!(matches!(pl.payment, PlCredential::Script(h) if h == [3u8; 28]));
        assert!(pl.staking.is_none());
    }

    #[test]
    fn address_to_plutus_pointer_carries_triple() {
        let addr = PrimAddress::Pointer(PointerAddress {
            network: NetworkId::Mainnet,
            payment: key_cred(4),
            pointer: PrimPointer {
                slot: 100,
                tx_index: 5,
                cert_index: 1,
            },
        });
        let pl = address_to_plutus(&addr).unwrap();
        assert!(matches!(
            pl.staking,
            Some(StakingCredential::Pointer {
                slot: 100,
                tx: 5,
                cert: 1
            })
        ));
    }

    #[test]
    fn address_to_plutus_rejects_reward_address() {
        let addr = PrimAddress::Reward(RewardAddress {
            network: NetworkId::Mainnet,
            stake: key_cred(9),
        });
        let err = address_to_plutus(&addr).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn address_to_plutus_rejects_byron() {
        let addr = PrimAddress::Byron(dugite_primitives::address::ByronAddress { payload: vec![] });
        let err = address_to_plutus(&addr).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // ────────────────────────────────────────────────────────────
    // value / mint
    // ────────────────────────────────────────────────────────────

    #[test]
    fn value_to_plutus_emits_ada_entry_first() {
        let v = PrimValue::lovelace(1_000_000);
        let pl = value_to_plutus(&v);
        assert_eq!(pl.policies.len(), 1);
        assert_eq!(pl.policies[0].0, [0u8; 28]);
        assert_eq!(pl.policies[0].1.len(), 1);
        assert_eq!(pl.policies[0].1[0].0, Vec::<u8>::new());
        assert_eq!(pl.policies[0].1[0].1, BigInt::from(1_000_000));
    }

    #[test]
    fn value_to_plutus_emits_multi_asset_after_ada() {
        let mut assets = BTreeMap::new();
        let name = AssetName::new(b"TOKEN".to_vec()).unwrap();
        assets.insert(name.clone(), 42u64);
        let mut ma = BTreeMap::new();
        ma.insert(h28(0xaa), assets);
        let v = PrimValue {
            coin: Lovelace(500),
            multi_asset: ma,
        };
        let pl = value_to_plutus(&v);
        assert_eq!(pl.policies.len(), 2);
        // ADA first.
        assert_eq!(pl.policies[0].0, [0u8; 28]);
        // Then the policy.
        assert_eq!(pl.policies[1].0, [0xaa; 28]);
        assert_eq!(pl.policies[1].1[0].0, b"TOKEN".to_vec());
        assert_eq!(pl.policies[1].1[0].1, BigInt::from(42));
    }

    #[test]
    fn mint_to_plutus_omits_ada_entry_and_keeps_signs() {
        let mut assets = BTreeMap::new();
        assets.insert(AssetName::new(b"BURN".to_vec()).unwrap(), -1_000i64);
        assets.insert(AssetName::new(b"MINT".to_vec()).unwrap(), 1_000i64);
        let mut mint = BTreeMap::new();
        mint.insert(h28(0xbb), assets);
        let pl = mint_to_plutus(&mint);
        assert_eq!(pl.policies.len(), 1);
        assert_eq!(pl.policies[0].0, [0xbb; 28]);
        // BTreeMap orders BURN < MINT lexicographically.
        assert_eq!(pl.policies[0].1[0].0, b"BURN".to_vec());
        assert_eq!(pl.policies[0].1[0].1, BigInt::from(-1_000));
        assert_eq!(pl.policies[0].1[1].0, b"MINT".to_vec());
        assert_eq!(pl.policies[0].1[1].1, BigInt::from(1_000));
    }

    // ────────────────────────────────────────────────────────────
    // withdrawals
    // ────────────────────────────────────────────────────────────

    #[test]
    fn withdrawal_to_plutus_unwraps_key_stake_credential() {
        let blob = encode_reward_addr_blob(true, false, [7u8; 28]);
        let w = Withdrawal {
            reward_account: blob,
            amount: Lovelace(123_456),
        };
        let (sc, amt) = withdrawal_to_plutus(&w).unwrap();
        assert!(matches!(
            sc,
            StakingCredential::Hash(PlCredential::PubKey(h)) if h == [7u8; 28]
        ));
        assert_eq!(amt, BigInt::from(123_456));
    }

    #[test]
    fn withdrawal_to_plutus_unwraps_script_stake_credential() {
        let blob = encode_reward_addr_blob(false, true, [0x55u8; 28]);
        let w = Withdrawal {
            reward_account: blob,
            amount: Lovelace(1),
        };
        let (sc, _) = withdrawal_to_plutus(&w).unwrap();
        assert!(matches!(
            sc,
            StakingCredential::Hash(PlCredential::Script(h)) if h == [0x55u8; 28]
        ));
    }

    #[test]
    fn withdrawal_to_plutus_rejects_non_reward_address() {
        // Enterprise address blob (header 0x60) is not a reward address.
        let mut blob = vec![0x60u8];
        blob.extend([1u8; 28]);
        let w = Withdrawal {
            reward_account: blob,
            amount: Lovelace(1),
        };
        let err = withdrawal_to_plutus(&w).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn withdrawal_to_plutus_rejects_malformed_blob() {
        let w = Withdrawal {
            reward_account: vec![],
            amount: Lovelace(1),
        };
        let err = withdrawal_to_plutus(&w).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // ────────────────────────────────────────────────────────────
    // slot ↔ posix time
    // ────────────────────────────────────────────────────────────

    fn preview_slot_config() -> SlotConfig {
        // Matches `dugite_ledger::plutus::SlotConfig::preview()` but
        // in dugite-uplc's field names.
        SlotConfig {
            network_start_unix_seconds: 1_666_656_000,
            slot_zero_offset: 0,
            slot_length_ms: 1_000,
            safe_zone_horizon_slot: None,
        }
    }

    #[test]
    fn slot_to_posix_ms_zero_slot_returns_network_start() {
        let sc = preview_slot_config();
        let ms = slot_to_posix_ms(0, &sc).unwrap();
        assert_eq!(ms, 1_666_656_000_000);
    }

    #[test]
    fn slot_to_posix_ms_advances_one_second_per_slot() {
        let sc = preview_slot_config();
        let ms_0 = slot_to_posix_ms(0, &sc).unwrap();
        let ms_60 = slot_to_posix_ms(60, &sc).unwrap();
        assert_eq!(ms_60 - ms_0, 60_000);
    }

    #[test]
    fn slot_to_posix_ms_byron_slots_use_20s_pivot() {
        // preprod: Shelley starts at absolute slot 86400 / unix 1655769600s;
        // Byron (slots < 86400) runs at 20s. cardano-ledger's multi-era
        // EpochInfo converts a Byron slot with Byron's 20s length — NOT the
        // Shelley 1s — and the previous code wrongly rejected slot<offset.
        let sc = SlotConfig {
            network_start_unix_seconds: 1_655_769_600,
            slot_zero_offset: 86_400,
            slot_length_ms: 1_000,
            safe_zone_horizon_slot: None,
        };
        // Byron slot 1000 → byron_start(1654041600000) + 1000*20000.
        assert_eq!(slot_to_posix_ms(1_000, &sc).unwrap(), 1_654_061_600_000);
        // Byron slot 39329 → 1654041600000 + 39329*20000.
        assert_eq!(slot_to_posix_ms(39_329, &sc).unwrap(), 1_654_828_180_000);
        // The pivot slot maps to exactly network_start (both era-lines meet).
        assert_eq!(slot_to_posix_ms(86_400, &sc).unwrap(), 1_655_769_600_000);
        // Shelley slot 100000 → network_start + (100000-86400)*1000.
        assert_eq!(slot_to_posix_ms(100_000, &sc).unwrap(), 1_655_783_200_000);
    }

    #[test]
    fn valid_range_to_posix_open_on_both_ends() {
        let sc = preview_slot_config();
        let r = valid_range_to_posix(None, None, &sc).unwrap();
        assert_eq!(r.lower, None);
        assert_eq!(r.upper, None);
    }

    #[test]
    fn valid_range_to_posix_translates_bounds() {
        let sc = preview_slot_config();
        let r = valid_range_to_posix(Some(10), Some(20), &sc).unwrap();
        assert_eq!(r.lower, Some(slot_to_posix_ms(10, &sc).unwrap()));
        assert_eq!(r.upper, Some(slot_to_posix_ms(20, &sc).unwrap()));
        assert!(r.upper.unwrap() > r.lower.unwrap());
    }

    // ────────────────────────────────────────────────────────────
    // Safe-zone horizon enforcement (mirrors Haskell PastHorizon)
    // ────────────────────────────────────────────────────────────

    #[test]
    fn slot_to_posix_ms_rejects_at_horizon_boundary() {
        // Mirrors the exact predicate `slot < horizon`: the horizon itself
        // is the exclusive upper bound — `slot == horizon` is past horizon.
        let mut sc = preview_slot_config();
        sc.safe_zone_horizon_slot = Some(800);
        let err = slot_to_posix_ms(800, &sc).unwrap_err();
        match err {
            PhaseTwoError::TimeTranslationPastHorizon { slot, horizon } => {
                assert_eq!(slot, 800);
                assert_eq!(horizon, 800);
            }
            other => panic!("expected TimeTranslationPastHorizon, got {other:?}"),
        }
    }

    #[test]
    fn slot_to_posix_ms_rejects_past_horizon() {
        // The exact regression case from Round 1 evidence:
        // requested SlotNo 865, era end (horizon) at SlotNo 800.
        let mut sc = preview_slot_config();
        sc.safe_zone_horizon_slot = Some(800);
        let err = slot_to_posix_ms(865, &sc).unwrap_err();
        assert!(matches!(
            err,
            PhaseTwoError::TimeTranslationPastHorizon {
                slot: 865,
                horizon: 800,
            }
        ));
    }

    #[test]
    fn slot_to_posix_ms_accepts_slot_below_horizon() {
        let mut sc = preview_slot_config();
        sc.safe_zone_horizon_slot = Some(800);
        // 799 < 800 → translatable.
        assert!(slot_to_posix_ms(799, &sc).is_ok());
        // 0 < 800 → translatable (origin).
        assert!(slot_to_posix_ms(0, &sc).is_ok());
    }

    #[test]
    fn slot_to_posix_ms_unbounded_horizon_accepts_far_future() {
        // `safe_zone_horizon_slot: None` mirrors Haskell `EraUnbounded`
        // / `UnsafeIndefiniteSafeZone`. No upper bound is enforced — but
        // production callers MUST set it; only tests rely on this.
        let sc = preview_slot_config();
        assert!(sc.safe_zone_horizon_slot.is_none());
        // A "far future" slot well past any reasonable horizon but small
        // enough not to trigger the i128→i64 overflow guard in
        // slot_to_posix_ms — proves the horizon check is the gating
        // predicate, not numeric range.
        assert!(slot_to_posix_ms(1_000_000_000, &sc).is_ok());
    }

    #[test]
    fn valid_range_to_posix_propagates_horizon_error() {
        // Phase-2 `transValidityInterval` translates BOTH bounds; the
        // error from the upper bound must propagate unchanged.
        let mut sc = preview_slot_config();
        sc.safe_zone_horizon_slot = Some(800);
        // Lower 100 is fine, upper 865 is past horizon.
        let err = valid_range_to_posix(Some(100), Some(865), &sc).unwrap_err();
        assert!(matches!(
            err,
            PhaseTwoError::TimeTranslationPastHorizon {
                slot: 865,
                horizon: 800,
            }
        ));
        // Symmetric: a past-horizon LOWER bound also errors.
        let err = valid_range_to_posix(Some(900), Some(950), &sc).unwrap_err();
        assert!(matches!(
            err,
            PhaseTwoError::TimeTranslationPastHorizon { .. }
        ));
    }

    // ────────────────────────────────────────────────────────────
    // input / id helpers
    // ────────────────────────────────────────────────────────────

    #[test]
    fn input_to_outref_preserves_hash_and_index() {
        let i = TransactionInput {
            transaction_id: h32(0xcc),
            index: 7,
        };
        let r = input_to_outref(&i);
        assert_eq!(r.tx_id, [0xcc; 32]);
        assert_eq!(r.idx, 7);
    }

    #[test]
    fn required_signers_to_plutus_round_trips_byte_arrays() {
        let signers = vec![h28(1), h28(2), h28(3)];
        let pl = required_signers_to_plutus(&signers);
        assert_eq!(pl, vec![[1u8; 28], [2u8; 28], [3u8; 28]]);
    }

    // ────────────────────────────────────────────────────────────
    // PlutusData translation
    // ────────────────────────────────────────────────────────────

    #[test]
    fn plutus_data_to_data_translates_primitive_leaves() {
        let i = PrimPlutusData::Integer(BigInt::from(-42));
        assert_eq!(plutus_data_to_data(&i), Data::I(BigInt::from(-42)));

        let b = PrimPlutusData::Bytes(b"hello".to_vec());
        assert_eq!(plutus_data_to_data(&b), Data::B(b"hello".to_vec()));
    }

    #[test]
    fn plutus_data_to_data_round_trips_constr_with_nested_fields() {
        let p = PrimPlutusData::Constr(
            3,
            vec![
                PrimPlutusData::Integer(BigInt::from(1)),
                PrimPlutusData::Bytes(vec![0xff, 0xee]),
                PrimPlutusData::List(vec![PrimPlutusData::Integer(BigInt::from(2))]),
            ],
        );
        let d = plutus_data_to_data(&p);
        assert_eq!(
            d,
            Data::Constr(
                3,
                vec![
                    Data::I(BigInt::from(1)),
                    Data::B(vec![0xff, 0xee]),
                    Data::List(vec![Data::I(BigInt::from(2))]),
                ]
            )
        );
    }

    #[test]
    fn plutus_data_to_data_preserves_map_order() {
        let p = PrimPlutusData::Map(vec![
            (
                PrimPlutusData::Integer(BigInt::from(2)),
                PrimPlutusData::Bytes(vec![0x02]),
            ),
            (
                PrimPlutusData::Integer(BigInt::from(1)),
                PrimPlutusData::Bytes(vec![0x01]),
            ),
        ]);
        let d = plutus_data_to_data(&p);
        match d {
            Data::Map(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].0, Data::I(BigInt::from(2))); // input order preserved
                assert_eq!(entries[1].0, Data::I(BigInt::from(1)));
            }
            other => panic!("expected Map, got {other:?}"),
        }
    }

    // ────────────────────────────────────────────────────────────
    // OutputDatum / TransactionOutput translation
    // ────────────────────────────────────────────────────────────

    #[test]
    fn datums_to_plutus_sorts_by_datum_hash_ascending() {
        // cardano-ledger builds txInfoData from the witness `TxDats =
        // Map DataHash (Data era)`, so entries are ordered ascending by the
        // 32-byte datum hash regardless of witness-set wire order. Feed several
        // distinct datums and assert the output is hash-sorted.
        let data: Vec<PrimPlutusData> = (0..6)
            .map(|n| PrimPlutusData::Integer(BigInt::from(n * 7 + 1)))
            .collect();
        let out = datums_to_plutus(&data, None).unwrap();
        assert_eq!(out.len(), 6);
        for w in out.windows(2) {
            assert!(
                w[0].0 <= w[1].0,
                "datums must be sorted ascending by hash: {:?} !<= {:?}",
                w[0].0,
                w[1].0
            );
        }
        // Sanity: the set of hashes equals the per-datum hashes (no loss).
        use std::collections::BTreeSet;
        let got: BTreeSet<[u8; 32]> = out.iter().map(|(h, _)| *h).collect();
        let expect: BTreeSet<[u8; 32]> = data
            .iter()
            .map(|d| datum_hash(&plutus_data_to_data(d)).unwrap())
            .collect();
        assert_eq!(got, expect);
    }

    #[test]
    fn output_datum_to_plutus_covers_all_three_variants() {
        assert_eq!(
            output_datum_to_plutus(&PrimOutputDatum::None),
            PlOutputDatum::None
        );
        let hash = Hash::<32>([0xee; 32]);
        assert_eq!(
            output_datum_to_plutus(&PrimOutputDatum::DatumHash(hash)),
            PlOutputDatum::Hash([0xee; 32])
        );
        let inline = PrimOutputDatum::InlineDatum {
            data: PrimPlutusData::Integer(BigInt::from(7)),
            raw_cbor: None,
        };
        assert_eq!(
            output_datum_to_plutus(&inline),
            PlOutputDatum::Inline(Data::I(BigInt::from(7)))
        );
    }

    fn enterprise_output(lovelace: u64) -> PrimTxOut {
        PrimTxOut {
            address: PrimAddress::Enterprise(EnterpriseAddress {
                network: NetworkId::Testnet,
                payment: key_cred(0x77),
            }),
            value: PrimValue::lovelace(lovelace),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    #[test]
    fn output_to_plutus_translates_address_value_datum() {
        let out = enterprise_output(2_500_000);
        let pl = output_to_plutus(&out).unwrap();
        assert!(matches!(pl.address.payment, PlCredential::PubKey(h) if h == [0x77; 28]));
        assert!(pl.address.staking.is_none());
        assert_eq!(pl.value.policies[0].1[0].1, BigInt::from(2_500_000));
        assert_eq!(pl.datum, PlOutputDatum::None);
        // No script_ref → no reference_script.
        assert!(pl.reference_script.is_none());
    }

    // ────────────────────────────────────────────────────────────
    // script-ref hashing
    // ────────────────────────────────────────────────────────────

    #[test]
    fn script_ref_hash_plutus_v1_v2_v3_use_distinct_tag_bytes() {
        // Same raw script bytes through three Plutus versions must yield
        // three different hashes — proving the tag byte participates.
        let bytes = vec![0xab, 0xcd, 0xef];
        let h1 = script_ref_hash(&PrimScriptRef::PlutusV1(bytes.clone()));
        let h2 = script_ref_hash(&PrimScriptRef::PlutusV2(bytes.clone()));
        let h3 = script_ref_hash(&PrimScriptRef::PlutusV3(bytes.clone()));
        assert_ne!(h1, h2);
        assert_ne!(h2, h3);
        assert_ne!(h1, h3);
    }

    #[test]
    fn script_ref_hash_plutus_v1_matches_manual_blake2b_224() {
        // Cross-check against the same `blake2b_224(tag || raw)` formula used
        // by the existing `dugite-cli policyid` path.
        let raw = vec![0x12, 0x34, 0x56];
        let expected = {
            let mut buf = vec![0x01];
            buf.extend_from_slice(&raw);
            dugite_primitives::hash::blake2b_224(&buf).0
        };
        assert_eq!(script_ref_hash(&PrimScriptRef::PlutusV1(raw)), expected);
    }

    #[test]
    fn script_ref_hash_native_uses_cbor_preimage_and_tag_zero() {
        // A NativeScript hash should equal blake2b_224(0x00 || cbor(script)).
        // We don't pin a specific byte value here (the encoder owns that),
        // but we verify the contract: independent encode + manual hash
        // yields the same digest the helper produced.
        let script = dugite_primitives::transaction::NativeScript::InvalidBefore(
            dugite_primitives::time::SlotNo(123),
        );
        let helper = script_ref_hash(&PrimScriptRef::NativeScript(script.clone()));
        let manual = {
            let cbor = dugite_serialization::encode::encode_native_script(&script);
            let mut buf = vec![0x00];
            buf.extend_from_slice(&cbor);
            dugite_primitives::hash::blake2b_224(&buf).0
        };
        assert_eq!(helper, manual);
    }

    #[test]
    fn script_ref_hash_native_differs_from_plutus_with_same_cbor() {
        // Same raw bytes used as plutus and as native preimage must hash
        // differently because the tag byte is different.
        let raw = vec![0x01, 0x02, 0x03];
        let plutus_h = script_ref_hash(&PrimScriptRef::PlutusV1(raw.clone()));
        let manual_native = {
            let mut buf = vec![0x00];
            buf.extend_from_slice(&raw);
            dugite_primitives::hash::blake2b_224(&buf).0
        };
        assert_ne!(plutus_h, manual_native);
    }

    #[test]
    fn output_to_plutus_populates_reference_script_when_present() {
        let mut out = enterprise_output(1);
        let raw = vec![0xff; 8];
        out.script_ref = Some(PrimScriptRef::PlutusV3(raw.clone()));
        let pl = output_to_plutus(&out).unwrap();
        assert_eq!(
            pl.reference_script,
            Some(script_ref_hash(&PrimScriptRef::PlutusV3(raw)))
        );
    }

    #[test]
    fn output_to_plutus_propagates_address_failure() {
        // Byron output triggers `address_to_plutus`'s `Internal` error.
        let out = PrimTxOut {
            address: PrimAddress::Byron(dugite_primitives::address::ByronAddress {
                payload: vec![],
            }),
            value: PrimValue::lovelace(1),
            datum: PrimOutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        };
        let err = output_to_plutus(&out).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // ────────────────────────────────────────────────────────────
    // input → TxInInfo resolution
    // ────────────────────────────────────────────────────────────

    fn resolved_entry(hash_byte: u8, idx: u32, lovelace: u64) -> (PrimTxIn, PrimTxOut, Vec<u8>) {
        (
            PrimTxIn {
                transaction_id: h32(hash_byte),
                index: idx,
            },
            enterprise_output(lovelace),
            vec![0x00],
        )
    }

    #[test]
    fn input_to_txininfo_finds_matching_resolved_entry() {
        let resolved = vec![
            resolved_entry(0xaa, 0, 1_000_000),
            resolved_entry(0xbb, 1, 2_000_000),
        ];
        let input = PrimTxIn {
            transaction_id: h32(0xbb),
            index: 1,
        };
        let info = input_to_txininfo(&input, &resolved).unwrap();
        assert_eq!(info.out_ref.tx_id, [0xbb; 32]);
        assert_eq!(info.out_ref.idx, 1);
        assert_eq!(
            info.resolved.value.policies[0].1[0].1,
            BigInt::from(2_000_000)
        );
    }

    #[test]
    fn input_to_txininfo_errors_when_input_not_in_resolved() {
        let resolved = vec![resolved_entry(0xaa, 0, 1)];
        let missing = PrimTxIn {
            transaction_id: h32(0xff),
            index: 9,
        };
        let err = input_to_txininfo(&missing, &resolved).unwrap_err();
        let PhaseTwoError::UtxoDecode(msg) = err else {
            panic!("expected UtxoDecode");
        };
        assert!(msg.contains("not in resolved-utxo map"), "got: {msg}");
        // The error should also surface the missing tx/idx for diagnosis.
        assert!(msg.contains("@9"), "got: {msg}");
    }

    /// Verifies that `inputs_to_txininfos` preserves the order of the slice it
    /// receives. Callers (populate_tx_info_v1/v2/v3) must sort the slice first
    /// via [`sort_inputs`]; this test confirms the function itself is pass-through.
    #[test]
    fn inputs_to_txininfos_preserves_caller_order() {
        let resolved = vec![
            resolved_entry(0xaa, 0, 1),
            resolved_entry(0xbb, 0, 2),
            resolved_entry(0xcc, 0, 3),
        ];
        // Caller-supplied order (unsorted): cc, aa, bb.
        let inputs = vec![
            PrimTxIn {
                transaction_id: h32(0xcc),
                index: 0,
            },
            PrimTxIn {
                transaction_id: h32(0xaa),
                index: 0,
            },
            PrimTxIn {
                transaction_id: h32(0xbb),
                index: 0,
            },
        ];
        let infos = inputs_to_txininfos(&inputs, &resolved).unwrap();
        let coins: Vec<u64> = infos
            .iter()
            .map(|i| match &i.resolved.value.policies[0].1[0].1 {
                v if v == &BigInt::from(1) => 1u64,
                v if v == &BigInt::from(2) => 2u64,
                v if v == &BigInt::from(3) => 3u64,
                _ => unreachable!(),
            })
            .collect();
        // cc=3, aa=1, bb=2 — caller order preserved.
        assert_eq!(coins, vec![3, 1, 2]);
    }

    /// Regression test for the Alonzo spend-redeemer sort-order bug.
    ///
    /// `sort_inputs` must return inputs in ascending `Ord TxIn` order:
    /// (TxId raw bytes lex, then TxIx numeric). This matches Haskell's
    /// `Set TxIn` which is what `txInfoInputs` / redeemer index resolution use.
    ///
    /// Vector mirrors the confirmed mainnet tx ground-truth:
    ///  wire[0]=0xfb#0, wire[1]=0xf9#1  →  sorted[0]=0xf9#1, sorted[1]=0xfb#0
    /// (since 0xf9 < 0xfb byte-lexicographically).
    #[test]
    fn sort_inputs_orders_by_txid_bytes_then_index() {
        // input_a: 0xfb…#0 — wire-first, but sorted-SECOND (0xfb > 0xf9).
        let input_a = PrimTxIn {
            transaction_id: h32(0xfb),
            index: 0,
        };
        // input_b: 0xf9…#1 — wire-second, but sorted-FIRST (0xf9 < 0xfb).
        let input_b = PrimTxIn {
            transaction_id: h32(0xf9),
            index: 1,
        };
        // Wire order: [input_a, input_b].
        let wire = vec![input_a.clone(), input_b.clone()];
        let sorted = sort_inputs(&wire);

        // sorted[0] must be the 0xf9 entry (smaller TxId), sorted[1] the 0xfb entry.
        assert_eq!(
            sorted[0].transaction_id.0, [0xf9u8; 32],
            "sorted[0] must be 0xf9 (smaller TxId)"
        );
        assert_eq!(sorted[0].index, 1);
        assert_eq!(
            sorted[1].transaction_id.0, [0xfbu8; 32],
            "sorted[1] must be 0xfb (larger TxId)"
        );
        assert_eq!(sorted[1].index, 0);

        // Redeemer Spend index=1 must now resolve to sorted[1]=0xfb#0, not wire[1]=0xf9#1.
        // (The full end-to-end is pinned in redeemer_resolve::tests::
        //  spend_redeemer_index_uses_sorted_input_set_not_wire_order.)
    }

    /// Same-TxId entries sort by index numerically (TxIx = Word16 numeric order).
    #[test]
    fn sort_inputs_same_txid_ordered_by_index() {
        let inputs = vec![
            PrimTxIn {
                transaction_id: h32(0xaa),
                index: 5,
            },
            PrimTxIn {
                transaction_id: h32(0xaa),
                index: 0,
            },
            PrimTxIn {
                transaction_id: h32(0xaa),
                index: 2,
            },
        ];
        let sorted = sort_inputs(&inputs);
        assert_eq!(sorted[0].index, 0);
        assert_eq!(sorted[1].index, 2);
        assert_eq!(sorted[2].index, 5);
    }

    #[test]
    fn inputs_to_txininfos_surfaces_first_missing_input() {
        let resolved = vec![resolved_entry(0xaa, 0, 1)];
        let inputs = vec![
            PrimTxIn {
                transaction_id: h32(0xaa),
                index: 0,
            },
            PrimTxIn {
                transaction_id: h32(0xbb),
                index: 0,
            },
        ];
        let err = inputs_to_txininfos(&inputs, &resolved).unwrap_err();
        let PhaseTwoError::UtxoDecode(msg) = err else {
            panic!("expected UtxoDecode");
        };
        assert!(msg.contains("@0"), "got: {msg}");
    }
}
