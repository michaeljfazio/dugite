//! Cardano ledger: UTxO management, transaction validation, rewards, governance.

pub mod eras;
pub mod ledger_seq;
pub mod plutus;
pub mod rules;
pub mod state;
pub mod utxo;
pub mod utxo_diff;
pub mod utxo_store;
pub mod validation;

pub use plutus::{
    evaluate_plutus_scripts, evaluate_plutus_scripts_with_reports, PlutusError, RedeemerReport,
    SlotConfig,
};
#[doc(hidden)]
pub use state::Rat;
pub use state::{
    check_snapshot_backend_match, compute_reward_update, infer_backend_from_snapshot,
    BackendCheckResult, BlockValidationMode, CertSubState, ConsensusSubState, EpochSubState,
    GovSubState, LedgerState, LedgerStateSnapshot, SnapshotBackend, SnapshotMeta, UtxoSubState,
};
pub use utxo::{CompositeUtxoView, UtxoLookup, UtxoSet};
pub use utxo_diff::{DiffSeq, UtxoDiff};
pub use utxo_store::UtxoStore;
pub use validation::{
    evaluate_native_script, validate_transaction, validate_transaction_with_pools, ValidationError,
};
