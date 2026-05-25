//! Pattern-matching helpers used by `SearchUtxos` (`UtxoPredicate`) and
//! `WatchTx` (`TxPredicate`).
//!
//! The matchers consume the *parsed* `dugite_primitives` shapes — not
//! the proto wire form — so the same logic serves both the in-memory
//! UTxO post-filter path and the streaming tx-event path.
//!
//! Pattern semantics follow the utxorpc spec:
//!   * `match` — leaf positive predicate.
//!   * `not[i]` — predicate is true if **every** sub-pattern is false.
//!   * `all_of[i]` — predicate is true if **every** sub-pattern is true.
//!   * `any_of[i]` — predicate is true if **any** sub-pattern is true.
//!
//! An empty (defaulted) predicate matches everything — clients use
//! `UtxoPredicate::default()` as the "all UTxOs" wildcard. We mirror
//! that semantics here so an unset `predicate` field in a request
//! matches every candidate.

use dugite_primitives::transaction::Transaction;

use crate::proto::v1beta;

/// Top-level entry point for `SearchUtxos`: evaluate `predicate`
/// against the supplied `TransactionOutput`. A `None` predicate matches
/// every output.
pub fn matches_utxo_predicate(
    predicate: Option<&v1beta::query::UtxoPredicate>,
    output: &dugite_primitives::transaction::TransactionOutput,
) -> bool {
    match predicate {
        None => true,
        Some(p) => eval_utxo_predicate(p, output),
    }
}

fn eval_utxo_predicate(
    p: &v1beta::query::UtxoPredicate,
    output: &dugite_primitives::transaction::TransactionOutput,
) -> bool {
    // `match` leaf — when present must be satisfied. An *absent* match
    // with no boolean combinators is the wildcard "matches everything".
    let leaf_ok = match p.r#match.as_ref() {
        Some(any) => match any.utxo_pattern.as_ref() {
            Some(v1beta::query::any_utxo_pattern::UtxoPattern::Cardano(tx_out_pat)) => {
                matches_tx_output_pattern(tx_out_pat, output)
            }
            None => true,
        },
        None => true,
    };
    if !leaf_ok {
        return false;
    }
    // `all_of` — every sub-predicate must hold.
    for sub in &p.all_of {
        if !eval_utxo_predicate(sub, output) {
            return false;
        }
    }
    // `any_of` — at least one must hold (when the list is non-empty).
    if !p.any_of.is_empty() && !p.any_of.iter().any(|s| eval_utxo_predicate(s, output)) {
        return false;
    }
    // `not` — every sub-predicate must be false.
    for sub in &p.not {
        if eval_utxo_predicate(sub, output) {
            return false;
        }
    }
    true
}

/// Match a single `TxOutputPattern` (address + asset) against the
/// supplied output.
pub fn matches_tx_output_pattern(
    pat: &v1beta::cardano::TxOutputPattern,
    output: &dugite_primitives::transaction::TransactionOutput,
) -> bool {
    if let Some(addr) = pat.address.as_ref() {
        if !matches_address_pattern(addr, &output.address) {
            return false;
        }
    }
    if let Some(asset) = pat.asset.as_ref() {
        if !matches_asset_pattern(asset, &output.value) {
            return false;
        }
    }
    true
}

/// Match an `AddressPattern` against a parsed address.
///
/// The three optional bytes fields combine as AND: every set field
/// must match. `exact_address` is byte-equal to the address's
/// canonical wire form; `payment_part` / `delegation_part` are the
/// 28-byte hash of the corresponding credential (key OR script — the
/// spec does not distinguish).
pub fn matches_address_pattern(
    pat: &v1beta::cardano::AddressPattern,
    addr: &dugite_primitives::address::Address,
) -> bool {
    if let Some(ea) = pat.exact_address.as_ref() {
        if &addr.to_bytes() != ea {
            return false;
        }
    }
    if let Some(pp) = pat.payment_part.as_ref() {
        let Some(cred) = addr.payment_credential() else {
            return false;
        };
        if !hash28_matches(pp, cred.to_hash().as_bytes()) {
            return false;
        }
    }
    if let Some(dp) = pat.delegation_part.as_ref() {
        use dugite_primitives::credentials::StakeReference;
        match addr.stake_reference() {
            StakeReference::StakeCredential(c) => {
                if !hash28_matches(dp, c.to_hash().as_bytes()) {
                    return false;
                }
            }
            StakeReference::Pointer(_) | StakeReference::Null => return false,
        }
    }
    true
}

