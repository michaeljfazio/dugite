//! Wire-form-preserving CBOR wrapper types.
//!
//! These types mirror the pallas-codec wrappers (`Nullable`, `MaybeIndefArray`,
//! `KeyValuePairs`) that the existing `multi_era.rs` decoder uses via pallas.
//! Replacing them with in-house equivalents severs the pallas-codec dependency.
//!
//! ## Design decisions
//!
//! - **`Nullable<T>`** distinguishes `null` from `undefined` at the type level,
//!   which matters for round-trip fidelity in Cardano's CBOR (some fields use
//!   `0xf7` undefined rather than `0xf6` null — though most use null).
//!
//! - **`MaybeIndef<T>`** preserves whether the source CBOR array was encoded as a
//!   definite-length or indefinite-length array. This is load-bearing for the
//!   `script_data_hash`: PlutusData arrays inside Conway-era `WitnessSet` may
//!   arrive as indefinite-length, and changing the encoding would break the hash.
//!
//! - **`KeyValuePairs<K, V>`** retains insertion order. The CBOR specification
//!   permits maps in any order; the Cardano ledger sorts map keys only when
//!   producing canonical form (outbound). Inbound decoding must preserve order
//!   to enable byte-exact re-encoding.

/// An optional CBOR value that distinguishes between three absence representations:
/// - `Some(v)` — a real value was present.
/// - `Null` — CBOR null (`0xf6`) was present.
/// - `Undefined` — CBOR undefined (`0xf7`) was present.
///
/// Most Cardano fields use `null`; a small number use `undefined` (e.g. some
/// Byron-era datum fields). Distinguishing them preserves round-trip fidelity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nullable<T> {
    /// A real value was decoded.
    Some(T),
    /// CBOR null (`0xf6`) was present.
    Null,
    /// CBOR undefined (`0xf7`) was present.
    Undefined,
}

impl<T> Nullable<T> {
    /// Return `true` if this is `Null` or `Undefined`.
    pub fn is_absent(&self) -> bool {
        matches!(self, Nullable::Null | Nullable::Undefined)
    }

    /// Return `true` if a value is present.
    pub fn is_present(&self) -> bool {
        matches!(self, Nullable::Some(_))
    }

    /// Convert to `Option<T>`, collapsing both absence variants to `None`.
    pub fn into_option(self) -> Option<T> {
        match self {
            Nullable::Some(v) => Option::Some(v),
            _ => None,
        }
    }

    /// Borrow the inner value if present.
    pub fn as_ref(&self) -> Option<&T> {
        match self {
            Nullable::Some(v) => Option::Some(v),
            _ => None,
        }
    }

    /// Map over the inner value, preserving absence variants.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Nullable<U> {
        match self {
            Nullable::Some(v) => Nullable::Some(f(v)),
            Nullable::Null => Nullable::Null,
            Nullable::Undefined => Nullable::Undefined,
        }
    }
}

impl<T: Default> Nullable<T> {
    /// Return the inner value or a default if absent.
    pub fn unwrap_or_default(self) -> T {
        match self {
            Nullable::Some(v) => v,
            _ => T::default(),
        }
    }
}

impl<T> From<Option<T>> for Nullable<T> {
    fn from(opt: Option<T>) -> Self {
        match opt {
            Option::Some(v) => Nullable::Some(v),
            None => Nullable::Null,
        }
    }
}

/// A CBOR array that preserves whether it was encoded as definite- or
/// indefinite-length.
///
/// Both variants hold the decoded items; the distinction controls how the
/// value is re-encoded. This is load-bearing for `script_data_hash`:
/// if the original transaction used an indefinite-length array for
/// `PlutusData`, re-encoding it as definite-length would produce a
/// different hash and break Plutus phase-2 validation.
///
/// # Usage
///
/// Decode with [`crate::decode::reader::Reader::read_array`] (which gives you
/// `Vec<T>`) and then wrap the result:
///
/// ```ignore
/// let items = r.read_array(decode_item)?;
/// let arr = MaybeIndef::Def(items); // if you know it was definite-length
/// ```
///
/// Or read the header yourself and branch:
///
/// ```ignore
/// let arr_len = r.read_array_header()?;
/// let items = ...; // read items
/// let arr = if arr_len.is_some() {
///     MaybeIndef::Def(items)
/// } else {
///     MaybeIndef::Indef(items)
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaybeIndef<T> {
    /// The items were encoded in a definite-length array (`0x80..0x9b`).
    Def(Vec<T>),
    /// The items were encoded in an indefinite-length array (`0x9f ... 0xff`).
    Indef(Vec<T>),
}

impl<T> MaybeIndef<T> {
    /// Borrow the items regardless of encoding.
    pub fn items(&self) -> &[T] {
        match self {
            MaybeIndef::Def(v) | MaybeIndef::Indef(v) => v,
        }
    }

