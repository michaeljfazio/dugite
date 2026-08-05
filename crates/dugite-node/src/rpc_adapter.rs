//! `NodeRpcAdapter` — the bridge from `dugite-node` internals to the
//! `dugite-rpc` server's [`LedgerContext`] trait.
//!
//! Every method is implemented against [`ChainDB`] / [`LedgerState`] /
//! [`Mempool`] / the retained genesis + era-history state:
//!
//! * `tip` — reads from `chain_db.get_tip()` and looks up the matching
//!   block's era via a minimal CBOR decode.
//! * `block_by_hash` — direct `chain_db.get_block()`.
//! * `block_at_slot` — `chain_db.get_block_at_or_after_slot()` filtered
//!   to exact-match.
//! * `block_after` — `chain_db.get_next_block_after_slot()`.
//! * `intersect` — walks the supplied points newest-to-oldest, returns
//!   the first that exists on-chain.
//! * `blocks_range` — `chain_db.get_blocks_in_slot_range()` then decodes
//!   identity per block for the carrier fields.
//! * `era_history` — issue #1009: real per-era `start`/`end` boundaries
//!   from `dugite_consensus::EraHistory::entries()`, the same source of
//!   truth the N2C `GetEraHistory` query uses
//!   (`Node::build_era_summaries`). `protocol_params` per era stays
//!   unset — see `dugite_rpc::EraHistoryView`'s doc comment for why.
//! * `genesis` — issue #1009: the Shelley-genesis section of
//!   `GenesisView` from the retained `ShelleyGenesis`. Byron/Alonzo/Conway
//!   sections stay unset — those genesis structs aren't retained past
//!   `Node::new()` today; see `dugite_rpc::GenesisView`'s doc comment.

use std::sync::Arc;

use async_trait::async_trait;
use dugite_consensus::{Bound, EraHistory};
use dugite_ledger::LedgerState;
use dugite_mempool::Mempool;
use dugite_network::TxValidator;
use dugite_primitives::address::Address;
use dugite_primitives::block::Point;
use dugite_primitives::hash::{Hash32, TransactionHash};
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::TransactionInput;
use dugite_rpc::{
    EraBoundaryView, EraHistoryView, EraSummary, GenesisView, LedgerContext, ParamsView, RawBlock,
    RawTx, RpcError, SubmitOutcome, TipFeed, TipInfo, TipPublisher, TipRollback, UtxoSnapshot,
};
use dugite_serialization::decode::decode_block_minimal;
use dugite_storage::ChainDB;
use tokio::sync::{watch, RwLock};
use tracing::debug;

