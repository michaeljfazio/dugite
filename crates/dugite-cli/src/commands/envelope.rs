//! Strict CBOR unwrapping for text-envelope key payloads.
//!
//! A cardano-cli text envelope stores its key as `cborHex`: the key bytes
//! wrapped in a CBOR byte string. Unwrapping that wrapper with a *heuristic*
//! — "strip 2 bytes if the input is longer than 2", or "strip 1 byte if
//! `(b[0] & 0xe0) == 0x40`" — silently corrupts any payload that does not
//! actually carry the wrapper it guessed at.
//!
//! Concretely: a RAW 32-byte Ed25519 key whose first byte happens to land in
//! `0x40..=0x5f` (a 1-in-8 chance) matches the `& 0xe0` test, loses its first
//! byte, and then either fails a length check with a confusing error or — if
//! the caller has no length check — produces a wrong hash from wrong bytes.
//! The unconditional `[2..]` form is worse: it corrupts *every* unwrapped
//! payload, including correctly-formed raw keys.
//!
//! [`unwrap_key_bytes`] replaces those heuristics with an exact-match rule,
//! the same one #934 applied to `pool_id_from_cold_vkey`: strip a header only
//! when the bytes are *exactly* a CBOR byte string of the expected length,
//! and otherwise accept only an already-raw payload of that length. Anything
//! else is a hard error naming what was found.

use anyhow::{bail, Result};

/// Strip the text-envelope CBOR byte-string wrapper from `cbor`, returning
/// exactly `expected_len` key bytes.
///
/// Accepts precisely three encodings and rejects everything else:
///
/// | Input                                    | Condition            |
/// |------------------------------------------|----------------------|
/// | `0x58 <len> <payload>` (2-byte header)   | `24 <= len <= 255`   |
/// | `0x40\|<len> <payload>` (1-byte header)  | `len <= 23`          |
/// | `<payload>` (already raw)                | always               |
///
/// Cardano keys are 32 or 64 bytes, so in practice the wrapper is always the
/// `0x58` form — but the short form is accepted for completeness because it is
/// the canonical CBOR encoding for payloads of 23 bytes or fewer.
///
/// `what` names the key in error messages (e.g. `"VRF verification key"`).
pub fn unwrap_key_bytes<'a>(cbor: &'a [u8], expected_len: usize, what: &str) -> Result<&'a [u8]> {
    // Canonical 2-byte header: 0x58 <len>, for 24..=255 byte payloads.
    if (24..=255).contains(&expected_len)
        && cbor.len() == expected_len + 2
        && cbor[0] == 0x58
        && cbor[1] == expected_len as u8
    {
        return Ok(&cbor[2..]);
    }

    // Canonical 1-byte header: 0x40 | len, for payloads of 23 bytes or fewer.
    if expected_len <= 23 && cbor.len() == expected_len + 1 && cbor[0] == 0x40 | expected_len as u8
    {
        return Ok(&cbor[1..]);
    }

    // Already-raw payload — never strip anything, however the first byte looks.
    if cbor.len() == expected_len {
        return Ok(cbor);
    }

    bail!(
        "{what}: expected {expected_len} raw bytes or a CBOR byte string of \
         {expected_len} bytes, got {} bytes starting with {:02x?}",
        cbor.len(),
        &cbor[..cbor.len().min(2)]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped32(first: u8) -> Vec<u8> {
        let mut v = vec![0x58, 0x20];
        v.push(first);
        v.extend(std::iter::repeat_n(0xAB, 31));
        v
    }

    #[test]
    fn strips_canonical_two_byte_header() {
        let cbor = wrapped32(0x11);
        let out = unwrap_key_bytes(&cbor, 32, "test key").unwrap();
        assert_eq!(out.len(), 32);
        assert_eq!(out[0], 0x11);
    }

    #[test]
    fn strips_canonical_one_byte_header_for_short_payloads() {
        let mut cbor = vec![0x40 | 4];
        cbor.extend([1, 2, 3, 4]);
        assert_eq!(unwrap_key_bytes(&cbor, 4, "short").unwrap(), &[1, 2, 3, 4]);
    }

    /// The regression the heuristics caused: a raw 32-byte key whose first
    /// byte lands in 0x40..=0x5f must NOT lose a byte.
    #[test]
    fn raw_key_starting_in_the_bytes_major_range_is_untouched() {
        for first in [0x40u8, 0x4f, 0x58, 0x5a, 0x5f] {
            let mut raw = vec![first];
            raw.extend(std::iter::repeat_n(0xCD, 31));
            assert_eq!(raw.len(), 32);
            let out = unwrap_key_bytes(&raw, 32, "raw key").unwrap();
            assert_eq!(
                out,
                &raw[..],
                "raw key starting {first:#04x} must be returned verbatim"
            );
        }
    }

    /// A 34-byte payload that is NOT a 0x58 0x20 wrapper must be rejected
    /// rather than blindly stripped to 32 bytes.
    #[test]
    fn rejects_lookalike_wrapper() {
        let mut bogus = vec![0x59, 0x20];
        bogus.extend(std::iter::repeat_n(0xEE, 32));
        assert!(unwrap_key_bytes(&bogus, 32, "bogus").is_err());

        let mut wrong_len = vec![0x58, 0x1f];
        wrong_len.extend(std::iter::repeat_n(0xEE, 32));
        assert!(unwrap_key_bytes(&wrong_len, 32, "wrong len").is_err());
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(unwrap_key_bytes(&[0u8; 31], 32, "short").is_err());
        assert!(unwrap_key_bytes(&[0u8; 33], 32, "long").is_err());
        assert!(unwrap_key_bytes(&[], 32, "empty").is_err());
    }

    #[test]
    fn handles_64_byte_keys() {
        let mut cbor = vec![0x58, 0x40];
        cbor.extend(std::iter::repeat_n(0x77, 64));
        assert_eq!(unwrap_key_bytes(&cbor, 64, "ext key").unwrap().len(), 64);
    }
}

