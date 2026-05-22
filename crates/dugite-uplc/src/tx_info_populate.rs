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

use crate::phase_two::{PhaseTwoError, SlotConfig};
use crate::script_context::{
    Address as PlAddress, AssetEntry, Credential as PlCredential, PlutusValue, PosixTimeRange,
    PubKeyHash, ScriptHash, StakingCredential, TxId, TxOutRef,
};
use dugite_primitives::address::Address as PrimAddress;
use dugite_primitives::credentials::{Credential as PrimCred, Pointer as PrimPointer};
use dugite_primitives::transaction::Withdrawal as PrimWithdrawal;
use dugite_primitives::value::Value as PrimValue;
use num_bigint::BigInt;
use std::collections::BTreeMap;

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

/// Convert a slot number to POSIX milliseconds using the supplied
/// [`SlotConfig`].
///
/// Plutus' `TxInfo.valid_range` is expressed in POSIX milliseconds since
/// the Unix epoch — the same convention `cardano-node` uses. The
/// translation is:
///
/// ```text
/// posix_ms = (slot - slot_zero_offset) * slot_length_ms
///                                     + network_start_unix_seconds * 1000
/// ```
///
/// Returns an error if `slot < slot_zero_offset` (would produce a
/// negative time) — that condition should never fire in practice
/// (the ledger constructs the validity range from the tx's own
/// `validity_interval_start` / `ttl`, both of which post-date the
/// network start) but rejecting it explicitly keeps us panic-free.
pub fn slot_to_posix_ms(slot: u64, sc: &SlotConfig) -> Result<i64, PhaseTwoError> {
    let rel = slot.checked_sub(sc.slot_zero_offset).ok_or_else(|| {
        PhaseTwoError::Internal(format!(
            "slot {slot} < slot_zero_offset {}",
            sc.slot_zero_offset
        ))
    })?;
    let delta_ms = (rel as i128) * (sc.slot_length_ms as i128);
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

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{
        BaseAddress, EnterpriseAddress, PointerAddress, RewardAddress,
    };
    use dugite_primitives::credentials::Pointer as PrimPointer;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::network::NetworkId;
    use dugite_primitives::transaction::{TransactionInput, Withdrawal};
    use dugite_primitives::value::{AssetName, Lovelace};

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
    fn slot_to_posix_ms_rejects_slot_before_offset() {
        let sc = SlotConfig {
            network_start_unix_seconds: 0,
            slot_zero_offset: 1_000,
            slot_length_ms: 1_000,
        };
        let err = slot_to_posix_ms(500, &sc).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
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
}