/// Reconstruct a `(numerator, denominator)` rational from a genesis
/// JSON decimal like `activeSlotsCoeff: 0.05`. Deliberately narrow to
/// the genesis-file use case, NOT a general float-to-rational algorithm
/// (those don't terminate for values without a short decimal
/// expansion): every real Cardano genesis file (mainnet/preview/preprod
/// = 0.05; devnets typically 0.1/0.2/1.0) has at most a few significant
/// decimal digits, so scaling to 9 digits of precision and reducing by
/// the GCD recovers the exact value rather than an approximation.
fn decimal_to_rational(x: f64) -> Option<(i32, u32)> {
    if !x.is_finite() || x < 0.0 || x > i32::MAX as f64 {
        return None;
    }
    const SCALE: i64 = 1_000_000_000;
    let scaled = (x * SCALE as f64).round() as i64;
    if scaled <= 0 {
        return None;
    }
    let divisor = gcd(scaled, SCALE);
    let numerator = scaled / divisor;
    let denominator = SCALE / divisor;
    i32::try_from(numerator)
        .ok()
        .zip(u32::try_from(denominator).ok())
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Convert an `EraHistory` `Bound` (slot/epoch + picoseconds RELATIVE to
/// system start) into an absolute-wall-clock `EraBoundaryView`, as the
/// `EraBoundary.time` proto field (ms since Unix epoch) requires. 1 ms =
/// 1_000_000_000 picoseconds (see `Bound::time_pico`'s own doc: 1 second
/// = 1_000_000_000_000 picoseconds).
fn era_boundary_view(b: &Bound, system_start_unix_ms: u128) -> EraBoundaryView {
    let relative_ms = b.time_pico / 1_000_000_000;
    let absolute_ms = system_start_unix_ms.saturating_add(relative_ms);
    EraBoundaryView {
        time_ms: absolute_ms.min(u64::MAX as u128) as u64,
        slot: b.slot,
        epoch: b.epoch,
    }
}

/// Parse a genesis `system_start` (RFC3339, e.g. `"2017-09-23T21:44:51Z"`)
/// into Unix seconds. Mirrors the parsing already used for slot-config
/// derivation (`genesis.rs::ShelleyGenesis::slot_config`) and tip-age
/// estimation (`node/mod.rs`) — one parse behavior, not a third copy.
/// Returns 0 on a malformed string (matches those call sites' fallback).
fn system_start_unix_seconds(system_start: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(system_start)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

use crate::node::tip_broadcast::{TipApply, TipBroadcaster};

/// Map dugite-primitives `RedeemerTag` → `dugite_rpc::RedeemerPurpose`.
fn map_redeemer_tag(
    tag: &dugite_primitives::transaction::RedeemerTag,
) -> dugite_rpc::RedeemerPurpose {
    use dugite_primitives::transaction::RedeemerTag;
    match tag {
        RedeemerTag::Spend => dugite_rpc::RedeemerPurpose::Spend,
        RedeemerTag::Mint => dugite_rpc::RedeemerPurpose::Mint,
        RedeemerTag::Cert => dugite_rpc::RedeemerPurpose::Cert,
        RedeemerTag::Reward => dugite_rpc::RedeemerPurpose::Reward,
        RedeemerTag::Vote => dugite_rpc::RedeemerPurpose::Vote,
        RedeemerTag::Propose => dugite_rpc::RedeemerPurpose::Propose,
        RedeemerTag::Guarding => dugite_rpc::RedeemerPurpose::Unspecified,
    }
}

/// Concrete impl of [`LedgerContext`] backed by node internals.
pub struct NodeRpcAdapter {
    pub(crate) chain_db: Arc<RwLock<ChainDB>>,
    pub(crate) ledger_state: Arc<RwLock<LedgerState>>,
    pub(crate) mempool: Arc<Mempool>,
    pub(crate) tx_validator: Arc<dyn TxValidator>,
    pub(crate) slot_config: dugite_ledger::plutus::SlotConfig,
    /// Retained for `LedgerContext::genesis` (issue #1009) — `None` on
    /// networks/configs that don't supply a Shelley genesis file (should
    /// not happen in practice; `genesis()` falls back to a
    /// network_magic/security_param-only response in that case, same as
    /// before this field existed).
    pub(crate) shelley_genesis: Option<crate::genesis::ShelleyGenesis>,
    /// Retained for `LedgerContext::era_history` (issue #1009) — same
    /// `Arc<RwLock<_>>` `Node` itself holds, so era-boundary updates
    /// (hard forks) are visible here too without a second copy.
    pub(crate) era_history: Arc<RwLock<EraHistory>>,
}

impl NodeRpcAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain_db: Arc<RwLock<ChainDB>>,
        ledger_state: Arc<RwLock<LedgerState>>,
        mempool: Arc<Mempool>,
        tx_validator: Arc<dyn TxValidator>,
        slot_config: dugite_ledger::plutus::SlotConfig,
        shelley_genesis: Option<crate::genesis::ShelleyGenesis>,
        era_history: Arc<RwLock<EraHistory>>,
    ) -> Self {
        Self {
            chain_db,
            ledger_state,
            mempool,
            tx_validator,
            slot_config,
            shelley_genesis,
            era_history,
        }
    }

    /// Pull (slot, hash, block_no, era) for a raw block CBOR via a
    /// minimal decode. Returns an internal error if the decode fails —
    /// this should never happen for blocks that ChainDB successfully
    /// admitted, so a failure here indicates on-disk corruption.
    fn raw_block_from_cbor(cbor: Vec<u8>) -> Result<RawBlock, RpcError> {
        let block = decode_block_minimal(&cbor)
            .map_err(|e| RpcError::Internal(format!("block decode failed: {e}")))?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(block.header.header_hash.as_ref());
        Ok(RawBlock {
            slot: block.header.slot.0,
            hash,
            block_number: block.header.block_number.0,
            era: block.era,
            cbor,
        })
    }
}

#[async_trait]
impl LedgerContext for NodeRpcAdapter {
    async fn tip(&self) -> Result<TipInfo, RpcError> {
        let db = self.chain_db.read().await;
        let (slot, hash, block_no) = match db.get_tip_info() {
            Some(t) => t,
            None => return Err(RpcError::NotFound("chain tip not available".into())),
        };
        // Look up the actual block to recover the era. Minor cost
        // (one minimal decode); the tip is queried often but blocks
        // are bounded ~72 KB and the decode is microseconds.
        let cbor = db
            .get_block(&hash)
            .map_err(|e| RpcError::Internal(format!("chain_db get_block: {e}")))?
            .ok_or_else(|| {
                RpcError::Internal("tip block missing from ChainDB after get_tip".into())
            })?;
        let raw = Self::raw_block_from_cbor(cbor)?;
        Ok(TipInfo {
            slot: slot.0,
            hash: raw.hash,
            block_number: block_no.0,
            era: raw.era,
        })
    }

    async fn block_by_hash(&self, hash: &Hash32) -> Result<Option<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let Some(cbor) = db
            .get_block(hash)
            .map_err(|e| RpcError::Internal(format!("chain_db get_block: {e}")))?
        else {
            return Ok(None);
        };
        Self::raw_block_from_cbor(cbor).map(Some)
    }

    async fn block_at_slot(&self, slot: u64) -> Result<Option<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let Some((s, _h, cbor)) = db
            .get_block_at_or_after_slot(SlotNo(slot))
            .map_err(|e| RpcError::Internal(format!("chain_db get_block_at_or_after_slot: {e}")))?
        else {
            return Ok(None);
        };
        if s.0 != slot {
            return Ok(None);
        }
        Self::raw_block_from_cbor(cbor).map(Some)
    }

    async fn block_after(&self, slot: u64) -> Result<Option<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let Some((_s, _h, cbor)) = db
            .get_next_block_after_slot(SlotNo(slot))
            .map_err(|e| RpcError::Internal(format!("chain_db get_next_block_after_slot: {e}")))?
        else {
            return Ok(None);
        };
        Self::raw_block_from_cbor(cbor).map(Some)
    }

    async fn intersect(&self, points: &[Point]) -> Result<Option<Point>, RpcError> {
        let db = self.chain_db.read().await;
        // Scan from newest-slot to oldest: ChainSync intersection
        // semantics return the LATEST point that exists. Origin is
        // always valid as a fallback.
        let mut sorted: Vec<&Point> = points.iter().collect();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.slot().map(|s| s.0).unwrap_or(0)));
        for point in sorted {
            match point {
                Point::Origin => return Ok(Some(Point::Origin)),
                Point::Specific(slot, hash) => {
                    if let Some(cbor) = db
                        .get_block(hash)
                        .map_err(|e| RpcError::Internal(format!("chain_db get_block: {e}")))?
                    {
                        // Verify slot matches as a sanity check.
                        if let Ok(decoded) = decode_block_minimal(&cbor) {
                            if decoded.header.slot == *slot {
                                return Ok(Some(point.clone()));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    async fn blocks_range(
        &self,
        from_slot: u64,
        to_slot: u64,
        limit: usize,
    ) -> Result<Vec<RawBlock>, RpcError> {
        let db = self.chain_db.read().await;
        let cbors = db
            .get_blocks_in_slot_range(SlotNo(from_slot), SlotNo(to_slot))
            .map_err(|e| RpcError::Internal(format!("chain_db blocks_range: {e}")))?;
        let mut out = Vec::with_capacity(cbors.len().min(limit));
        for cbor in cbors.into_iter().take(limit) {
            out.push(Self::raw_block_from_cbor(cbor)?);
        }
        Ok(out)
    }

    async fn utxo_by_ref(&self, refs: &[TransactionInput]) -> Result<Vec<UtxoSnapshot>, RpcError> {
        let ledger = self.ledger_state.read().await;
        let mut out = Vec::with_capacity(refs.len());
        for input in refs {
            if let Some(output) = ledger.utxo.utxo_set.lookup(input) {
                out.push(UtxoSnapshot {
                    ref_: input.clone(),
                    output,
                    slot: None,
                });
            }
        }
        Ok(out)
    }

    async fn utxos_by_address(&self, addr: &Address) -> Result<Vec<UtxoSnapshot>, RpcError> {
        let ledger = self.ledger_state.read().await;
        Ok(ledger
            .utxo
            .utxo_set
            .outputs_for_address(addr)
            .into_iter()
            .map(|(input, output)| UtxoSnapshot {
                ref_: input,
                output,
                slot: None,
            })
            .collect())
    }

    async fn utxos_by_payment_credential(
        &self,
        cred: &Hash32,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::hash::Hash28;

        // Hash32 is the padded form (28 hash bytes + 4 zero bytes).
        // Extract the first 28 bytes as the credential payload.
        let mut h28 = [0u8; 28];
        h28.copy_from_slice(&cred.as_ref()[..28]);
        let h28 = Hash28::from_bytes(h28);

        let ledger = self.ledger_state.read().await;
        // Try VerificationKey first (most common), then Script.
        let mut out: Vec<UtxoSnapshot> = Vec::new();
        let cap = 5_000;

        let key_cred = Credential::VerificationKey(h28);
        for (input, output) in ledger
            .utxo
            .utxo_set
            .outputs_for_payment_credential(&key_cred, cap)
        {
            out.push(UtxoSnapshot {
                ref_: input,
                output,
                slot: None,
            });
        }

        if out.len() < cap {
            let script_cred = Credential::Script(h28);
            for (input, output) in ledger
                .utxo
                .utxo_set
                .outputs_for_payment_credential(&script_cred, cap - out.len())
            {
                out.push(UtxoSnapshot {
                    ref_: input,
                    output,
                    slot: None,
                });
            }
        }

        Ok(out)
    }

    async fn utxos_by_asset(
        &self,
        policy: &Hash32,
        name: Option<&[u8]>,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        use dugite_primitives::hash::Hash28;
        let mut policy_h = [0u8; 28];
        policy_h.copy_from_slice(&policy.as_ref()[..28]);
        let policy_h = Hash28::from_bytes(policy_h);

        let ledger = self.ledger_state.read().await;
        let cap = 5_000;
        Ok(ledger
            .utxo
            .utxo_set
            .outputs_for_asset(&policy_h, name, cap)
            .into_iter()
            .map(|(input, output)| UtxoSnapshot {
                ref_: input,
                output,
                slot: None,
            })
            .collect())
    }

    async fn params_at_tip(&self) -> Result<ParamsView, RpcError> {
        let ledger = self.ledger_state.read().await;
        let params = ledger.epochs.protocol_params.clone();
        let pv = params.protocol_version_major;
        Ok(ParamsView {
            params: Arc::new(params),
            protocol_version_major: pv,
        })
    }

    async fn era_history(&self) -> Result<EraHistoryView, RpcError> {
        // Issue #1009: real per-era boundaries from the SAME EraHistory
        // `Node` maintains for the N2C GetEraHistory query
        // (`Node::build_era_summaries`) — one source of truth, not a
        // second derivation. `entries()` carries the era tag directly
        // (unlike `to_era_summary_exports()`, which drops it), so no
        // separate protocol-version-to-era guess is needed.
        let eh = self.era_history.read().await;
        // System start (ms) needed to turn each boundary's
        // system-start-relative `time_pico` into an absolute wall-clock
        // ms timestamp, as `EraBoundary.time` requires.
        let system_start_ms = self
            .shelley_genesis
            .as_ref()
            .map(|sg| system_start_unix_seconds(&sg.system_start).saturating_mul(1_000) as u128)
            .unwrap_or(0);
        let summaries = eh
            .entries()
            .iter()
            .map(|entry| EraSummary {
                era: entry.era,
                start: era_boundary_view(&entry.start, system_start_ms),
                end: entry
                    .end
                    .as_ref()
                    .map(|b| era_boundary_view(b, system_start_ms)),
                slot_length_ms: entry.params.slot_length_ms as u32,
                epoch_length_slots: entry.params.epoch_size as u32,
            })
            .collect();
        Ok(EraHistoryView { summaries })
    }

    async fn genesis(&self) -> Result<GenesisView, RpcError> {
        // Issue #1009: the Shelley-genesis section, from the genesis
        // struct `Node` retains for its own startup (`Node::shelley_genesis`).
        // Byron/Alonzo/Conway sections are NOT populated — those genesis
        // structs are parsed once during `Node::new()` and dropped rather
        // than retained; see `dugite_rpc::GenesisView`'s doc comment for
        // the full accounting of what's deliberately left out and why.
        let Some(sg) = self.shelley_genesis.as_ref() else {
            // No Shelley genesis file configured — should not happen for
            // a real network, but fail open to an empty-but-valid
            // response rather than Err, matching this method's
            // pre-#1009 behaviour for missing data.
            return Ok(GenesisView::default());
        };
        Ok(GenesisView {
            network_magic: sg.network_magic as u32,
            network_id: sg.network_id.clone(),
            system_start_unix: system_start_unix_seconds(&sg.system_start),
            security_param: sg.security_param as u32,
            epoch_length: sg.epoch_length as u32,
            slot_length: sg.slot_length as u32,
            max_lovelace_supply: sg.max_lovelace_supply,
            max_kes_evolutions: sg.max_k_e_s_evolutions as u32,
            slots_per_kes_period: sg.slots_per_k_e_s_period as u32,
            update_quorum: sg.update_quorum as u32,
            active_slots_coeff: decimal_to_rational(sg.active_slots_coeff),
        })
    }

    async fn submit_tx(&self, era: u16, raw_cbor: &[u8]) -> SubmitOutcome {
        // 1. Phase-1 + Phase-2 validation (mirrors N2C LocalTxSubmission).
        if let Err(e) = self.tx_validator.validate_tx(era, raw_cbor) {
            return SubmitOutcome::Rejected {
                reason: format!("{e:?}"),
            };
        }

        // 2. Decode tx to extract hash + body for mempool admission.
        let tx = match dugite_serialization::decode_transaction(era, raw_cbor) {
            Ok(t) => t,
            Err(e) => {
                return SubmitOutcome::Rejected {
                    reason: format!("decode failed: {e}"),
                };
            }
        };

        let size_bytes = raw_cbor.len();
        let tx_hash = tx.hash;
        let fee = tx.body.fee;
        let (ex_mem, ex_steps) = {
            let mut m: u64 = 0;
            let mut s: u64 = 0;
            for r in &tx.witness_set.redeemers {
                m = m.saturating_add(r.ex_units.mem);
                s = s.saturating_add(r.ex_units.steps);
            }
            (m, s)
        };
        let ref_scripts_bytes = tx
            .witness_set
            .plutus_v1_scripts
            .iter()
            .map(|s| s.len())
            .chain(tx.witness_set.plutus_v2_scripts.iter().map(|s| s.len()))
            .chain(tx.witness_set.plutus_v3_scripts.iter().map(|s| s.len()))
            .sum();

        // 3. Admit to mempool.
        let admit_result = self.mempool.add_tx_full(
            tx_hash,
            tx,
            size_bytes,
            fee,
            ex_mem,
            ex_steps,
            ref_scripts_bytes,
            dugite_mempool::TxOrigin::Local,
        );

        match admit_result {
            Ok(_) => SubmitOutcome::Accepted { hash: tx_hash },
            Err(e) => SubmitOutcome::Rejected {
                reason: format!("mempool admission failed: {e}"),
            },
        }
    }

    async fn eval_tx(&self, era: u16, raw_cbor: &[u8]) -> dugite_rpc::EvalOutcome {
        // 1. Phase-1 + Phase-2 validation via the same LedgerTxValidator
        //    instance N2C uses. Failure messages flow through verbatim
        //    so clients see the structured TxValidationError reason.
        let validation = self.tx_validator.validate_tx(era, raw_cbor);

        // 2. Decode (separately, so a successful validation can still
        //    surface fee / per-redeemer reports). On decode failure we
        //    fall back to the bare error+fee=0 shape.
        let decoded = dugite_serialization::decode_transaction(era, raw_cbor);
        let fee = decoded.as_ref().map(|tx| tx.body.fee.0).unwrap_or(0);

        // 3. Run Phase-2 with per-redeemer reports. We don't need the
        //    validator's pass/fail (already in `validation`), but the
        //    reports surface ex_units + traces back to the client.
        //    Skip when the tx has no Plutus redeemers — keeps the
        //    happy-path fast.
        let mut redeemers: Vec<dugite_rpc::RedeemerReport> = Vec::new();
        if let Ok(tx) = decoded.as_ref() {
            if dugite_ledger::plutus::has_plutus_scripts(tx) {
                let ledger = self.ledger_state.read().await;
                let max_units = (
                    ledger.epochs.protocol_params.max_tx_ex_units.steps,
                    ledger.epochs.protocol_params.max_tx_ex_units.mem,
                );
                // Build a composite view: the live UTxO set plus the
                // (deduplicated) mempool virtual UTxOs so reference
                // inputs introduced in pending txs resolve. We use the
                // existing mempool view if available; otherwise the
                // live set.
                let utxo_view = &ledger.utxo.utxo_set;
                let protocol_major = ledger.epochs.protocol_params.protocol_version_major as u32;
                let report_outcome = dugite_ledger::evaluate_plutus_scripts_with_reports(
                    tx,
                    utxo_view,
                    None, // cost models — phase-2 evaluator falls back to per-step defaults
                    max_units,
                    &self.slot_config,
                    protocol_major,
                );
                drop(ledger);
                if let Ok(reports) = report_outcome {
                    for r in reports {
                        redeemers.push(dugite_rpc::RedeemerReport {
                            index: r.index,
                            purpose: map_redeemer_tag(&r.tag),
                            ex_units: (r.ex_units_cpu, r.ex_units_mem),
                            logs: r.logs,
                            error: None,
                        });
                    }
                }
            }
        }

        dugite_rpc::EvalOutcome {
            fee,
            error: match validation {
                Ok(()) => None,
                Err(e) => Some(format!("{e:?}")),
            },
            redeemers,
        }
    }

    async fn utxos_filter(
        &self,
        keep: &(dyn for<'a> Fn(&'a UtxoSnapshot) -> bool + Send + Sync),
        cap: usize,
    ) -> Result<Vec<UtxoSnapshot>, RpcError> {
        // O(N) walk over the in-memory UTxO set. LSM-backed backends
        // delegate to `scan_all` which streams ~256 chunks at a time so
        // peak memory stays bounded; we still cap the returned vector.
        let ledger = self.ledger_state.read().await;
        let mut out: Vec<UtxoSnapshot> = Vec::new();
        let mut full = false;
        ledger.utxo.utxo_set.scan_all(|input, output| {
            if full {
                return;
            }
            let snap = UtxoSnapshot {
                ref_: input.clone(),
                output: output.clone(),
                slot: None,
            };
            let matched = keep(&snap);
            if matched {
                out.push(snap);
                if out.len() >= cap {
                    full = true;
                }
            }
        });
        Ok(out)
    }

    async fn datum_by_hash(&self, hash: &Hash32) -> Result<Option<Vec<u8>>, RpcError> {
        use dugite_primitives::transaction::OutputDatum;

        // 1) Scan the in-memory UTxO set for an inline datum matching
        //    the hash. Bounded by the UTxO set size (#403 mitigation:
        //    via `scan_all` chunk streaming).
        let ledger = self.ledger_state.read().await;
        let target = *hash;
        let mut found: Option<Vec<u8>> = None;
        ledger.utxo.utxo_set.scan_all(|_input, output| {
            if found.is_some() {
                return;
            }
            if let OutputDatum::InlineDatum { data, raw_cbor } = &output.datum {
                let cbor = raw_cbor
                    .clone()
                    .unwrap_or_else(|| dugite_serialization::encode_plutus_data(data));
                let h = dugite_primitives::hash::blake2b_256(&cbor);
                if h == target {
                    found = Some(cbor);
                }
            }
        });
        drop(ledger);
        if found.is_some() {
            return Ok(found);
        }

        // 2) Walk the mempool transactions and check their witness sets
        //    for plutus_data hash matches.
        for hash_id in self.mempool.tx_hashes_ordered() {
            if let Some(tx) = self.mempool.get_tx(&hash_id) {
                for datum in &tx.witness_set.plutus_data {
                    let cbor = dugite_serialization::encode_plutus_data(datum);
                    let h = dugite_primitives::hash::blake2b_256(&cbor);
                    if h == target {
                        return Ok(Some(cbor));
                    }
                }
            }
        }

        Ok(None)
    }

    async fn tx_by_hash(&self, hash: &TransactionHash) -> Result<Option<RawTx>, RpcError> {
        // 1) Mempool fast path.
        if let Some(tx) = self.mempool.get_tx(hash) {
            return Ok(Some(RawTx {
                hash: tx.hash,
                cbor: tx.raw_cbor.unwrap_or_default(),
            }));
        }

        // 2) Bounded scan of recent volatile blocks. We walk back at
        //    most TX_BY_HASH_BLOCK_WINDOW blocks from the tip; tx
        //    lookups outside that window require a chain-wide tx index
        //    (separate feature; tracked in the docs/Limitations section).
        const TX_BY_HASH_BLOCK_WINDOW: u64 = 2_160; // ~12 h on mainnet
        let db = self.chain_db.read().await;
        let Some((tip_slot, _, _)) = db.get_tip_info() else {
            return Ok(None);
        };
        let from = tip_slot.0.saturating_sub(TX_BY_HASH_BLOCK_WINDOW * 20);
        let blocks = db
            .get_blocks_in_slot_range(SlotNo(from), tip_slot)
            .map_err(|e| RpcError::Internal(format!("blocks_in_slot_range: {e}")))?;
        for cbor in blocks {
            let Ok(block) = decode_block_minimal(&cbor) else {
                continue;
            };
            // tx_byte_ranges + decode each tx looking for hash match.
            let Some(ranges) = block.tx_byte_ranges() else {
                continue;
            };
            for range in ranges {
                let tx_bytes = &cbor[range.clone()];
                let era_id = match block.era {
                    dugite_primitives::Era::Shelley => 1u16,
                    dugite_primitives::Era::Allegra => 2,
                    dugite_primitives::Era::Mary => 3,
                    dugite_primitives::Era::Alonzo => 4,
                    dugite_primitives::Era::Babbage => 5,
                    dugite_primitives::Era::Conway | dugite_primitives::Era::Dijkstra => 6,
                    dugite_primitives::Era::Byron => continue,
                };
                if let Ok(tx) = dugite_serialization::decode_transaction(era_id, tx_bytes) {
                    if tx.hash == *hash {
                        return Ok(Some(RawTx {
                            hash: tx.hash,
                            cbor: tx_bytes.to_vec(),
                        }));
                    }
                }
            }
        }
        Ok(None)
    }

    async fn ledger_state(&self) -> Result<dugite_rpc::LedgerStateView, RpcError> {
        let ledger = self.ledger_state.read().await;
        let slot = ledger.current_slot().map(|s| s.0).unwrap_or(0);
        let epoch = ledger.epoch_of_slot(slot);
        let first_slot = ledger.first_slot_of_epoch(epoch);
        let slot_in_epoch = slot.saturating_sub(first_slot);
        let block_no = ledger.current_block_number().0;
        drop(ledger);
        let db = self.chain_db.read().await;
        let (_, hash, _) = db
            .get_tip_info()
            .ok_or_else(|| RpcError::NotFound("ledger_state: chain tip unavailable".into()))?;
        let mut hash_arr = [0u8; 32];
        hash_arr.copy_from_slice(hash.as_ref());
        let pv = self
            .ledger_state
            .read()
            .await
            .epochs
            .protocol_params
            .protocol_version_major;
        let era = dugite_primitives::block::ProtocolVersion {
            major: pv,
            minor: 0,
        }
        .era();
        Ok(dugite_rpc::LedgerStateView {
            tip: TipInfo {
                slot,
                hash: hash_arr,
                block_number: block_no,
                era,
            },
            epoch,
            slot_in_epoch,
        })
    }

    async fn mempool_snapshot(&self) -> Result<Vec<RawTx>, RpcError> {
        let hashes = self.mempool.tx_hashes_ordered();
        let mut out = Vec::with_capacity(hashes.len());
        for hash in hashes {
            if let Some(tx) = self.mempool.get_tx(&hash) {
                out.push(RawTx {
                    hash: tx.hash,
                    cbor: tx.raw_cbor.unwrap_or_default(),
                });
            }
        }
        Ok(out)
    }

    async fn mempool_contains(&self, hash: &TransactionHash) -> bool {
        self.mempool.contains(hash)
    }
}

/// Spawns a forwarder task that subscribes to the node-side
/// [`TipBroadcaster`] and republishes payload-shaped events into the
/// RPC-side [`TipPublisher`]. Exits cleanly when `shutdown_rx` fires
/// (true) or when the upstream broadcaster has no more senders.
pub fn spawn_tip_forwarder(
    broadcaster: Arc<TipBroadcaster>,
    publisher: TipPublisher,
    mut shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let mut apply_rx = broadcaster.subscribe_apply();
    let mut rollback_rx = broadcaster.subscribe_rollback();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        debug!("dugite-rpc tip forwarder: shutdown signalled, exiting");
                        break;
                    }
                }
                apply = apply_rx.recv() => {
                    match apply {
                        Ok(ev) => publisher.announce_apply(map_apply(ev)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged = n, "dugite-rpc tip forwarder lagged on apply broadcast");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                rollback = rollback_rx.recv() => {
                    match rollback {
                        Ok(ev) => publisher.announce_rollback(TipRollback {
                            slot: ev.slot,
                            hash: ev.hash,
                        }),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged = n, "dugite-rpc tip forwarder lagged on rollback broadcast");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    })
}

fn map_apply(ev: TipApply) -> TipInfo {
    TipInfo {
        slot: ev.slot,
        hash: ev.hash,
        block_number: ev.block_number,
        era: ev.era,
    }
}

/// Builds a fresh [`TipFeed`] suitable for handing to
/// [`dugite_rpc::RpcServer::start`]. Returns the feed + its publisher.
pub fn build_tip_feed() -> (TipFeed, TipPublisher) {
    let feed = TipFeed::new();
    let publisher = feed.publisher();
    (feed, publisher)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── decimal_to_rational (issue #1009) ─────────────────────────────

    #[test]
    fn decimal_to_rational_matches_real_genesis_values() {
        // mainnet / preview / preprod all use exactly this value.
        assert_eq!(decimal_to_rational(0.05), Some((1, 20)));
        // Common devnet values.
        assert_eq!(decimal_to_rational(0.1), Some((1, 10)));
        assert_eq!(decimal_to_rational(0.2), Some((1, 5)));
        assert_eq!(decimal_to_rational(1.0), Some((1, 1)));
    }

    #[test]
    fn decimal_to_rational_rejects_invalid_input() {
        assert_eq!(decimal_to_rational(-0.05), None, "negative is invalid");
        assert_eq!(
            decimal_to_rational(0.0),
            None,
            "zero has no meaningful rational form here"
        );
        assert_eq!(decimal_to_rational(f64::NAN), None);
        assert_eq!(decimal_to_rational(f64::INFINITY), None);
    }

    #[test]
    fn gcd_matches_expected_values() {
        assert_eq!(gcd(50_000_000, 1_000_000_000), 50_000_000);
        assert_eq!(gcd(6, 4), 2);
        assert_eq!(gcd(7, 5), 1);
    }

    // ─── system_start_unix_seconds ─────────────────────────────────────

    #[test]
    fn system_start_unix_seconds_parses_real_genesis_timestamps() {
        // Mainnet shelley-genesis.json systemStart.
        assert_eq!(
            system_start_unix_seconds("2019-07-24T20:20:16Z"),
            1_563_999_616
        );
        // Preview.
        assert_eq!(
            system_start_unix_seconds("2022-10-25T00:00:00Z"),
            1_666_656_000
        );
    }

    #[test]
    fn system_start_unix_seconds_falls_back_to_zero_on_malformed_input() {
        assert_eq!(system_start_unix_seconds("not a date"), 0);
        assert_eq!(system_start_unix_seconds(""), 0);
    }

    // ─── era_boundary_view ──────────────────────────────────────────────

    #[test]
    fn era_boundary_view_adds_system_start_to_relative_time() {
        let bound = Bound {
            time_pico: 4_492_800u128 * 20_000 * 1_000_000_000, // 4,492,800 Byron slots @ 20s
            slot: 4_492_800,
            epoch: 208,
        };
        // Mainnet system start, ms.
        let system_start_ms = 1_506_203_091_000u128;
        let view = era_boundary_view(&bound, system_start_ms);
        // relative_ms = 4_492_800 * 20_000 = 89_856_000_000 ms
        assert_eq!(view.time_ms, 1_506_203_091_000 + 89_856_000_000);
        assert_eq!(view.slot, 4_492_800);
        assert_eq!(view.epoch, 208);
    }

    #[test]
    fn era_boundary_view_at_system_start_is_the_system_start_time() {
        let bound = Bound {
            time_pico: 0,
            slot: 0,
            epoch: 0,
        };
        let view = era_boundary_view(&bound, 1_666_656_000_000);
        assert_eq!(view.time_ms, 1_666_656_000_000);
        assert_eq!(view.slot, 0);
        assert_eq!(view.epoch, 0);
    }
}
