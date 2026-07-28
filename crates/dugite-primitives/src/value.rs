use crate::hash::PolicyId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Lovelace (1 ADA = 1,000,000 Lovelace)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Lovelace(pub u64);

impl Lovelace {
    pub const ZERO: Self = Lovelace(0);

    pub fn to_ada(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    pub fn from_ada(ada: f64) -> Self {
        Lovelace((ada * 1_000_000.0) as u64)
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Lovelace)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Lovelace)
    }
}

impl std::ops::Add for Lovelace {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Lovelace(self.0.saturating_add(rhs.0))
    }
}

impl std::ops::Sub for Lovelace {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Lovelace(self.0.saturating_sub(rhs.0))
    }
}

impl std::ops::AddAssign for Lovelace {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl std::fmt::Display for Lovelace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} lovelace", self.0)
    }
}

/// Asset name (up to 32 bytes)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AssetName(pub Vec<u8>);

impl AssetName {
    pub const MAX_LENGTH: usize = 32;

    pub fn new(bytes: Vec<u8>) -> Result<Self, &'static str> {
        if bytes.len() > Self::MAX_LENGTH {
            return Err("Asset name exceeds 32 bytes");
        }
        Ok(AssetName(bytes))
    }

    pub fn empty() -> Self {
        AssetName(Vec::new())
    }

    pub fn as_utf8(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }
}

impl std::fmt::Display for AssetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.as_utf8() {
            Some(s) if s.chars().all(|c| c.is_ascii_graphic()) => write!(f, "{}", s),
            _ => write!(f, "0x{}", self.to_hex()),
        }
    }
}

/// Multi-asset value: ADA + native tokens
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Value {
    pub coin: Lovelace,
    pub multi_asset: MultiAsset,
}

/// Map from PolicyId -> AssetName -> quantity
pub type MultiAsset = BTreeMap<PolicyId, BTreeMap<AssetName, u64>>;

impl Value {
    pub fn lovelace(coin: u64) -> Self {
        Value {
            coin: Lovelace(coin),
            multi_asset: BTreeMap::new(),
        }
    }

    pub fn is_pure_ada(&self) -> bool {
        self.multi_asset.is_empty()
    }

    /// The Mary-era `Val` instance `size` for a `MaryValue` — a CBOR-independent
    /// "heap word count" shared by both Mary's `scaledMinDeposit` and Alonzo's
    /// `utxoEntrySize` minimum-UTxO formulas (Alonzo's `Value` type IS Mary's
    /// `MaryValue`; see `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Era.hs:37`,
    /// `type instance Value AlonzoEra = MaryValue`).
    ///
    /// Reference: `Cardano.Ledger.Mary.Value` (`eras/mary/impl/src/Cardano/
    /// Ledger/Mary/Value.hs:225-244,264-282,714-728`), oracle-verified live
    /// against IntersectMBO/cardano-ledger for issue #919 (2026-07-29).
    ///
    /// Ada-only (no multi-assets): `size = 2`. This is NOT a "Mary" special
    /// case — Mary's `scaledMinDeposit` never actually calls this function
    /// for ada-only values (it short-circuits to the flat `minUTxOValue`
    /// first, see `ProtocolParameters::min_coin_for_output`). Alonzo's
    /// `utxoEntrySize`, by contrast, calls `size` UNCONDITIONALLY — an
    /// ada-only Alonzo output genuinely costs `27 + 2 + dataHashSize` words.
    ///
    /// Multi-asset: `6 + roundupBytesToWords(representationSize)` where
    /// `roundupBytesToWords(b) = (b + 7) / 8` (ceiling division, word = 8
    /// bytes) and `representationSize` sums three terms: `12*numTriples`,
    /// `28*numDistinctPolicyIds`, and the total byte length of every
    /// distinct asset name. `numTriples` counts every (policy, assetName)
    /// pair with non-zero quantity; `28` is the Blake2b-224 policy-ID hash
    /// size; the asset-name byte sum is deduplicated by NAME BYTES ALONE
    /// across ALL policies (a Haskell `Set` of name bytestrings) — two
    /// different policies minting a token of the identical name pay for
    /// those name bytes only once.
    pub fn mary_value_size(&self) -> u64 {
        if self.multi_asset.is_empty() {
            return 2;
        }
        let num_triples: u64 = self.multi_asset.values().map(|m| m.len() as u64).sum();
        let num_policies = self.multi_asset.len() as u64;
        let mut unique_names: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        for assets in self.multi_asset.values() {
            for name in assets.keys() {
                unique_names.insert(name.0.as_slice());
            }
        }
        let name_bytes: u64 = unique_names.iter().map(|n| n.len() as u64).sum();
        let representation_size = 12 * num_triples + 28 * num_policies + name_bytes;
        let rounded_words = representation_size.div_ceil(8);
        6 + rounded_words
    }

