//! Fuzz target for BootstrapWitness field-size pre-flight checks (issue #546 F1/F4).
//!
//! Background: Byron extended keys are 64 bytes (scalar || extension). Prior to the #546
//! fix, the `HasWitnessFields` impl on `BootstrapWitness` returned the 64-byte vkey,
//! triggering the `len() != 32` guard in `verify_single_witness` and silently skipping ALL
//! bootstrap witness verification — zero cryptographic verification on Byron-era inputs.
//!
//! The fix introduces `verify_single_bootstrap_witness` with:
//!   - Pre-flight: vkey=64, sig=64, chain_code=32 (malformed → hard reject)
//!   - Ed25519 verify over vkey[0..32] (the scalar part of the extended key)
//!   - Address-binding: root = blake2b_224(sha3_256(CBOR([0, [0, vkey64], attrs])))
//!
//! This fuzz target stresses the Phase-1 validation pipeline with arbitrary bootstrap
//! witness field sizes to ensure:
//!   1. No panics on any attacker-chosen field lengths.
//!   2. Malformed witnesses (wrong sizes) are REJECTED, never silently skipped.
//!   3. The invariant holds across all size combinations on the length lattice.
//!
//! Byte layout:
//!   [0..2]   = vkey length (mod 513) — canonical is 64
//!   [2..4]   = sig length (mod 513)  — canonical is 64
//!   [4..6]   = chain_code length (mod 513) — canonical is 32
//!   [6..8]   = attrs length (mod 513) — variable OK
//!   [8..]    = content bytes (looped to fill each field)
//!
//! Run with: cargo +nightly fuzz run fuzz_bootstrap_witness_sizes -- -max_total_time=60

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;
use std::collections::HashMap;

use dugite_ledger::utxo::UtxoLookup;
use dugite_ledger::validation::validate_transaction;
use dugite_primitives::address::{Address, ByronAddress};
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{
    BootstrapWitness, OutputDatum, Transaction, TransactionBody, TransactionInput,
    TransactionOutput, TransactionWitnessSet,
};
use dugite_primitives::value::{Lovelace, Value};

fn read_u16_le(slice: &[u8]) -> u16 {
    if slice.len() < 2 {
        return 0;
    }
    u16::from_le_bytes([slice[0], slice[1]])
}

struct FuzzUtxo(HashMap<TransactionInput, TransactionOutput>);

impl UtxoLookup for FuzzUtxo {
    fn lookup(&self, input: &TransactionInput) -> Option<TransactionOutput> {
        self.0.get(input).cloned()
    }
}

/// Minimal valid Byron address payload (no embedded root we care about).
fn simple_byron_payload() -> Vec<u8> {
    vec![0x82u8, 0x00u8, 0x01u8]
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 9 {
        return;
    }

    let vkey_len = (read_u16_le(&data[0..2]) % 513) as usize;
    let sig_len = (read_u16_le(&data[2..4]) % 513) as usize;
    let chain_code_len = (read_u16_le(&data[4..6]) % 513) as usize;
    let attrs_len = (read_u16_le(&data[6..8]) % 513) as usize;
    let filler = &data[8..];

    // Build byte fields of the requested size (loop filler bytes as content).
    let make_bytes = |n: usize| -> Vec<u8> {
        if n == 0 || filler.is_empty() {
            vec![0u8; n]
        } else {
            (0..n).map(|i| filler[i % filler.len()]).collect()
        }
    };

    let bw = BootstrapWitness {
        vkey: make_bytes(vkey_len),
        signature: make_bytes(sig_len),
        chain_code: make_bytes(chain_code_len),
        attributes: make_bytes(attrs_len),
    };

    let input = TransactionInput {
        transaction_id: Hash32::from_bytes([0xAAu8; 32]),
        index: 0,
    };

    // UTxO entry with a simple Byron address.
    let utxo_output = TransactionOutput {
        address: Address::Byron(ByronAddress {
            payload: simple_byron_payload(),
        }),
        value: Value::lovelace(10_000_000),
        datum: OutputDatum::None,
        script_ref: None,
        is_legacy: false,
        raw_cbor: None,
    };

    let mut utxo_map = HashMap::new();
    utxo_map.insert(input.clone(), utxo_output);

    let tx = Transaction {
        era: Era::Conway,
        hash: Hash32::ZERO,
        body: TransactionBody {
            inputs: vec![input],
            outputs: vec![TransactionOutput {
                address: Address::Byron(ByronAddress {
                    payload: simple_byron_payload(),
                }),
                value: Value::lovelace(9_800_000),
                datum: OutputDatum::None,
                script_ref: None,
                is_legacy: false,
                raw_cbor: None,
            }],
            fee: Lovelace(200_000),
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
        },
        witness_set: TransactionWitnessSet {
            vkey_witnesses: vec![],
            native_scripts: vec![],
            bootstrap_witnesses: vec![bw],
            plutus_v1_scripts: vec![],
            plutus_v2_scripts: vec![],
            plutus_v3_scripts: vec![],
            plutus_data: vec![],
            redeemers: vec![],
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            pallas_script_data_hash: None,
        },
        is_valid: true,
        auxiliary_data: None,
        raw_cbor: None,
        raw_body_cbor: None,
        raw_witness_cbor: None,
    };

    let params = ProtocolParameters::mainnet_defaults();
    let utxo_set = FuzzUtxo(utxo_map);

    // Invariant 1: MUST NOT panic regardless of field sizes.
    let result = validate_transaction(&tx, &utxo_set, &params, 1_000_000, 300, None);

    // Invariant 2: Malformed sizes → MUST be rejected, never silently skipped.
    // "Silent skip" was the pre-fix bug: 64-byte vkey triggers len()!=32 guard → Ok.
    let sizes_malformed = vkey_len != 64 || sig_len != 64 || chain_code_len != 32;
    if sizes_malformed {
        assert!(
            result.is_err(),
            "malformed bootstrap witness MUST be rejected (not silently skipped): \
             vkey={vkey_len}, sig={sig_len}, chain_code={chain_code_len}"
        );
    }
    // When sizes are canonical (64/64/32) the validator may still reject (bad sig,
    // address-binding mismatch with zero-root UTxO) — we only assert no-panic.
});