/// Resolve the target network the way cardano-cli does (#935 item 1).
///
/// cardano-cli takes `--mainnet | --testnet-magic NATURAL` (mutually
/// exclusive, one required) and, when neither is given, falls back to the
/// `CARDANO_NODE_NETWORK_ID` environment variable (`mainnet` or a magic
/// number). dugite additionally keeps its own `--network mainnet|testnet`
/// string flag, which has no cardano-cli counterpart.
///
/// Precedence: explicit flags first, then `--network`, then the environment
/// variable, then mainnet. A typo in any explicit source is a hard error —
/// never a silent testnet fallback (the #934 regression).
pub fn resolve_network(
    mainnet: bool,
    testnet_magic: Option<u32>,
    network: Option<&str>,
) -> Result<dugite_primitives::network::NetworkId> {
    use dugite_primitives::network::NetworkId;

    if mainnet && testnet_magic.is_some() {
        bail!("--mainnet and --testnet-magic are mutually exclusive");
    }
    if mainnet {
        return Ok(NetworkId::Mainnet);
    }
    if testnet_magic.is_some() {
        return Ok(NetworkId::Testnet);
    }

    if let Some(n) = network {
        return match n {
            "mainnet" => Ok(NetworkId::Mainnet),
            "testnet" | "testnet-magic" => Ok(NetworkId::Testnet),
            other => bail!(
                "invalid --network value \"{other}\": accepted values are \
                 \"mainnet\" and \"testnet\" (synonym: \"testnet-magic\")"
            ),
        };
    }

    match std::env::var("CARDANO_NODE_NETWORK_ID") {
        Ok(v) if v == "mainnet" => Ok(NetworkId::Mainnet),
        Ok(v) if v.parse::<u32>().is_ok() => Ok(NetworkId::Testnet),
        Ok(v) => bail!(
            "invalid CARDANO_NODE_NETWORK_ID value \"{v}\": expected \"mainnet\" \
             or a testnet magic number"
        ),
        Err(_) => Ok(NetworkId::Mainnet),
    }
}