    pub fn add(&self, other: &Value) -> Self {
        let coin = self.coin + other.coin; // Lovelace::Add is saturating
        let mut multi_asset = self.multi_asset.clone();
        for (policy, assets) in &other.multi_asset {
            let entry = multi_asset.entry(*policy).or_default();
            for (name, qty) in assets {
                let e = entry.entry(name.clone()).or_insert(0);
                *e = e.saturating_add(*qty);
            }
        }
        Value { coin, multi_asset }
    }

    /// Check if this value is greater than or equal to another (for UTxO validation)
    pub fn geq(&self, other: &Value) -> bool {
        if self.coin.0 < other.coin.0 {
            return false;
        }
        for (policy, assets) in &other.multi_asset {
            match self.multi_asset.get(policy) {
                None => return false,
                Some(self_assets) => {
                    for (name, qty) in assets {
                        match self_assets.get(name) {
                            None => return false,
                            Some(self_qty) if self_qty < qty => return false,
                            _ => {}
                        }
                    }
                }
            }
        }
        true
    }

    pub fn policy_count(&self) -> usize {
        self.multi_asset.len()
    }

    pub fn asset_count(&self) -> usize {
        self.multi_asset.values().map(|a| a.len()).sum()
    }
}

impl Default for Value {
    fn default() -> Self {
        Value::lovelace(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash28;

    #[test]
    fn test_lovelace_ada_conversion() {
        let l = Lovelace(2_500_000);
        assert!((l.to_ada() - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_value_geq() {
        let v1 = Value::lovelace(5_000_000);
        let v2 = Value::lovelace(3_000_000);
        assert!(v1.geq(&v2));
        assert!(!v2.geq(&v1));
    }

    #[test]
    fn test_multi_asset_value() {
        let policy = Hash28::from_bytes([1u8; 28]);
        let asset_name = AssetName::new(b"TestToken".to_vec()).unwrap();

        let mut v = Value::lovelace(2_000_000);
        v.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset_name, 100);

        assert!(!v.is_pure_ada());
        assert_eq!(v.policy_count(), 1);
        assert_eq!(v.asset_count(), 1);
    }

    // ========================================================================
    // Lovelace saturating arithmetic tests
    // ========================================================================

    #[test]
    fn test_lovelace_add_normal() {
        let a = Lovelace(5_000_000);
        let b = Lovelace(3_000_000);
        assert_eq!(a + b, Lovelace(8_000_000));
    }

    #[test]
    fn test_lovelace_add_saturates() {
        let a = Lovelace(u64::MAX);
        let b = Lovelace(1);
        assert_eq!(a + b, Lovelace(u64::MAX));
    }

    #[test]
    fn test_lovelace_add_both_large() {
        let a = Lovelace(u64::MAX / 2 + 1);
        let b = Lovelace(u64::MAX / 2 + 1);
        assert_eq!(a + b, Lovelace(u64::MAX));
    }

    #[test]
    fn test_lovelace_sub_normal() {
        let a = Lovelace(5_000_000);
        let b = Lovelace(3_000_000);
        assert_eq!(a - b, Lovelace(2_000_000));
    }

    #[test]
    fn test_lovelace_sub_saturates() {
        let a = Lovelace(3_000_000);
        let b = Lovelace(5_000_000);
        assert_eq!(a - b, Lovelace(0));
    }

    #[test]
    fn test_lovelace_add_assign_normal() {
        let mut a = Lovelace(5_000_000);
        a += Lovelace(3_000_000);
        assert_eq!(a, Lovelace(8_000_000));
    }

    #[test]
    fn test_lovelace_add_assign_saturates() {
        let mut a = Lovelace(u64::MAX);
        a += Lovelace(1);
        assert_eq!(a, Lovelace(u64::MAX));
    }

    #[test]
    fn test_lovelace_checked_add() {
        assert_eq!(Lovelace(5).checked_add(Lovelace(3)), Some(Lovelace(8)));
        assert_eq!(Lovelace(u64::MAX).checked_add(Lovelace(1)), None);
    }

    #[test]
    fn test_lovelace_checked_sub() {
        assert_eq!(Lovelace(5).checked_sub(Lovelace(3)), Some(Lovelace(2)));
        assert_eq!(Lovelace(3).checked_sub(Lovelace(5)), None);
    }

    #[test]
    fn test_value_add_saturates_coin() {
        let v1 = Value::lovelace(u64::MAX);
        let v2 = Value::lovelace(1);
        let sum = v1.add(&v2);
        assert_eq!(sum.coin, Lovelace(u64::MAX));
    }

    #[test]
    fn test_value_add_merges_multi_asset() {
        let policy = Hash28::from_bytes([1u8; 28]);
        let asset = AssetName::new(b"Token".to_vec()).unwrap();

        let mut v1 = Value::lovelace(1_000_000);
        v1.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 50);

        let mut v2 = Value::lovelace(2_000_000);
        v2.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 30);

        let sum = v1.add(&v2);
        assert_eq!(sum.coin, Lovelace(3_000_000));
        assert_eq!(sum.multi_asset[&policy][&asset], 80);
    }

    // -----------------------------------------------------------------------
    // Additional value and lovelace tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_lovelace_display_format() {
        assert_eq!(format!("{}", Lovelace(0)), "0 lovelace");
        assert_eq!(format!("{}", Lovelace(1_000_000)), "1000000 lovelace");
        assert_eq!(
            format!("{}", Lovelace(u64::MAX)),
            format!("{} lovelace", u64::MAX)
        );
    }

    #[test]
    fn test_lovelace_zero_identity() {
        let a = Lovelace(5_000_000);
        assert_eq!(a + Lovelace::ZERO, a);
        assert_eq!(a - Lovelace::ZERO, a);
    }

    #[test]
    fn test_value_add_disjoint_policies() {
        // Two values with different policies should merge without conflict
        let policy_a = Hash28::from_bytes([1u8; 28]);
        let policy_b = Hash28::from_bytes([2u8; 28]);
        let asset = AssetName::new(b"Tok".to_vec()).unwrap();

        let mut v1 = Value::lovelace(1_000_000);
        v1.multi_asset
            .entry(policy_a)
            .or_default()
            .insert(asset.clone(), 100);

        let mut v2 = Value::lovelace(2_000_000);
        v2.multi_asset
            .entry(policy_b)
            .or_default()
            .insert(asset.clone(), 200);

        let sum = v1.add(&v2);
        assert_eq!(sum.coin, Lovelace(3_000_000));
        assert_eq!(sum.policy_count(), 2);
        assert_eq!(sum.multi_asset[&policy_a][&asset], 100);
        assert_eq!(sum.multi_asset[&policy_b][&asset], 200);
    }

    #[test]
    fn test_value_add_multiple_assets_same_policy() {
        let policy = Hash28::from_bytes([1u8; 28]);
        let asset_a = AssetName::new(b"TokenA".to_vec()).unwrap();
        let asset_b = AssetName::new(b"TokenB".to_vec()).unwrap();

        let mut v1 = Value::lovelace(1_000_000);
        v1.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset_a.clone(), 10);

        let mut v2 = Value::lovelace(2_000_000);
        v2.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset_b.clone(), 20);

