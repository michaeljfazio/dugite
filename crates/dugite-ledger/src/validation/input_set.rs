//! A `Set TxIn` body field (inputs, collateral inputs), as Haskell presents it to index-based consumers.
//!
//! `TxBody.inputs` and `TxBody.collateral` are `Set TxIn` in cardano-ledger. Every consumer that
//! addresses an input by POSITION (redeemer pointers, `redeemersNeeded`,
//! `hasExactSetOfRedeemers`, the Plutus `txInfoInputs` list) sees the sorted,
//! DEDUPLICATED set — never the wire array.
//!
//! The wire decoder keeps `body.inputs` as a plain `Vec` in wire order (the raw
//! order is needed for hashing and re-serialisation), so a duplicate TxIn in a
//! pre-Conway body survives decoding. The Haskell decoder
//! (`cardano-ledger-binary` `Decoder.hs` `decodeSet`) collapses it with
//! `Set.fromList` for protocol versions below 9 and rejects it with
//! `decodeSetEnforceNoDuplicates` from PV 9. Indexing the raw `Vec` therefore
//! shifts every later input by one position and reports a false "script-locked
//! input has no matching Spend redeemer" for a tx Haskell accepted (preview
//! Babbage tx `4c4afd46…77b6`, block 2,160,646).
//!
//! At PV >= 9 a duplicate never reaches these consumers in a tx that can pass,
//! so deduplicating here changes nothing for it; the helper is therefore
//! protocol-version independent, exactly like
//! `dugite_uplc::tx_info_populate::sort_inputs`, which builds the Plutus
//! `ScriptContext` from the same view. Who rejects the duplicate differs per
//! field: `phase1` Rule 1b checks ONLY `body.inputs`, while a duplicate
//! collateral or reference input at PV >= 9 is rejected only by the Conway wire
//! decoder (`era_conway.rs` `read_set_strict`, keys 13 and 18). A tx built in
//! memory with a duplicate collateral input therefore gets no PV >= 9
//! `DuplicateInput` from the validator; the wire decoder is the gate.

use dugite_primitives::transaction::TransactionInput;

/// The members of a `Set TxIn` body field in canonical `Set TxIn` order: ascending by
/// `(transaction_id, index)`, each distinct input exactly once.
///
/// The returned position of an input is the redeemer index Haskell assigns it
/// (`Set.lookupIndex` / `Set.elemAt`).
pub(crate) fn tx_in_set(inputs: &[TransactionInput]) -> Vec<&TransactionInput> {
    let mut sorted: Vec<&TransactionInput> = inputs.iter().collect();
    sorted.sort_by(|a, b| {
        a.transaction_id
            .cmp(&b.transaction_id)
            .then(a.index.cmp(&b.index))
    });
    sorted.dedup();
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_primitives::hash::Hash32;

    fn txin(id: u8, index: u32) -> TransactionInput {
        TransactionInput {
            transaction_id: Hash32::from_bytes([id; 32]),
            index,
        }
    }

    #[test]
    fn sorts_and_collapses_duplicates() {
        let inputs = vec![txin(9, 0), txin(3, 1), txin(9, 0), txin(3, 0), txin(3, 1)];
        let set = tx_in_set(&inputs);
        let got: Vec<(u8, u32)> = set
            .iter()
            .map(|i| (i.transaction_id.as_bytes()[0], i.index))
            .collect();
        assert_eq!(got, vec![(3, 0), (3, 1), (9, 0)]);
    }

    #[test]
    fn empty_and_singleton() {
        assert!(tx_in_set(&[]).is_empty());
        assert_eq!(tx_in_set(&[txin(1, 7)]).len(), 1);
    }
}
