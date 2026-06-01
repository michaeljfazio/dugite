//! `WithdrawalsNotInRewardsCERTS` validation.
//!
//! This module implements the Conway-era reward-withdrawal sanity check that
//! ensures every withdrawal in a transaction:
//!
//! 1. Is on the right network (header byte network bit matches the node's network),
//! 2. References a registered reward account, AND
//! 3. Withdraws an amount equal to the registered balance EXACTLY.
//!
//! Two regimes are encoded:
//!
//! - **PV ≤ 10**: Bundles missing accounts and incomplete withdrawals into a
//!   single error variant. Reference: Haskell
//!   `Cardano.Ledger.Conway.Rules.Certs.conwayCertsTransition` /
//!   `withdrawalsThatDoNotDrainAccounts`.
//!
//! - **PV ≥ 11**: After
//!   `hardforkConwayMoveWithdrawalsAndDRepChecksToLedgerRule`, the check moves
//!   to the LEDGER rule and is split in two: missing accounts vs. incomplete
//!   withdrawals. Reference: Haskell
//!   `Cardano.Ledger.Conway.Rules.Ledger.testIncompleteAndMissingWithdrawals`.

use std::collections::BTreeMap;

use dugite_primitives::hash::Hash32;
use dugite_primitives::value::Lovelace;

/// Result of partitioning a transaction's withdrawals by failure mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawalSplit {
    /// Withdrawals whose reward account is unregistered or has the wrong
    /// network bit. Each entry is `(raw_addr_hex, supplied_amount)`.
    pub missing: Vec<(String, u64)>,
    /// Withdrawals whose reward account is registered but whose declared
    /// amount does not equal the registered balance. Each entry is
    /// `(raw_addr_hex, supplied_amount, expected_balance)`.
    pub incomplete: Vec<(String, u64, u64)>,
}

/// Check every withdrawal in `withdrawals` against the registered
/// `reward_accounts`, partitioning failures into "missing" vs. "incomplete".
///
/// Returns `None` when every withdrawal is well-formed and balanced.
///
/// Reference: Haskell `withdrawalsThatDoNotDrainAccounts` in
/// `Cardano.Ledger.Conway.Rules.Certs`.
pub fn withdrawals_that_do_not_drain_accounts(
    withdrawals: &BTreeMap<Vec<u8>, Lovelace>,
    network_id: u8,
    reward_accounts: &imbl::HashMap<Hash32, Lovelace>,
) -> Option<WithdrawalSplit> {
    let mut missing = Vec::new();
    let mut incomplete = Vec::new();

    for (addr_bytes, amount) in withdrawals {
        // Network: bit 0 of byte 0. Empty / malformed addresses are treated as
        // "missing" (they cannot reference any registered account).
        if addr_bytes.is_empty() || (addr_bytes[0] & 0x01) != network_id {
            missing.push((hex_encode(addr_bytes), amount.0));
            continue;
        }
        // Credential extraction matches `LedgerState::reward_account_to_hash`:
        // strip header byte, take 28-byte credential, zero-pad to Hash32, and
        // mark byte 28 = 0x01 for script credentials.
        let key = reward_account_to_hash32(addr_bytes);
        match reward_accounts.get(&key) {
            None => missing.push((hex_encode(addr_bytes), amount.0)),
            Some(balance) if balance.0 != amount.0 => {
                incomplete.push((hex_encode(addr_bytes), amount.0, balance.0));
            }
            _ => {}
        }
    }

    if missing.is_empty() && incomplete.is_empty() {
        None
    } else {
        Some(WithdrawalSplit {
            missing,
            incomplete,
        })
    }
}