    /// Consume and return the items.
    pub fn into_items(self) -> Vec<T> {
        match self {
            MaybeIndef::Def(v) | MaybeIndef::Indef(v) => v,
        }
    }

    /// Return `true` if the source encoding was indefinite-length.
    pub fn is_indef(&self) -> bool {
        matches!(self, MaybeIndef::Indef(_))
    }

    /// Return the number of items.
    pub fn len(&self) -> usize {
        self.items().len()
    }

    /// Return `true` if there are no items.
    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }

    /// Map over every item, preserving the definite/indefinite shape.
    pub fn map<U, F: FnMut(T) -> U>(self, f: F) -> MaybeIndef<U> {
        match self {
            MaybeIndef::Def(v) => MaybeIndef::Def(v.into_iter().map(f).collect()),
            MaybeIndef::Indef(v) => MaybeIndef::Indef(v.into_iter().map(f).collect()),
        }
    }
}

/// An ordered sequence of key-value pairs preserving insertion order.
///
/// CBOR maps do not mandate any key ordering; the Cardano ledger only sorts
/// keys when producing canonical (outbound) form. Inbound decoders must
/// preserve the order in which pairs appear so that re-encoding produces the
/// same bytes.
///
/// This type is a thin newtype over `Vec<(K, V)>`, exposing map-like
/// accessors for convenience without enforcing any ordering constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyValuePairs<K, V>(pub Vec<(K, V)>);

impl<K, V> KeyValuePairs<K, V> {
    /// Create an empty `KeyValuePairs`.
    pub fn new() -> Self {
        KeyValuePairs(Vec::new())
    }

    /// Create from an existing vector of pairs.
    pub fn from_vec(pairs: Vec<(K, V)>) -> Self {
        KeyValuePairs(pairs)
    }

    /// Return the number of pairs.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Return `true` if there are no pairs.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over key-value pairs by reference.
    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.0.iter()
    }

    /// Find the first value for a given key (using `PartialEq`).
    pub fn get(&self, key: &K) -> Option<&V>
    where
        K: PartialEq,
    {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Consume and return the underlying vector.
    pub fn into_vec(self) -> Vec<(K, V)> {
        self.0
    }
}

impl<K, V> Default for KeyValuePairs<K, V> {
    fn default() -> Self {
        KeyValuePairs::new()
    }
}

impl<K, V> From<Vec<(K, V)>> for KeyValuePairs<K, V> {
    fn from(v: Vec<(K, V)>) -> Self {
        KeyValuePairs(v)
    }
}

impl<K, V> IntoIterator for KeyValuePairs<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a KeyValuePairs<K, V> {
    type Item = &'a (K, V);
    type IntoIter = std::slice::Iter<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Nullable
    // -----------------------------------------------------------------------

    #[test]
    fn nullable_some_is_present() {
        let n: Nullable<u32> = Nullable::Some(42);
        assert!(n.is_present());
        assert!(!n.is_absent());
    }

    #[test]
    fn nullable_null_is_absent() {
        let n: Nullable<u32> = Nullable::Null;
        assert!(n.is_absent());
        assert!(!n.is_present());
    }

    #[test]
    fn nullable_undefined_is_absent() {
        let n: Nullable<u32> = Nullable::Undefined;
        assert!(n.is_absent());
    }

    #[test]
    fn nullable_into_option_some() {
        assert_eq!(Nullable::Some(7u32).into_option(), Some(7));
    }

    #[test]
    fn nullable_into_option_null() {
        assert_eq!(Nullable::<u32>::Null.into_option(), None);
    }

    #[test]
    fn nullable_into_option_undefined() {
        assert_eq!(Nullable::<u32>::Undefined.into_option(), None);
    }

    #[test]
    fn nullable_as_ref() {
        let n = Nullable::Some(99u32);
        assert_eq!(n.as_ref(), Some(&99));
        let n2: Nullable<u32> = Nullable::Null;
        assert_eq!(n2.as_ref(), None);
    }

    #[test]
    fn nullable_map() {
        let n = Nullable::Some(3u32);
        let m = n.map(|v| v * 2);
        assert_eq!(m, Nullable::Some(6));

        let n2: Nullable<u32> = Nullable::Null;
        let m2 = n2.map(|v| v * 2);
        assert_eq!(m2, Nullable::Null);
    }

    #[test]
    fn nullable_from_option() {
        let n: Nullable<u32> = Some(1).into();
        assert_eq!(n, Nullable::Some(1));
        let n2: Nullable<u32> = None.into();
        assert_eq!(n2, Nullable::Null);
    }

