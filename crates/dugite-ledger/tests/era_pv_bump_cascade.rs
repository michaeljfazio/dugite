//! Integration test: era-boundary protocol-version (PV) non-mutation (issues #615, #626, #630).
//!
//! Haskell's HFC carry-forward semantics: `upgradeShelleyPParams`,
//! `upgradeAllegraPParams`, `upgradeMaryPParams`, `upgradeAlonzoPParams`,
//! `upgradeBabbagePParams` all carry `protocolVersion` forward verbatim via
//! `coerce` (zero-cost type coercion). PV advances are driven exclusively by:
//!
//! - **Byron→Shelley**: PV from `shelley-genesis.json::protocolParams::protocolVersion`.
//! - **Shelley/Allegra/Mary/Alonzo intra-era**: PPUP (pre-Conway protocol-update
//!   proposals), decoded via tx body key 6 (fixed in #624).
//! - **Alonzo→Babbage / Babbage→Conway**: PPUP / HardForkInitiation gov action.
//!
//! `on_era_transition` for ALL eras is a NO-OP with respect to PV. Bumping
//! there would race ahead of `prevPParams` capture and break
//! `hardforkBabbageForgoRewardPrefilter` (root cause of #626).
//!
//! This synthetic test exercises the full Byron→Conway cascade with no PPUP
//! applied, so PV stays constant at whatever the genesis seeded. In production,
//! PPUP proposals fired in each era drive the expected bumps.
//!
//! Reference: cardano-ledger Haskell source — `upgradeShelleyPParams` et al.

use dugite_ledger::state::{BlockValidationMode, LedgerState};
use dugite_primitives::address::{Address, ByronAddress, EnterpriseAddress};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::network::NetworkId;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::{BlockNo, SlotNo};
use dugite_primitives::transaction::{
    OutputDatum, Transaction, TransactionInput, TransactionOutput,
};
use dugite_primitives::value::{Lovelace, Value};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a minimal block carrying a single value-conserving transaction. We
/// reuse the same input/output per era and seed the UTxO before each apply so
/// we don't accumulate state we don't care about for this test.
fn build_era_block(
    era: Era,
    slot: u64,
    block_no: u64,
    pv_major: u64,
    prev_hash: Hash32,
) -> (Block, TransactionInput) {
    let input = TransactionInput {
        transaction_id: Hash32::from_bytes([block_no as u8; 32]),
        index: 0,
    };
    let output = TransactionOutput {
        address: if era == Era::Byron {
            Address::Byron(ByronAddress {
                payload: vec![1u8; 32],
            })
        } else {
            Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0xABu8; 28])),
            })
        },
        value: Value {
            coin: Lovelace(1_000_000),
            multi_asset: Default::default(),
        },
        datum: OutputDatum::None,
        script_ref: None,
        is_legacy: era == Era::Byron,
        raw_cbor: None,
    };
    let mut tx = Transaction::empty_with_hash(Hash32::from_bytes(
        [(block_no as u8).wrapping_add(0x80); 32],
    ));
    tx.body.inputs = vec![input.clone()];
    tx.body.outputs = vec![output];
    tx.body.fee = Lovelace(0);

    let block = Block {
        header: BlockHeader {
            header_hash: Hash32::from_bytes({
                let mut b = [0u8; 32];
                b[..8].copy_from_slice(&block_no.to_be_bytes());
                b
            }),
            prev_hash,
            issuer_vkey: vec![],
            vrf_vkey: vec![],
            vrf_result: VrfOutput {
                output: vec![],
                proof: vec![],
            },
            nonce_vrf_output: vec![],
            nonce_vrf_proof: vec![],
            prev_nonce: None,
            raw_header_body: None,
            block_number: BlockNo(block_no),
            slot: SlotNo(slot),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: OperationalCert {
                hot_vkey: vec![],
                sequence_number: 0,
                kes_period: 0,
                sigma: vec![],
            },
            protocol_version: ProtocolVersion {
                major: pv_major,
                minor: 0,
            },
            kes_signature: vec![],
        },
        transactions: vec![tx],
        era,
        raw_cbor: None,
        byron: None,
    };
    (block, input)
}