fn hash28_matches(want: &[u8], have: &[u8]) -> bool {
    // The proto-side field is documented as bytes; clients may pass 28
    // or 32 bytes (the latter is the zero-padded-to-32 form some tools
    // emit). Compare the leading 28 in either case.
    let want_slice = if want.len() >= 28 { &want[..28] } else { want };
    let have_slice = if have.len() >= 28 { &have[..28] } else { have };
    want_slice == have_slice
}

fn matches_asset_pattern(
    pat: &v1beta::cardano::AssetPattern,
    value: &dugite_primitives::value::Value,
) -> bool {
    // Empty asset pattern (no policy / no name) matches every output —
    // the spec treats it as a wildcard within the `asset` slot.
    let Some(policy) = pat.policy_id.as_ref() else {
        return true;
    };
    if policy.len() < 28 {
        return false;
    }
    use dugite_primitives::hash::Hash28;
    let mut pid = [0u8; 28];
    pid.copy_from_slice(&policy[..28]);
    let policy_id = Hash28::from_bytes(pid);
    let Some(assets) = value.multi_asset.get(&policy_id) else {
        return false;
    };
    if let Some(name) = pat.asset_name.as_ref() {
        // Look up the exact asset name; absent → no match.
        assets
            .iter()
            .any(|(an, _)| an.0.as_slice() == name.as_slice())
    } else {
        // Policy-only — any non-empty asset under this policy matches.
        !assets.is_empty()
    }
}

// ─── WatchTx tx-level matcher ────────────────────────────────────────────

/// Top-level entry point for `WatchTx`: evaluate `predicate` against
/// the supplied parsed transaction. `None` predicate matches every tx.
pub fn matches_tx_predicate(
    predicate: Option<&v1beta::watch::TxPredicate>,
    tx: &Transaction,
) -> bool {
    match predicate {
        None => true,
        Some(p) => eval_tx_predicate(p, tx),
    }
}

fn eval_tx_predicate(p: &v1beta::watch::TxPredicate, tx: &Transaction) -> bool {
    // `match` leaf.
    let leaf_ok = match p.r#match.as_ref() {
        Some(any) => match any.chain.as_ref() {
            Some(v1beta::watch::any_chain_tx_pattern::Chain::Cardano(tx_pat)) => {
                matches_cardano_tx_pattern(tx_pat, tx)
            }
            None => true,
        },
        None => true,
    };
    if !leaf_ok {
        return false;
    }
    for sub in &p.all_of {
        if !eval_tx_predicate(sub, tx) {
            return false;
        }
    }
    if !p.any_of.is_empty() && !p.any_of.iter().any(|s| eval_tx_predicate(s, tx)) {
        return false;
    }
    for sub in &p.not {
        if eval_tx_predicate(sub, tx) {
            return false;
        }
    }
    true
}

fn matches_cardano_tx_pattern(pat: &v1beta::cardano::TxPattern, tx: &Transaction) -> bool {
    // Each populated sub-pattern is an AND clause: every populated
    // field must hold against at least one matching element of the tx.
    if let Some(produces) = pat.produces.as_ref() {
        if !tx
            .body
            .outputs
            .iter()
            .any(|o| matches_tx_output_pattern(produces, o))
        {
            return false;
        }
    }
    if let Some(has_addr) = pat.has_address.as_ref() {
        let any_match = tx
            .body
            .outputs
            .iter()
            .any(|o| matches_address_pattern(has_addr, &o.address));
        if !any_match {
            return false;
        }
    }
    if let Some(moves) = pat.moves_asset.as_ref() {
        let any_match = tx
            .body
            .outputs
            .iter()
            .any(|o| matches_asset_pattern(moves, &o.value));
        if !any_match {
            return false;
        }
    }
    if let Some(mints) = pat.mints_asset.as_ref() {
        let any_match = tx_mint_contains_asset(tx, mints);
        if !any_match {
            return false;
        }
    }
    // `consumes` requires resolved-input data we don't have on the
    // mempool path. Tx-level WatchTx is "outputs + mint" today; the
    // input-side match is documented in `docs/src/running/utxo-rpc.md`
    // as the WatchTx limitation.
    true
}

