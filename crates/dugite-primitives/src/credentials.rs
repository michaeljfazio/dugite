use crate::hash::{Hash28, ScriptHash};
use serde::{Deserialize, Serialize};

/// Payment or staking credential
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Credential {
    /// Verification key hash credential
    VerificationKey(Hash28),
    /// Script hash credential (native or Plutus)
    Script(ScriptHash),
}

impl Credential {
    pub fn to_hash(&self) -> &Hash28 {
        match self {
            Credential::VerificationKey(h) => h,
            Credential::Script(h) => h,
        }
    }

    pub fn is_script(&self) -> bool {
        matches!(self, Credential::Script(_))
    }

    /// Order two credentials in **ledger** order: `Script` BEFORE
    /// `VerificationKey`, tie-broken by the 28-byte hash ascending.
    ///
    /// This is the ordering the Haskell ledger `Credential` derives —
    /// `ScriptHashObj` is the first data constructor and `KeyHashObj` the
    /// second, so the derived `Ord` is `ScriptHashObj < KeyHashObj`
    /// (i.e. **Script < Key**). It is the OPPOSITE of this enum's *derived*
    /// `Ord` (`VerificationKey(0) < Script(1)` ⇒ Key < Script), which models
    /// the Plutus `Credential` Data-tag order and the canonical CBOR key-byte
    /// order.
    ///
    /// Use this comparator at every site where the *ledger* `Credential`
    /// ordering is observable — the phase-2 `ScriptContext` / `TxInfo`
    /// construction and the `redeemerPointerInverse` (`Set.elemAt`) index
    /// space (e.g. `txInfoWdrl`, `txInfoVotes`, `TreasuryWithdrawals` /
    /// `UpdateCommittee` maps). The derived `Ord` (Key < Script) MUST stay
    /// unchanged for its Plutus/CBOR roles.
    pub fn cmp_ledger(&self, other: &Self) -> core::cmp::Ordering {
        // Script sorts before VerificationKey: rank Script = 0, Key = 1.
        fn rank(c: &Credential) -> u8 {
            match c {
                Credential::Script(_) => 0,
                Credential::VerificationKey(_) => 1,
            }
        }
        rank(self)
            .cmp(&rank(other))
            .then_with(|| self.to_hash().as_bytes().cmp(other.to_hash().as_bytes()))
    }

    /// Convert to a 32-byte hash that preserves the credential TYPE.
    ///
    /// The 28-byte hash is zero-padded to 32 bytes, with byte 28 set to
    /// `0x01` for script credentials and `0x00` for key credentials.
    /// This ensures that a key hash and script hash with identical 28-byte
    /// values produce DIFFERENT Hash32 keys, matching Haskell's `Credential`
    /// type which distinguishes `KeyHashObj` from `ScriptHashObj`.
    pub fn to_typed_hash32(&self) -> crate::hash::Hash<32> {
        let mut bytes = [0u8; 32];
        bytes[..28].copy_from_slice(self.to_hash().as_bytes());
        if self.is_script() {
            bytes[28] = 0x01;
        }
        crate::hash::Hash::<32>(bytes)
    }
}

/// Stake credential reference
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StakeReference {
    /// Stake credential embedded in the address
    StakeCredential(Credential),
    /// Pointer to a stake registration certificate
    Pointer(Pointer),
    /// No staking component
    Null,
}