    #[test]
    fn nullable_unwrap_or_default() {
        assert_eq!(Nullable::Some(5u32).unwrap_or_default(), 5);
        assert_eq!(Nullable::<u32>::Null.unwrap_or_default(), 0);
        assert_eq!(Nullable::<u32>::Undefined.unwrap_or_default(), 0);
    }

    // -----------------------------------------------------------------------
    // MaybeIndef
    // -----------------------------------------------------------------------

    #[test]
    fn maybe_indef_def_items() {
        let arr: MaybeIndef<u32> = MaybeIndef::Def(vec![1, 2, 3]);
        assert_eq!(arr.items(), &[1, 2, 3]);
        assert!(!arr.is_indef());
    }

    #[test]
    fn maybe_indef_indef_items() {
        let arr: MaybeIndef<u32> = MaybeIndef::Indef(vec![4, 5]);
        assert_eq!(arr.items(), &[4, 5]);
        assert!(arr.is_indef());
    }

    #[test]
    fn maybe_indef_len_empty() {
        let arr: MaybeIndef<u32> = MaybeIndef::Def(vec![]);
        assert_eq!(arr.len(), 0);
        assert!(arr.is_empty());
    }

    #[test]
    fn maybe_indef_into_items() {
        let arr = MaybeIndef::Def(vec![10u32, 20]);
        let items = arr.into_items();
        assert_eq!(items, vec![10, 20]);
    }

    #[test]
    fn maybe_indef_map_def() {
        let arr = MaybeIndef::Def(vec![1u32, 2, 3]);
        let doubled = arr.map(|x| x * 2);
        assert_eq!(doubled, MaybeIndef::Def(vec![2, 4, 6]));
        assert!(!doubled.is_indef());
    }

    #[test]
    fn maybe_indef_map_indef_preserves_shape() {
        let arr = MaybeIndef::Indef(vec![1u32, 2]);
        let doubled = arr.map(|x| x * 2);
        assert!(doubled.is_indef());
        assert_eq!(doubled.into_items(), vec![2, 4]);
    }

    #[test]
    fn maybe_indef_equality() {
        // Def and Indef with the same items are NOT equal — the shape matters.
        let def = MaybeIndef::Def(vec![1u32]);
        let indef = MaybeIndef::Indef(vec![1u32]);
        assert_ne!(def, indef);
    }

    // -----------------------------------------------------------------------
    // KeyValuePairs
    // -----------------------------------------------------------------------

    #[test]
    fn kvp_new_empty() {
        let kvp: KeyValuePairs<u32, u32> = KeyValuePairs::new();
        assert!(kvp.is_empty());
        assert_eq!(kvp.len(), 0);
    }

    #[test]
    fn kvp_from_vec() {
        let pairs = vec![(1u32, "a"), (2, "b")];
        let kvp = KeyValuePairs::from_vec(pairs.clone());
        assert_eq!(kvp.0, pairs);
    }

    #[test]
    fn kvp_preserves_insertion_order() {
        // Keys 3, 1, 2 in insertion order — must NOT be sorted.
        let kvp = KeyValuePairs::from_vec(vec![(3u32, "c"), (1, "a"), (2, "b")]);
        let keys: Vec<u32> = kvp.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![3, 1, 2]);
    }

    #[test]
    fn kvp_get_first_match() {
        let kvp = KeyValuePairs::from_vec(vec![(1u32, "a"), (2, "b"), (1, "c")]);
        // get returns the first occurrence.
        assert_eq!(kvp.get(&1), Some(&"a"));
        assert_eq!(kvp.get(&2), Some(&"b"));
        assert_eq!(kvp.get(&99), None);
    }

    #[test]
    fn kvp_into_iter() {
        let kvp = KeyValuePairs::from_vec(vec![(10u32, 100u32), (20, 200)]);
        let collected: Vec<(u32, u32)> = kvp.into_iter().collect();
        assert_eq!(collected, vec![(10, 100), (20, 200)]);
    }

    #[test]
    fn kvp_ref_iter() {
        let kvp = KeyValuePairs::from_vec(vec![(1u32, 2u32)]);
        let collected: Vec<&(u32, u32)> = (&kvp).into_iter().collect();
        assert_eq!(collected.len(), 1);
        assert_eq!(*collected[0], (1, 2));
    }

    #[test]
    fn kvp_default_is_empty() {
        let kvp: KeyValuePairs<u32, u32> = KeyValuePairs::default();
        assert!(kvp.is_empty());
    }

    #[test]
    fn kvp_from_vec_trait() {
        let v = vec![(1u32, "x")];
        let kvp: KeyValuePairs<u32, &str> = v.into();
        assert_eq!(kvp.len(), 1);
    }

    #[test]
    fn kvp_into_vec() {
        let pairs = vec![(5u32, "five")];
        let kvp = KeyValuePairs::from_vec(pairs.clone());
        assert_eq!(kvp.into_vec(), pairs);
    }
}
