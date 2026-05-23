//! Integration test: era-boundary protocol-version bumps (issue #615).
//!
//! In cardano-node the Hard Fork Combinator (HFC) era-crossing tick writes the
//! new era's initial protocol version into `curPParams`. Dugite has no
//! separate HFC layer — era transitions are dispatched entirely in the ledger
//! crate via `block.era`, so each era's `on_era_transition` must replicate the
//! HFC's PV write.
//!
//! This test walks a synthetic chain from Byron through Conway and asserts
//! that `epochs.protocol_params.protocol_version_major` matches the canonical
//! "first-block-of-each-era" PV at each boundary:
//!
//! | Era      | PV at HFC activation |
//! |----------|----------------------|
//! | Byron    | 1 (`mainnet_defaults` seeds at 1) |
//! | Shelley  | 2                    |
//! | Allegra  | 3                    |
//! | Mary     | 4                    |
//! | Alonzo   | 5                    |
//! | Babbage  | 7                    |
//! | Conway   | 9                    |
//!
//! (Mary→Alonzo lands at 5; the intra-era PV6 / PV8 / PV10 bumps are
//! subsequent ParameterChange proposals and continue to flow through the
//! normal PPUP path — we do not test those here.)
//!
//! Reference: cardano-ledger wiki, "First Block of Each Era".

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

/// Drive a synthetic from-genesis sequence Byron→Shelley→Allegra→Mary→Alonzo
/// →Babbage→Conway and assert that `protocol_version_major` matches the
/// canonical first-block-of-each-era PV at every boundary. Pre-#615 this
/// regression test fails at the Shelley/Allegra/Mary/Alonzo/Babbage steps
/// because dugite never bumped the PV outside Conway.
#[test]
fn era_boundary_pv_cascade_matches_haskell() {
    // Seed the ledger as if we are at Byron PV1.0. (`mainnet_defaults` ships
    // a Conway-shaped ProtocolParameters with PV9 since it is what live nodes
    // start from; for a from-genesis cascade we explicitly reset to PV1.0.)
    let mut params = ProtocolParameters::mainnet_defaults();
    params.protocol_version_major = 1;
    params.protocol_version_minor = 0;
    let mut state = LedgerState::new(params);
    state.tip.block_number = BlockNo(0);
    assert_eq!(
        state.epochs.protocol_params.protocol_version_major, 1,
        "Test prelude must seed PV1 (sanity)",
    );

    // (era, slot, block_no, header_pv_major, expected_post_apply_pv_major)
    //
    // Slots are arbitrary but monotonically increasing so the apply loop's
    // ordering checks succeed. We don't care about real epoch lengths — we're
    // only exercising the era-boundary HFC PV bump.
    let steps = [
        (Era::Byron, 100, 1, 1, 1u64),
        (Era::Shelley, 200, 2, 2, 2),
        (Era::Allegra, 300, 3, 3, 3),
        (Era::Mary, 400, 4, 4, 4),
        (Era::Alonzo, 500, 5, 5, 5),
        (Era::Babbage, 600, 6, 7, 7),
        (Era::Conway, 700, 7, 9, 9),
    ];

    for (era, slot, block_no, header_pv, expected_pv) in steps {
        apply_era_block(&mut state, era, slot, block_no, header_pv);
        assert_eq!(
            state.epochs.protocol_params.protocol_version_major, expected_pv,
            "After entering {:?} (block {}, slot {}), curPParams.protocol_version_major \
             must be {} — mirrors cardano-node HFC era-crossing tick (issue #615)",
            era, block_no, slot, expected_pv,
        );
        assert_eq!(
            state.epochs.protocol_params.protocol_version_minor, 0,
            "Initial minor PV at each era boundary is always 0 (issue #615)",
        );
        assert_eq!(state.era, era, "LedgerState.era must track block.era");
    }
}