fn tx_mint_contains_asset(tx: &Transaction, pat: &v1beta::cardano::AssetPattern) -> bool {
    let Some(policy) = pat.policy_id.as_ref() else {
        return !tx.body.mint.is_empty();
    };
    if policy.len() < 28 {
        return false;
    }
    use dugite_primitives::hash::Hash28;
    let mut pid = [0u8; 28];
    pid.copy_from_slice(&policy[..28]);
    let policy_id = Hash28::from_bytes(pid);
    let Some(entries) = tx.body.mint.get(&policy_id) else {
        return false;
    };
    if let Some(name) = pat.asset_name.as_ref() {
        entries
            .iter()
            .any(|(an, _)| an.0.as_slice() == name.as_slice())
    } else {
        !entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::address::{Address, ByronAddress};
    use dugite_primitives::hash::Hash;
    use dugite_primitives::transaction::TransactionOutput;
    use dugite_primitives::value::{Lovelace, Value};

    fn dummy_output(addr: Address, value: Value) -> TransactionOutput {
        TransactionOutput {
            address: addr,
            value,
            datum: dugite_primitives::transaction::OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    }

    fn byron_addr(payload: u8) -> Address {
        Address::Byron(ByronAddress {
            payload: vec![payload; 30],
        })
    }

    #[test]
    fn wildcard_predicate_matches() {
        let out = dummy_output(byron_addr(0x00), Value::lovelace(0));
        assert!(matches_utxo_predicate(None, &out));
        assert!(matches_utxo_predicate(
            Some(&v1beta::query::UtxoPredicate::default()),
            &out
        ));
    }

    #[test]
    fn not_inverts_match() {
        let out = dummy_output(byron_addr(0x00), Value::lovelace(0));
        let exact = byron_addr(0x11).to_bytes();
        let inner = v1beta::query::UtxoPredicate {
            r#match: Some(v1beta::query::AnyUtxoPattern {
                utxo_pattern: Some(v1beta::query::any_utxo_pattern::UtxoPattern::Cardano(
                    v1beta::cardano::TxOutputPattern {
                        address: Some(v1beta::cardano::AddressPattern {
                            exact_address: Some(exact),
                            payment_part: None,
                            delegation_part: None,
                        }),
                        asset: None,
                    },
                )),
            }),
            not: vec![],
            all_of: vec![],
            any_of: vec![],
        };
        let neg = v1beta::query::UtxoPredicate {
            not: vec![inner],
            ..Default::default()
        };
        assert!(matches_utxo_predicate(Some(&neg), &out));
    }

    #[test]
    fn any_of_short_circuits() {
        let out = dummy_output(byron_addr(0xAA), Value::lovelace(0));
        let want = byron_addr(0xAA).to_bytes();
        let nope = byron_addr(0x55).to_bytes();
        let p = v1beta::query::UtxoPredicate {
            any_of: vec![
                v1beta::query::UtxoPredicate {
                    r#match: Some(v1beta::query::AnyUtxoPattern {
                        utxo_pattern: Some(v1beta::query::any_utxo_pattern::UtxoPattern::Cardano(
                            v1beta::cardano::TxOutputPattern {
                                address: Some(v1beta::cardano::AddressPattern {
                                    exact_address: Some(nope),
                                    payment_part: None,
                                    delegation_part: None,
                                }),
                                asset: None,
                            },
                        )),
                    }),
                    ..Default::default()
                },
                v1beta::query::UtxoPredicate {
                    r#match: Some(v1beta::query::AnyUtxoPattern {
                        utxo_pattern: Some(v1beta::query::any_utxo_pattern::UtxoPattern::Cardano(
                            v1beta::cardano::TxOutputPattern {
                                address: Some(v1beta::cardano::AddressPattern {
                                    exact_address: Some(want),
                                    payment_part: None,
                                    delegation_part: None,
                                }),
                                asset: None,
                            },
                        )),
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(matches_utxo_predicate(Some(&p), &out));
    }

    #[test]
    fn asset_pattern_policy_only() {
        let mut policy_assets = std::collections::BTreeMap::new();
        let policy = Hash::<28>::from_bytes([0xAB; 28]);
        let asset_name = dugite_primitives::value::AssetName::new(vec![0x01]).unwrap();
        policy_assets.insert(asset_name, 7u64);
        let mut ma = std::collections::BTreeMap::new();
        ma.insert(policy, policy_assets);
        let value = Value {
            coin: Lovelace(0),
            multi_asset: ma,
        };
        let out = dummy_output(byron_addr(0x00), value);
        let pat = v1beta::cardano::AssetPattern {
            policy_id: Some(vec![0xAB; 28]),
            asset_name: None,
        };
        assert!(matches_asset_pattern(&pat, &out.value));
    }
}