/// Same conversion as `LedgerState::reward_account_to_hash`, repeated here
/// to avoid creating a public state ↔ validation dependency just for a
/// helper. Reward addresses are 29 bytes: 1-byte header + 28-byte credential.
fn reward_account_to_hash32(reward_account: &[u8]) -> Hash32 {
    let mut key_bytes = [0u8; 32];
    if reward_account.len() >= 29 {
        key_bytes[..28].copy_from_slice(&reward_account[1..29]);
        if reward_account[0] & 0x10 != 0 {
            key_bytes[28] = 0x01; // script credential
        }
    }
    Hash32::from_bytes(key_bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_addr(network_bit: u8, cred: u8) -> Vec<u8> {
        // 29 bytes: 0xe0/0xe1 (key reward addr) + 28-byte credential.
        let mut v = vec![0xe0 | (network_bit & 0x01)];
        v.extend(std::iter::repeat_n(cred, 28));
        v
    }

    fn script_addr(network_bit: u8, cred: u8) -> Vec<u8> {
        // 29 bytes: 0xf0/0xf1 (script reward addr) + 28-byte credential.
        let mut v = vec![0xf0 | (network_bit & 0x01)];
        v.extend(std::iter::repeat_n(cred, 28));
        v
    }

    fn make_key(addr: &[u8]) -> Hash32 {
        reward_account_to_hash32(addr)
    }

    #[test]
    fn test_no_bad_withdrawals_returns_none() {
        let mut withdrawals = BTreeMap::new();
        let addr = key_addr(1, 0xab);
        withdrawals.insert(addr.clone(), Lovelace(100));

        let mut accounts = imbl::HashMap::new();
        accounts.insert(make_key(&addr), Lovelace(100));

        assert_eq!(
            withdrawals_that_do_not_drain_accounts(&withdrawals, 1, &accounts),
            None
        );
    }

    #[test]
    fn test_unregistered_account_collected_as_missing() {
        let mut withdrawals = BTreeMap::new();
        let addr = key_addr(1, 0xcd);
        withdrawals.insert(addr.clone(), Lovelace(50));

        let accounts = imbl::HashMap::new(); // empty — addr is unregistered
        let split =
            withdrawals_that_do_not_drain_accounts(&withdrawals, 1, &accounts).expect("err");
        assert_eq!(split.missing, vec![(hex_encode(&addr), 50)]);
        assert!(split.incomplete.is_empty());
    }

    #[test]
    fn test_wrong_network_collected_as_missing() {
        let mut withdrawals = BTreeMap::new();
        let testnet_addr = key_addr(0, 0xde); // network bit 0
        withdrawals.insert(testnet_addr.clone(), Lovelace(75));

        // Even if the credential is "registered" with the same bytes, mainnet
        // (bit=1) won't see this as matching network.
        let mut accounts = imbl::HashMap::new();
        accounts.insert(make_key(&testnet_addr), Lovelace(75));

        let split =
            withdrawals_that_do_not_drain_accounts(&withdrawals, 1, &accounts).expect("err");
        assert_eq!(split.missing, vec![(hex_encode(&testnet_addr), 75)]);
        assert!(split.incomplete.is_empty());
    }

    #[test]
    fn test_amount_mismatch_collected_as_incomplete() {
        let mut withdrawals = BTreeMap::new();
        let addr = key_addr(1, 0x11);
        withdrawals.insert(addr.clone(), Lovelace(40));

        let mut accounts = imbl::HashMap::new();
        accounts.insert(make_key(&addr), Lovelace(100)); // expected 100, supplied 40

        let split =
            withdrawals_that_do_not_drain_accounts(&withdrawals, 1, &accounts).expect("err");
        assert!(split.missing.is_empty());
        assert_eq!(split.incomplete, vec![(hex_encode(&addr), 40, 100)]);
    }

    #[test]
    fn test_combined_missing_and_incomplete() {
        let mut withdrawals = BTreeMap::new();
        let addr_a = key_addr(1, 0x21); // unregistered → missing
        let addr_b = key_addr(1, 0x22); // mismatch → incomplete
        let addr_c = key_addr(1, 0x23); // ok
        withdrawals.insert(addr_a.clone(), Lovelace(10));
        withdrawals.insert(addr_b.clone(), Lovelace(20));
        withdrawals.insert(addr_c.clone(), Lovelace(30));

        let mut accounts = imbl::HashMap::new();
        accounts.insert(make_key(&addr_b), Lovelace(99));
        accounts.insert(make_key(&addr_c), Lovelace(30));

        let split =
            withdrawals_that_do_not_drain_accounts(&withdrawals, 1, &accounts).expect("err");
        assert_eq!(split.missing, vec![(hex_encode(&addr_a), 10)]);
        assert_eq!(split.incomplete, vec![(hex_encode(&addr_b), 20, 99)]);
    }

    #[test]
    fn test_script_credential_distinguished_from_key_credential() {
        // A key-cred reward addr and a script-cred reward addr that share the
        // same 28-byte credential bytes must hash to different Hash32 keys
        // (byte 28 differs). Registering only the key-cred should NOT satisfy
        // a withdrawal of the script-cred.
        let mut withdrawals = BTreeMap::new();
        let script = script_addr(1, 0x77);
        withdrawals.insert(script.clone(), Lovelace(5));

        let key = key_addr(1, 0x77);
        let mut accounts = imbl::HashMap::new();
        accounts.insert(make_key(&key), Lovelace(5));

        let split =
            withdrawals_that_do_not_drain_accounts(&withdrawals, 1, &accounts).expect("err");
        assert_eq!(split.missing, vec![(hex_encode(&script), 5)]);
    }
}