/// Seed the UTxO and apply the block in `ApplyOnly` mode, which exercises the
/// era-transition dispatch (Step 2 of `LedgerState::apply_block`).
fn apply_era_block(state: &mut LedgerState, era: Era, slot: u64, block_no: u64, pv_major: u64) {
    let prev_hash = state.tip.point.hash().copied().unwrap_or(Hash32::ZERO);
    let (block, input) = build_era_block(era, slot, block_no, pv_major, prev_hash);

    // Seed UTxO entry so the tx can spend it. For Byron we use a legacy Byron
    // output; for Shelley+ we use an Enterprise address.
    let utxo_output = if era == Era::Byron {
        TransactionOutput {
            address: Address::Byron(ByronAddress {
                payload: vec![0u8; 32],
            }),
            value: Value {
                coin: Lovelace(1_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: true,
            raw_cbor: None,
        }
    } else {
        TransactionOutput {
            address: Address::Enterprise(EnterpriseAddress {
                network: NetworkId::Mainnet,
                payment: Credential::VerificationKey(Hash28::from_bytes([0xABu8; 28])),
            }),
            value: Value {
                coin: Lovelace(1_000_000),
                multi_asset: Default::default(),
            },
            datum: OutputDatum::None,
            script_ref: None,
            is_legacy: false,
            raw_cbor: None,
        }
    };
    state.utxo.utxo_set.insert(input, utxo_output);

    state
        .apply_block(&block, BlockValidationMode::ApplyOnly)
        .unwrap_or_else(|e| {
            panic!(
                "apply_block({:?}, slot={}, block_no={}) failed: {:?}",
                era, slot, block_no, e
            )
        });
}

// ─────────────────────────────────────────────────────────────────────────────
// Test
// ─────────────────────────────────────────────────────────────────────────────

/// Drive a synthetic Byron→Shelley→Allegra→Mary→Alonzo→Babbage→Conway sequence
/// and assert that `on_era_transition` NEVER mutates `curPParams.protocol_version`.
/// With no PPUP applied, PV stays constant at the genesis seed throughout.
///
/// Regression for issues #615, #626, #630.
#[test]
fn era_boundary_pv_cascade_not_mutated_by_on_era_transition() {
    // Seed with PV=2 (matching what shelley-genesis.json ships on mainnet).
    // All era transitions in this test must be no-ops for PV.
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = 2;
    params.protocol_version_minor = 0;
    let mut state = LedgerState::new(params);
    state.tip.block_number = BlockNo(0);
    assert_eq!(
        state.epochs.protocol_params.protocol_version_major, 2,
        "Test prelude must seed PV2 (sanity)",
    );

    // (era, slot, block_no, header_pv_major)
    // expected_pv is always 2 — on_era_transition is a no-op for PV in all eras.
    let steps = [
        (Era::Byron, 100, 1, 2),
        (Era::Shelley, 200, 2, 2),
        (Era::Allegra, 300, 3, 2),
        (Era::Mary, 400, 4, 2),
        (Era::Alonzo, 500, 5, 2),
        (Era::Babbage, 600, 6, 2),
        (Era::Conway, 700, 7, 2),
    ];

    for (era, slot, block_no, header_pv) in steps {
        apply_era_block(&mut state, era, slot, block_no, header_pv);
        assert_eq!(
            state.epochs.protocol_params.protocol_version_major, 2,
            "After entering {:?} (block {}, slot {}): on_era_transition must NOT \
             mutate curPParams.pv — PPUP drives all bumps (issues #626/#630)",
            era, block_no, slot,
        );
        assert_eq!(state.era, era, "LedgerState.era must track block.era");
    }
}