        let sum = v1.add(&v2);
        assert_eq!(sum.policy_count(), 1);
        assert_eq!(sum.asset_count(), 2);
        assert_eq!(sum.multi_asset[&policy][&asset_a], 10);
        assert_eq!(sum.multi_asset[&policy][&asset_b], 20);
    }

    #[test]
    fn test_value_geq_with_multi_asset() {
        let policy = Hash28::from_bytes([1u8; 28]);
        let asset = AssetName::new(b"Token".to_vec()).unwrap();

        let mut v1 = Value::lovelace(5_000_000);
        v1.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 100);

        let mut v2 = Value::lovelace(3_000_000);
        v2.multi_asset
            .entry(policy)
            .or_default()
            .insert(asset.clone(), 50);

        assert!(v1.geq(&v2));
        assert!(!v2.geq(&v1));

        // Equal values
        assert!(v1.geq(&v1));
    }

    #[test]
    fn test_value_geq_missing_policy() {
        let policy = Hash28::from_bytes([1u8; 28]);
        let asset = AssetName::new(b"Tok".to_vec()).unwrap();

        let v1 = Value::lovelace(5_000_000); // no multi-asset

        let mut v2 = Value::lovelace(1_000_000);
        v2.multi_asset.entry(policy).or_default().insert(asset, 10);

        // v1 has enough ADA but missing the token policy
        assert!(!v1.geq(&v2));
    }

    #[test]
    fn test_value_default_is_zero() {
        let v = Value::default();
        assert_eq!(v.coin, Lovelace::ZERO);
        assert!(v.is_pure_ada());
        assert_eq!(v.policy_count(), 0);
        assert_eq!(v.asset_count(), 0);
    }

    #[test]
    fn test_asset_name_max_length() {
        // 32 bytes should succeed
        let ok = AssetName::new(vec![0u8; 32]);
        assert!(ok.is_ok());

        // 33 bytes should fail
        let too_long = AssetName::new(vec![0u8; 33]);
        assert!(too_long.is_err());
    }

    /// Length-lattice: test the full neighbourhood around the 32-byte boundary.
    /// This guards against D4 (AssetName::new() bypass in convert_value_assets /
    /// convert_mint); those call sites must always route through AssetName::new().
    #[test]
    fn test_asset_name_length_lattice() {
        // All lengths 0..=32 must be accepted.
        for n in 0..=32usize {
            assert!(
                AssetName::new(vec![0u8; n]).is_ok(),
                "length {n} should be accepted"
            );
        }
        // All lengths 33..=64 must be rejected.
        for n in 33..=64usize {
            assert!(
                AssetName::new(vec![0u8; n]).is_err(),
                "length {n} should be rejected"
            );
        }
    }

    #[test]
    fn test_asset_name_display() {
        // ASCII-printable should display as text
        let ascii = AssetName::new(b"MyToken".to_vec()).unwrap();
        assert_eq!(format!("{ascii}"), "MyToken");

        // Non-UTF8 should display as hex
        let binary = AssetName::new(vec![0xFF, 0xFE]).unwrap();
        assert_eq!(format!("{binary}"), "0xfffe");

        // Empty should display as empty string (no bytes to encode)
        let empty = AssetName::empty();
        assert_eq!(format!("{empty}"), "");
    }

    #[test]
    fn test_lovelace_from_ada() {
        let l = Lovelace::from_ada(1.5);
        assert_eq!(l, Lovelace(1_500_000));

        let l2 = Lovelace::from_ada(0.0);
        assert_eq!(l2, Lovelace(0));
    }

    // -----------------------------------------------------------------------
    // #919: `mary_value_size` oracle-verified goldens (IntersectMBO/cardano-ledger,
    // `Cardano.Ledger.Mary.Value`, live-verified 2026-07-29).
    // -----------------------------------------------------------------------

    use crate::hash::PolicyId;

    #[test]
    fn mary_value_size_ada_only_is_two() {
        let v = Value::lovelace(1_000_000);
        assert_eq!(v.mary_value_size(), 2);
    }

    #[test]
    fn mary_value_size_one_policy_one_zero_byte_asset_name() {
        // representationSize = 12*1 + 28*1 + 0 = 40 -> roundup(40/8) = 5 -> size = 6+5 = 11.
        let mut v = Value::lovelace(0);
        let policy = PolicyId::from_bytes([0x11u8; 28]);
        let mut assets = BTreeMap::new();
        assets.insert(AssetName::empty(), 1u64);
        v.multi_asset.insert(policy, assets);
        assert_eq!(v.mary_value_size(), 11);
    }

    #[test]
    fn mary_value_size_one_policy_one_eight_byte_asset_name() {
        // representationSize = 12*1 + 28*1 + 8 = 48 -> roundup(48/8) = 6 -> size = 6+6 = 12.
        let mut v = Value::lovelace(0);
        let policy = PolicyId::from_bytes([0x22u8; 28]);
        let mut assets = BTreeMap::new();
        assets.insert(AssetName::new(vec![0u8; 8]).unwrap(), 1u64);
        v.multi_asset.insert(policy, assets);
        assert_eq!(v.mary_value_size(), 12);
    }

    #[test]
    fn mary_value_size_two_policies_three_entries_twelve_unique_name_bytes() {
        // representationSize = 12*3 + 28*2 + 12 = 36+56+12 = 104 -> roundup(104/8) = 13 -> size = 19.
        let mut v = Value::lovelace(0);
        let policy_a = PolicyId::from_bytes([0x01u8; 28]);
        let policy_b = PolicyId::from_bytes([0x02u8; 28]);
        let mut assets_a = BTreeMap::new();
        assets_a.insert(AssetName::new(vec![0u8; 4]).unwrap(), 1u64);
        assets_a.insert(AssetName::new(vec![1u8; 4]).unwrap(), 1u64);
        let mut assets_b = BTreeMap::new();
        assets_b.insert(AssetName::new(vec![2u8; 4]).unwrap(), 1u64);
        v.multi_asset.insert(policy_a, assets_a);
        v.multi_asset.insert(policy_b, assets_b);
        assert_eq!(v.mary_value_size(), 19);
    }

    #[test]
    fn mary_value_size_dedups_identical_asset_names_across_policies() {
        // Two policies both minting an identical 7-byte-named token: the name
        // bytes are counted ONCE (a Haskell `Set` of name bytestrings), not
        // once per (policy, name) pair.
        // representationSize = 12*2 (numTriples) + 28*2 (numPids) + 7 (dedup'd name) = 24+56+7 = 87
        // -> roundup(87/8) = 11 -> size = 17.
        let mut v = Value::lovelace(0);
        let policy_a = PolicyId::from_bytes([0xaau8; 28]);
        let policy_b = PolicyId::from_bytes([0xbbu8; 28]);
        let name = AssetName::new(b"MyToken".to_vec()).unwrap();
        let mut assets_a = BTreeMap::new();
        assets_a.insert(name.clone(), 1u64);
        let mut assets_b = BTreeMap::new();
        assets_b.insert(name, 1u64);
        v.multi_asset.insert(policy_a, assets_a);
        v.multi_asset.insert(policy_b, assets_b);
        assert_eq!(v.mary_value_size(), 17);
    }
}