/// Decode an inline verification key STRING: bech32 (`..._vk1...`) or raw hex.
///
/// cardano-cli exposes a `--<role>-verification-key STRING` alternative
/// alongside every `--<role>-verification-key-file FILE` (#935 items 1/3).
pub fn parse_inline_verification_key(s: &str, expected_len: usize, what: &str) -> Result<Vec<u8>> {
    if let Ok((_hrp, data)) = bech32::decode(s) {
        if data.len() != expected_len {
            bail!(
                "{what}: bech32 payload is {} bytes, expected {expected_len}",
                data.len()
            );
        }
        return Ok(data);
    }
    let bytes = hex::decode(s.trim())
        .map_err(|e| anyhow::anyhow!("{what}: not valid bech32 or hex ({e})"))?;
    // Accept the CBOR-wrapped hex form too, under the same strict rule.
    Ok(unwrap_key_bytes(&bytes, expected_len, what)?.to_vec())
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use dugite_primitives::network::NetworkId;

    /// Guard: these tests mutate a process-global env var.
    fn with_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("CARDANO_NODE_NETWORK_ID").ok();
        match value {
            Some(v) => std::env::set_var("CARDANO_NODE_NETWORK_ID", v),
            None => std::env::remove_var("CARDANO_NODE_NETWORK_ID"),
        }
        let out = f();
        match prev {
            Some(v) => std::env::set_var("CARDANO_NODE_NETWORK_ID", v),
            None => std::env::remove_var("CARDANO_NODE_NETWORK_ID"),
        }
        out
    }

    #[test]
    fn explicit_flags_win() {
        assert_eq!(
            resolve_network(true, None, None).unwrap(),
            NetworkId::Mainnet
        );
        assert_eq!(
            resolve_network(false, Some(2), None).unwrap(),
            NetworkId::Testnet
        );
        assert!(resolve_network(true, Some(2), None).is_err());
    }

    #[test]
    fn network_string_is_honoured_and_typos_hard_error() {
        assert_eq!(
            resolve_network(false, None, Some("mainnet")).unwrap(),
            NetworkId::Mainnet
        );
        assert_eq!(
            resolve_network(false, None, Some("testnet")).unwrap(),
            NetworkId::Testnet
        );
        // #934: a typo must never silently become testnet.
        assert!(resolve_network(false, None, Some("mainnnet")).is_err());
    }

    #[test]
    fn env_var_is_the_fallback_and_flags_override_it() {
        with_env(Some("2"), || {
            assert_eq!(
                resolve_network(false, None, None).unwrap(),
                NetworkId::Testnet,
                "CARDANO_NODE_NETWORK_ID=2 must select testnet"
            );
            // An explicit flag outranks the environment.
            assert_eq!(
                resolve_network(true, None, None).unwrap(),
                NetworkId::Mainnet
            );
        });
        with_env(Some("mainnet"), || {
            assert_eq!(
                resolve_network(false, None, None).unwrap(),
                NetworkId::Mainnet
            );
        });
        with_env(Some("not-a-network"), || {
            assert!(resolve_network(false, None, None).is_err());
        });
        with_env(None, || {
            assert_eq!(
                resolve_network(false, None, None).unwrap(),
                NetworkId::Mainnet,
                "no flags and no env => mainnet"
            );
        });
    }

    #[test]
    fn inline_key_accepts_hex_bech32_and_cbor_wrapped() {
        let raw = [0x42u8; 32];

        let hex_form = hex::encode(raw);
        assert_eq!(
            parse_inline_verification_key(&hex_form, 32, "k").unwrap(),
            raw.to_vec()
        );

        let mut wrapped = vec![0x58, 0x20];
        wrapped.extend(raw);
        assert_eq!(
            parse_inline_verification_key(&hex::encode(&wrapped), 32, "k").unwrap(),
            raw.to_vec()
        );

        let b32 =
            bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("addr_vk").unwrap(), &raw).unwrap();
        assert_eq!(
            parse_inline_verification_key(&b32, 32, "k").unwrap(),
            raw.to_vec()
        );
    }

    #[test]
    fn inline_key_rejects_wrong_length_and_garbage() {
        assert!(parse_inline_verification_key("nonsense!!", 32, "k").is_err());
        assert!(parse_inline_verification_key(&hex::encode([0u8; 31]), 32, "k").is_err());
    }
}