/// Certificate pointer (slot, tx_index, cert_index)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pointer {
    pub slot: u64,
    pub tx_index: u64,
    pub cert_index: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash;

    fn key_hash() -> Hash28 {
        Hash::from_bytes([0x01; 28])
    }

    fn script_hash() -> ScriptHash {
        Hash::from_bytes([0x02; 28])
    }

    // ========== Credential::to_hash ==========

    #[test]
    fn test_to_hash_verification_key() {
        let cred = Credential::VerificationKey(key_hash());
        assert_eq!(cred.to_hash(), &key_hash());
    }

    #[test]
    fn test_to_hash_script() {
        let cred = Credential::Script(script_hash());
        assert_eq!(cred.to_hash(), &script_hash());
    }

    // ========== Credential::is_script ==========

    #[test]
    fn test_is_script_false_for_key() {
        let cred = Credential::VerificationKey(key_hash());
        assert!(!cred.is_script());
    }

    #[test]
    fn test_is_script_true_for_script() {
        let cred = Credential::Script(script_hash());
        assert!(cred.is_script());
    }

    // ========== Credential::to_typed_hash32 ==========

    #[test]
    fn test_to_typed_hash32_key_padding() {
        let cred = Credential::VerificationKey(key_hash());
        let h32 = cred.to_typed_hash32();
        let bytes = h32.as_bytes();
        // First 28 bytes match the input hash
        assert_eq!(&bytes[..28], &[0x01; 28]);
        // Byte 28 is 0x00 for key credentials
        assert_eq!(bytes[28], 0x00);
        // Remaining bytes are zero
        assert_eq!(&bytes[29..], &[0x00; 3]);
    }

    #[test]
    fn test_to_typed_hash32_script_padding() {
        let cred = Credential::Script(script_hash());
        let h32 = cred.to_typed_hash32();
        let bytes = h32.as_bytes();
        // First 28 bytes match the input hash
        assert_eq!(&bytes[..28], &[0x02; 28]);
        // Byte 28 is 0x01 for script credentials
        assert_eq!(bytes[28], 0x01);
        // Remaining bytes are zero
        assert_eq!(&bytes[29..], &[0x00; 3]);
    }

    #[test]
    fn test_to_typed_hash32_distinctness() {
        // Critical invariant: same 28-byte hash produces DIFFERENT Hash32
        // for key vs script credentials.
        let same_hash = Hash::from_bytes([0xaa; 28]);
        let key_cred = Credential::VerificationKey(same_hash);
        let script_cred = Credential::Script(same_hash);
        assert_ne!(key_cred.to_typed_hash32(), script_cred.to_typed_hash32());
    }

    // ========== Credential Ord ==========

    #[test]
    fn test_credential_ord_key_before_script() {
        // Derived Ord: enum variant order (VerificationKey=0 < Script=1) ⇒
        // Key < Script. This is the *Plutus* `Credential` Data-tag order and
        // the canonical CBOR key-byte order — it is the OPPOSITE of the
        // *ledger* `Credential` Ord (`ScriptHashObj < KeyHashObj` ⇒
        // Script < Key). Where ledger semantics are observable (phase-2
        // ScriptContext / TxInfo / redeemerPointerInverse index space) use
        // `Credential::cmp_ledger`, which orders Script before Key. Do NOT
        // reuse this derived Ord for those sites.
        let key = Credential::VerificationKey(key_hash());
        let script = Credential::Script(script_hash());
        assert!(key < script);
    }

    // ========== Credential::cmp_ledger (ledger Script < Key) ==========

    #[test]
    fn test_cmp_ledger_script_before_key_distinct_hashes() {
        // key_hash()=0x01.., script_hash()=0x02.. — different hashes. Ledger
        // order puts the Script credential FIRST regardless of hash bytes.
        let key = Credential::VerificationKey(key_hash());
        let script = Credential::Script(script_hash());
        assert_eq!(script.cmp_ledger(&key), core::cmp::Ordering::Less);
        assert_eq!(key.cmp_ledger(&script), core::cmp::Ordering::Greater);
        // And the derived enum Ord disagrees (Key < Script) — confirming the
        // two orderings are genuinely opposite on the type tie-break.
        assert!(key < script);
    }

    #[test]
    fn test_cmp_ledger_script_before_key_same_hash() {
        // Adversarial same-28-byte-hash collision: the script credential must
        // still sort before the key credential under the ledger comparator.
        let same = Hash::from_bytes([0xaa; 28]);
        let key = Credential::VerificationKey(same);
        let script = Credential::Script(same);
        assert_eq!(script.cmp_ledger(&key), core::cmp::Ordering::Less);
        assert_eq!(key.cmp_ledger(&script), core::cmp::Ordering::Greater);
        // Reflexive: identical credentials compare Equal.
        assert_eq!(key.cmp_ledger(&key), core::cmp::Ordering::Equal);
        assert_eq!(script.cmp_ledger(&script), core::cmp::Ordering::Equal);
    }

    #[test]
    fn test_cmp_ledger_same_type_orders_by_hash() {
        // Within the same credential type the 28-byte hash breaks the tie,
        // ascending.
        let lo = Credential::Script(Hash::from_bytes([0x01; 28]));
        let hi = Credential::Script(Hash::from_bytes([0x02; 28]));
        assert_eq!(lo.cmp_ledger(&hi), core::cmp::Ordering::Less);
        let lo_k = Credential::VerificationKey(Hash::from_bytes([0x01; 28]));
        let hi_k = Credential::VerificationKey(Hash::from_bytes([0x02; 28]));
        assert_eq!(lo_k.cmp_ledger(&hi_k), core::cmp::Ordering::Less);
    }

    // ========== StakeReference ==========

    #[test]
    fn test_stake_reference_serde_roundtrip() {
        let variants = vec![
            StakeReference::StakeCredential(Credential::VerificationKey(key_hash())),
            StakeReference::Pointer(Pointer {
                slot: 100,
                tx_index: 2,
                cert_index: 0,
            }),
            StakeReference::Null,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let v2: StakeReference = serde_json::from_str(&json).unwrap();
            assert_eq!(v, v2);
        }
    }

    // ========== Pointer ==========

    #[test]
    fn test_pointer_ord() {
        let p1 = Pointer {
            slot: 1,
            tx_index: 0,
            cert_index: 0,
        };
        let p2 = Pointer {
            slot: 2,
            tx_index: 0,
            cert_index: 0,
        };
        assert!(p1 < p2);
    }

    #[test]
    fn test_pointer_serde_roundtrip() {
        let p = Pointer {
            slot: 42,
            tx_index: 3,
            cert_index: 1,
        };
        let json = serde_json::to_string(&p).unwrap();
        let p2: Pointer = serde_json::from_str(&json).unwrap();
        assert_eq!(p, p2);
    }
}
