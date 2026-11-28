//! Apply an ImpSpec signal through dugite's production entry points.
//!
//! * The state is built by `LedgerState::from_haskell_snapshot` — the function
//!   `mithril-import` runs after `decode_state_file` — followed by the same
//!   genesis-derived configuration calls the node's
//!   `import_haskell_ledger_snapshot` makes, with the ImpSpec genesis values
//!   (see `imp_state`'s module doc). The UTxO is inserted the way the node
//!   inserts a tables blob.
//! * Both rules go through `LedgerState::apply_block` in
//!   `BlockValidationMode::ValidateAll`, the path every block (network,
//!   forged, replayed) takes. `process_epoch_transition` is documented as
//!   test-only and is NOT used.
//!   * LEDGER: one block holding the transaction, at the vector's slot.
//!   * NEWEPOCH: one empty block at the first slot of the signal epoch, which is
//!     what drives dugite's epoch-boundary path.
//!
//! ## A block is not a bare rule
//!
//! `apply_block` also runs steps Haskell's LEDGER / NEWEPOCH rules do not:
//! the TICK-time pparam prediction, block-body limits (`BBODY`), nonce
//! evolution and the tip update. Those are the effects `project::NOT_COMPARED`
//! excludes or the comparison does not read; nothing here re-implements a rule.

use dugite_ledger::plutus::SlotConfig;
use dugite_ledger::{BlockValidationMode, LedgerState};
use dugite_primitives::block::{Block, BlockHeader, OperationalCert, ProtocolVersion, VrfOutput};
use dugite_primitives::era::Era;
use dugite_primitives::hash::Hash32;
use dugite_primitives::network::NetworkId;
use dugite_primitives::time::{BlockNo, SlotNo};

use crate::upstream::ledger_rules_replay::imp_state::{
    reconstruct_tip_slot, to_haskell_ledger_state, ImpState, RuleKind, IMP_ACTIVE_SLOTS_COEFF,
    IMP_EPOCH_LENGTH, IMP_MAX_LOVELACE_SUPPLY, IMP_SECURITY_PARAM, IMP_SLOT_LENGTH_MS,
    IMP_SYSTEM_START_MS,
};
use crate::upstream::ledger_rules_replay::vector::ImpVector;

/// Tip hash the imported ledger starts at; the harness block's `prev_hash`.
const TIP_HASH: [u8; 32] = [0xA0; 32];
/// `sgNetworkMagic` of the ImpSpec genesis.
const IMP_NETWORK_MAGIC: u64 = 123_456;

/// Build dugite's ledger state from a decoded ImpSpec state.
pub fn import_state(imp: ImpState) -> (LedgerState, Option<u64>) {
    let ImpState {
        kind,
        nes,
        utxo,
        env_slot,
        reward_update_present,
        ..
    } = imp;
    // The tip position only matters for `finalise_genesis_derived_windows`
    // (reward-pulser flag) and the block connection below.
    let tip_slot = match kind {
        RuleKind::Ledger => env_slot.unwrap_or(0).saturating_sub(1),
        RuleKind::NewEpoch => reconstruct_tip_slot(nes.epoch.0, reward_update_present),
    };
    let hs = to_haskell_ledger_state(nes, tip_slot, Hash32::from_bytes(TIP_HASH));
    let mut state = LedgerState::from_haskell_snapshot(&hs);

    // Genesis-derived configuration, in the node's order.
    state.set_epoch_length(IMP_EPOCH_LENGTH, IMP_SECURITY_PARAM);
    state.set_slot_config(SlotConfig {
        zero_time: IMP_SYSTEM_START_MS,
        zero_slot: 0,
        slot_length: IMP_SLOT_LENGTH_MS,
        safe_zone_horizon_slot: None,
    });
    state.set_update_quorum(5);
    state.set_shelley_transition(0, 0, IMP_NETWORK_MAGIC);
    state.epochs.protocol_params.active_slots_coeff = IMP_ACTIVE_SLOTS_COEFF;
    state.epochs.prev_protocol_params.active_slots_coeff = IMP_ACTIVE_SLOTS_COEFF;
    state.finalise_genesis_derived_windows();
    state.node_network = Some(NetworkId::Testnet);
    state.max_lovelace_supply = IMP_MAX_LOVELACE_SUPPLY;

    for (i, o) in utxo {
        state.utxo.utxo_set.insert(i, o);
    }
    (state, env_slot)
}

fn block(slot: u64, txs: Vec<dugite_primitives::transaction::Transaction>) -> Block {
    Block {
        header: BlockHeader {
            header_hash: Hash32::from_bytes([0xB1; 32]),
            prev_hash: Hash32::from_bytes(TIP_HASH),
            // Empty issuer: no pool is credited with the block.
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
            block_number: BlockNo(2),
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
                major: 11,
                minor: 0,
            },
            kes_signature: vec![],
        },
        transactions: txs,
        era: Era::Conway,
        raw_cbor: None,
        byron: None,
    }
}

/// Outcome of applying a signal.
pub enum Applied {
    /// dugite accepted; the post state.
    Accepted(Box<LedgerState>),
    /// dugite rejected (or failed to decode the signal / panicked); the class.
    Rejected(String),
}

/// Apply a LEDGER signal (a `Tx`) to `state`.
pub fn apply_ledger(mut state: LedgerState, vec: &ImpVector, env_slot: Option<u64>) -> Applied {
    // `Tx era` is `array(4) [body, witnesses, isValid, auxiliary_data]`.
    let tx = match dugite_serialization::decode_transaction(6, &vec.sig_cbor) {
        Ok(tx) => tx,
        Err(e) => return Applied::Rejected(format!("TxDecodeFailed: {e}")),
    };
    let slot = env_slot.unwrap_or(0);
    run(&mut state, &block(slot, vec![tx]))
}

/// Apply a NEWEPOCH signal (an `EpochNo`) to `state`.
pub fn apply_newepoch(mut state: LedgerState, signal_epoch: u64) -> Applied {
    run(&mut state, &block(signal_epoch * IMP_EPOCH_LENGTH, vec![]))
}

fn run(state: &mut LedgerState, b: &Block) -> Applied {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.apply_block(b, BlockValidationMode::ValidateAll)
    }));
    match r {
        Ok(Ok(())) => Applied::Accepted(Box::new(state.clone())),
        Ok(Err(e)) => Applied::Rejected(error_class(&e.to_string())),
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            Applied::Rejected(format!("Panic: {}", first_line(&msg)))
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(120).collect()
}

/// Reduce a `LedgerError` display string to a stable class: the leading
/// predicate-failure names, with hashes/amounts/slots stripped, so the recorded
/// observed result does not churn on values that vary per vector.
pub fn error_class(msg: &str) -> String {
    // `BlockTxValidationFailed` carries `errors: <list>`; keep the part after
    // the last "tx <hash>: " prefix.
    let body = msg.rsplit_once("tx ").map(|(_, r)| r).unwrap_or(msg);
    let body = body.split_once(": ").map(|(_, r)| r).unwrap_or(body);
    // Take identifier-like tokens that start a clause.
    let mut classes: Vec<String> = Vec::new();
    for clause in body.split(&[';', '\n'][..]) {
        let tok: String = clause
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if tok.len() > 2 && !classes.contains(&tok) {
            classes.push(tok);
        }
    }
    if classes.is_empty() {
        first_line(msg)
    } else {
        classes.join("+")
    }
}
