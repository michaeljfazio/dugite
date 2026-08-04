//! CBOR encoding helpers for LocalStateQuery responses.
//!
//! This module contains `encode_query_result` (the top-level serializer) and
//! all supporting encode helpers.  The HFC wrapper logic (EitherMismatch Right
//! / QueryAnytime / QueryHardFork) lives here so that the dispatch layer in
//! `mod.rs` stays thin.

use crate::node::n2c_query::types::{
    DRepDelegationGroup, DRepKey, GovActionId, ProposalSnapshot, ProtocolParamsSnapshot,
    QueryResult, RelaySnapshot, ShelleyPParamsSnapshot, SnapshotStakeData, UtxoSnapshot,
};
use dugite_primitives::transaction::GovAction;

// ─── Top-level result encoder ────────────────────────────────────────────────

/// Encode a `QueryResult` as a full N2C `MsgResult` response.
///
/// Wire format:
/// ```text
/// [4, result]                          -- QueryAnytime / QueryHardFork / top-level
/// [4, [result]]                        -- BlockQuery (EitherMismatch success: array(1))
/// ```
#[allow(dead_code)] // used in tests
pub fn encode_query_result(result: &QueryResult) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);

    // MsgResult [4, result]
    // For BlockQuery (era-specific) results: [4, [result]]  (HFC success wrapper)
    // For QueryAnytime/QueryHardFork results: [4, result]   (no wrapper)
    enc.array(2).ok();
    enc.u32(4).ok(); // MsgResult tag

    // QueryVersion2 (N2C v16+) response encoding for BlockQuery:
    //
    // Top-level queries (outer tags 1/2/3: GetSystemStart/GetChainBlockNo/GetChainPoint):
    //   [4, toCBOR result]  — result directly, no HFC wrapping
    //
    // BlockQuery > QueryIfCurrent results (Shelley era queries):
    //   [4, [result]]  — EitherMismatch success wrapper (array(1))
    //   Haskell HFC: success = array(1)[result], mismatch = array(2)[era1, era2]
    //   Discriminator is array length, NOT a tag byte.
    //
    // BlockQuery > QueryAnytime results (GetCurrentEra, GetEraStart):
    //   [4, result]  — no EitherMismatch wrapping
    //
    // BlockQuery > QueryHardFork results (GetCurrentEra, GetInterpreter):
    //   [4, result]  — no EitherMismatch wrapping (raw word8 for era, encoded summary for history)
    let needs_either_mismatch = !matches!(
        result,
        // Top-level queries (no wrapping)
        QueryResult::SystemStart(_)
            | QueryResult::ChainBlockNo(_)
            | QueryResult::ChainPoint { .. }
            // BlockQuery > QueryAnytime results (no wrapping)
            | QueryResult::CurrentEra(_)
            // BlockQuery > QueryHardFork results (no wrapping)
            | QueryResult::HardForkCurrentEra(_)
            | QueryResult::EraHistory(_)
    );

    if needs_either_mismatch {
        // HFC EitherMismatch success: array(1) containing just the result.
        // Array length 1 = success, length 2 = era mismatch.
        //
        // NOTE: `LedgerTip` is a BlockQuery > QueryIfCurrent result and
        // therefore DOES take the HFC wrapper. Its absence from the exclusion
        // list above is intentional.
        enc.array(1).ok();
    }

    encode_query_result_value(&mut enc, result);

    buf
}

/// Encode a `QueryResult` as the MsgResult payload (no `[4, ...]` envelope).
///
/// Returns the result with proper HFC wrapping:
/// - BlockQuery QueryIfCurrent results: `[result]` (EitherMismatch success: array(1))
/// - Top-level / QueryAnytime / QueryHardFork results: `result` (no wrapping)
pub fn encode_query_result_payload(result: &QueryResult) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);

    let needs_either_mismatch = !matches!(
        result,
        QueryResult::SystemStart(_)
            | QueryResult::ChainBlockNo(_)
            | QueryResult::ChainPoint { .. }
            | QueryResult::CurrentEra(_)
            | QueryResult::HardForkCurrentEra(_)
            | QueryResult::EraHistory(_)
    );

    if needs_either_mismatch {
        // HFC EitherMismatch success: array(1) containing just the result.
        // Array length 1 = success, length 2 = era mismatch.
        //
        // `LedgerTip` is a BlockQuery > QueryIfCurrent result and therefore
        // DOES take the HFC wrapper (see comment in `encode_query_result`).
        enc.array(1).ok();
    }

    encode_query_result_value(&mut enc, result);
    buf
}

/// Encode just the query result value (no MsgResult wrapper, no HFC wrapper).
///
/// Used by `encode_query_result` for normal encoding and by `WrappedCbor` for
/// inner encoding (GetCBOR tag 9 wraps the inner result in `tag(24)`).
pub(crate) fn encode_query_result_value(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    result: &QueryResult,
) {
    match result {
        QueryResult::EpochNo(epoch) => {
            enc.u64(*epoch).ok();
        }
        QueryResult::LedgerTip { slot, hash } => {
            // GetLedgerTip (Shelley BlockQuery tag 0) result is a bare Point:
            //   [slot, hash]
            //
            // This matches cardano-node 10.6.2 wire (captured 43-byte payload,
            // see issue #407):
            //   82 04 81 82 1a <slot4> 58 20 <hash32>
            //
            // The outer `array(1)` HFC EitherMismatch success wrapper is added
            // by `encode_query_result` because `LedgerTip` is not in the
            // top-level-query exclusion list.
            enc.array(2).ok();
            enc.u64(*slot).ok();
            enc.bytes(hash).ok();
        }
        QueryResult::CurrentEra(era) => {
            enc.u32(*era).ok();
        }
        QueryResult::SystemStart(time_str) => {
            // SystemStart is a UTCTime, encoded as [year, day_of_year, pico_of_day]
            // Parse the ISO 8601 date string and convert to ordinal date representation
            encode_system_start(enc, time_str);
        }
        QueryResult::ChainBlockNo(block_no) => {
            // WithOrigin encoding (generic Serialise):
            //   Origin = [0] (constructor 0)
            //   At blockNo = [1, blockNo] (constructor 1)
            enc.array(2).ok();
            enc.u8(1).ok(); // At constructor
            enc.u64(*block_no).ok();
        }
        QueryResult::ChainPoint { slot, hash } => {
            // Point encoding: [] for Origin, [slot, hash] for Specific
            if hash.is_empty() {
                enc.array(0).ok();
            } else {
                enc.array(2).ok();
                enc.u64(*slot).ok();
                enc.bytes(hash).ok();
            }
        }
        QueryResult::ProtocolParams(pp) => {
            encode_protocol_params_cbor(enc, pp);
        }
        QueryResult::StakeDistribution(pools) => {
            // Wire format: Map<pool_hash(28), IndividualPoolStake>
            // IndividualPoolStake: array(2) [tag(30)[num,den], vrf_hash(32)]
            // Haskell `poolsByTotalStakeFraction` (Shelley/API/Wallet.hs:185-201)
            // reports each pool's share of TOTAL CIRCULATING stake, not of total
            // active stake:
            //   individualPoolStake = (stake / totalActive) * (totalActive / totalStake)
            //                       = stake / totalStake
            // where totalStake = maxLovelaceSupply - reserves - treasury. Dividing
            // by total ACTIVE stake made every pool on a fully-delegated chain
            // report ~1.0 instead of its real share (#905).
            //
            // Pools with no delegators are omitted entirely — Haskell's
            // `calculatePoolDistr'` guards on `spssNumDelegators > 0` before
            // inserting into the map.
            let live: Vec<_> = pools.iter().filter(|p| p.stake > 0).collect();
            enc.map(live.len() as u64).ok();
            for pool in live {
                enc.bytes(&pool.pool_id).ok();
                enc.array(2).ok();
                encode_reduced_rational(enc, pool.stake, pool.total_circulation);
                enc.bytes(&pool.vrf_keyhash).ok();
            }
        }
        QueryResult::GovState(gov) => {
            encode_gov_state(enc, gov);
        }
        QueryResult::DRepState(dreps) => {
            encode_drep_state(enc, dreps);
        }
        QueryResult::CommitteeState(committee) => {
            encode_committee_state(enc, committee);
        }
        QueryResult::UtxoByAddress(utxos) => {
            encode_utxo_by_address(enc, utxos);
        }
        QueryResult::StakeAddressInfo(addrs) => {
            encode_stake_address_info(enc, addrs);
        }
        QueryResult::StakeSnapshots(snapshots) => {
            encode_stake_snapshots(enc, snapshots);
        }
        QueryResult::StakePools(pool_ids) => {
            encode_stake_pools(enc, pool_ids);
        }
        QueryResult::PoolParams(params) => {
            encode_pool_params_map(enc, params);
        }
        QueryResult::PoolState {
            pool_params,
            future_pool_params,
            retiring,
            deposits,
        } => {
            encode_pool_state(enc, pool_params, future_pool_params, retiring, deposits);
        }
        QueryResult::AccountState { treasury, reserves } => {
            // Account state: [treasury, reserves]
            enc.array(2).ok();
            enc.u64(*treasury).ok();
            enc.u64(*reserves).ok();
        }
        QueryResult::GenesisConfig(gc, version) => {
            encode_genesis_config(enc, gc, *version);
        }
        QueryResult::NonMyopicMemberRewards(rewards) => {
            // Map from stake_amount -> map from pool_id -> reward
            enc.map(rewards.len() as u64).ok();
            for entry in rewards {
                enc.u64(entry.stake_amount).ok();
                enc.map(entry.pool_rewards.len() as u64).ok();
                for (pool_id, reward) in &entry.pool_rewards {
                    enc.bytes(pool_id).ok();
                    enc.u64(*reward).ok();
                }
            }
        }
        QueryResult::ProposedPParamsUpdates => {
            // Empty map — Conway era uses governance proposals instead of PP updates
            enc.map(0).ok();
        }
        QueryResult::Constitution {
            url,
            data_hash,
            script_hash,
        } => {
            encode_constitution(enc, url, data_hash, script_hash.as_deref());
        }
        QueryResult::PoolDistr {
            pools,
            total_active_stake,
        } => {
            // Wire format: Map<pool_hash(28), IndividualPoolStake>
            // IndividualPoolStake: array(2) [tag(30)[num,den], vrf_hash(32)]
            //
            // Deprecated at N2C V21 in favour of tag 36, and upstream answers
            // it by delegating to `GetPoolDistr2` with the same argument
            // (`fromLedgerPoolDistr $ answerPureBlockQuery cfg (GetPoolDistr2
            // mPoolIds)`), so it reads the same `set`-snapshot distribution and
            // applies the same zero-delegator filter (#964).
            encode_pool_distr_legacy(enc, pools, *total_active_stake);
        }
        QueryResult::StakeDelegDeposits(deposits) => {
            // Wire format: Map<Credential, Coin>
            // Credential: [0|1, hash(28)]
            enc.map(deposits.len() as u64).ok();
            for entry in deposits {
                enc.array(2).ok();
                enc.u8(entry.credential_type).ok();
                enc.bytes(&entry.credential_hash).ok();
                enc.u64(entry.deposit).ok();
            }
        }
        QueryResult::DRepStakeDistr(entries) => {
            encode_drep_stake_distr(enc, entries);
        }
        QueryResult::FilteredVoteDelegatees(delegatees) => {
            encode_filtered_vote_delegatees(enc, delegatees);
        }
        QueryResult::DRepDelegations(delegations) => {
            encode_drep_delegations(enc, delegations);
        }
        QueryResult::EraHistory(summaries) => {
            encode_era_history(enc, summaries);
        }
        QueryResult::WrappedCbor(inner) => {
            // GetCBOR (tag 9): encode the inner result value as CBOR, then wrap in tag(24).
            // The inner encoding must NOT include the MsgResult [4,...] or HFC wrappers —
            // those are already provided by the outer encode_query_result call.
            let mut inner_buf = Vec::new();
            let mut inner_enc = minicbor::Encoder::new(&mut inner_buf);
            encode_query_result_value(&mut inner_enc, inner);
            enc.tag(minicbor::data::Tag::new(24)).ok();
            enc.bytes(&inner_buf).ok();
        }
        QueryResult::DebugEpochState {
            treasury,
            reserves,
            snap_mark,
            snap_set,
            snap_go,
            snap_fee,
        } => {
            encode_debug_epoch_state(
                enc, *treasury, *reserves, snap_mark, snap_set, snap_go, *snap_fee,
            );
        }
        QueryResult::DebugNewEpochState {
            epoch,
            blocks_made_prev,
            blocks_made_cur,
            treasury,
            reserves,
            snap_mark,
            snap_set,
            snap_go,
            snap_fee,
            total_active_stake,
            pool_distr,
        } => {
            encode_debug_new_epoch_state(
                enc,
                *epoch,
                blocks_made_prev,
                blocks_made_cur,
                *treasury,
                *reserves,
                snap_mark,
                snap_set,
                snap_go,
                *snap_fee,
                *total_active_stake,
                pool_distr,
            );
        }
        QueryResult::DebugChainDepState {
            last_slot,
            last_slot_is_origin,
            ocert_counters,
            evolving_nonce,
            candidate_nonce,
            epoch_nonce,
            previous_epoch_nonce,
            lab_nonce,
            last_epoch_block_nonce,
        } => {
            encode_debug_chain_dep_state(
                enc,
                *last_slot,
                *last_slot_is_origin,
                ocert_counters,
                evolving_nonce,
                candidate_nonce,
                epoch_nonce,
                previous_epoch_nonce,
                lab_nonce,
                last_epoch_block_nonce,
            );
        }
        QueryResult::RewardProvenance {
            epoch,
            total_rewards_pot,
            treasury_tax,
            active_stake,
        } => {
            // Reward provenance: array(4) [epoch, rewards_pot, treasury_tax, active_stake]
            enc.array(4).ok();
            enc.u64(*epoch).ok();
            enc.u64(*total_rewards_pot).ok();
            enc.u64(*treasury_tax).ok();
            enc.u64(*active_stake).ok();
        }
        QueryResult::RewardInfoPools(pools) => {
            encode_reward_info_pools(enc, pools);
        }
        QueryResult::HardForkCurrentEra(era) => {
            // QueryHardFork GetCurrentEra result: EraIndex as raw word8
            enc.u8(*era as u8).ok();
        }
        QueryResult::Proposals(proposals) => {
            // GetProposals result: Seq (GovActionState) = OSet of GovActionState
            enc.array(proposals.len() as u64).ok();
            for p in proposals {
                encode_gov_action_state(enc, p);
            }
        }
        QueryResult::RatifyState {
            gov,
            enacted,
            expired,
            delayed,
        } => {
            encode_ratify_state(enc, gov, enacted, expired, *delayed);
        }
        QueryResult::NoFuturePParams => {
            // GetFuturePParams result: Maybe PParams = Nothing
            // Haskell encodeMaybe: Nothing = encodeListLen 0 = empty array (0x80)
            enc.array(0).ok();
        }
        QueryResult::PoolDistr2 {
            pools,
            total_active_stake,
        } => {
            encode_pool_distr2(enc, pools, *total_active_stake);
        }
        QueryResult::StakeDistribution2 {
            pools,
            total_active_stake,
        } => {
            encode_stake_distribution2(enc, pools, *total_active_stake);
        }
        QueryResult::MaxMajorProtocolVersion(v) => {
            // Plain integer
            enc.u32(*v).ok();
        }
        QueryResult::LedgerPeerSnapshot(peers) => {
            encode_ledger_peer_snapshot(enc, peers);
        }
        QueryResult::LedgerPeerSnapshotV23 {
            big,
            anchor,
            network_magic,
            peers,
        } => {
            if *big {
                encode_ledger_peer_snapshot_v23_big(enc, anchor, *network_magic, peers);
            } else {
                encode_ledger_peer_snapshot_v23_all(enc, anchor, *network_magic, peers);
            }
        }
        QueryResult::StakePoolDefaultVote(vote) => {
            // Bare word8: 0=DefaultNo, 1=DefaultAbstain, 2=DefaultNoConfidence
            enc.u8(*vote).ok();
        }
        QueryResult::SPOStakeDistr(entries) => {
            // Map<pool_hash(28), Coin> — plain map from pool key hash to lovelace
            enc.map(entries.len() as u64).ok();
            for (pool_id, stake) in entries {
                enc.bytes(pool_id).ok();
                enc.u64(*stake).ok();
            }
        }
        QueryResult::Error(msg) => {
            enc.str(msg).ok();
        }
    }
}

// ─── UTxO encoding ───────────────────────────────────────────────────────────

/// Encode a UTxO output in PostAlonzo format (CBOR map with integer keys).
///
/// Format: `{0: address_bytes, 1: value, 2?: datum_option, 3?: script_ref}`
/// Value: `coin` (integer) or `[coin, {policy_id -> {asset_name -> quantity}}]`
///
/// Key 3 (script_ref) is emitted when `utxo.script_ref` is `Some`.  The wire
/// encoding is `tag(24) bstr(encode_script_ref(sr))` matching the Babbage/Conway
/// CDDL: `3 => #6.24(bytes .cbor script)`.
pub(crate) fn encode_utxo_output(enc: &mut minicbor::Encoder<&mut Vec<u8>>, utxo: &UtxoSnapshot) {
    let has_datum = utxo.datum_hash.is_some() || utxo.inline_datum.is_some();
    let has_script_ref = utxo.script_ref.is_some();
    let field_count = 2 + has_datum as u64 + has_script_ref as u64;
    enc.map(field_count).ok();

    // 0: address (raw bytes)
    enc.u32(0).ok();
    enc.bytes(&utxo.address_bytes).ok();

    // 1: value
    enc.u32(1).ok();
    if utxo.multi_asset.is_empty() {
        // Coin-only: encode as plain integer
        enc.u64(utxo.lovelace).ok();
    } else {
        // Multi-asset: [coin, {policy_id -> {asset_name -> quantity}}]
        enc.array(2).ok();
        enc.u64(utxo.lovelace).ok();
        enc.map(utxo.multi_asset.len() as u64).ok();
        for (policy_id, assets) in &utxo.multi_asset {
            enc.bytes(policy_id).ok();
            enc.map(assets.len() as u64).ok();
            for (asset_name, quantity) in assets {
                enc.bytes(asset_name).ok();
                enc.u64(*quantity).ok();
            }
        }
    }

    // 2: datum_option
    //
    // Per Conway CDDL:
    //     datum_option = [0, $hash32]     ; hashed datum (legacy)
    //                  // [1, data]       ; inline datum
    //     data         = #6.24(bytes .cbor data)
    //
    // The discriminator is the leading integer: 0 = hashed, 1 = inline.
    // Inline datums are CBOR-tag-24-wrapped to indicate "embedded CBOR
    // datum bytes" — cardano-cli's auto-balance evaluator unwraps this
    // when constructing the `ScriptContext.txInfoOutputs` datum field.
    // Hashed datums are mutually exclusive with inline; only one can be
    // present.
    if let Some(ref inline_datum) = utxo.inline_datum {
        enc.u32(2).ok();
        enc.array(2).ok();
        enc.u32(1).ok();
        enc.tag(minicbor::data::Tag::new(24)).ok();
        enc.bytes(inline_datum).ok();
    } else if let Some(ref datum_hash) = utxo.datum_hash {
        enc.u32(2).ok();
        // DatumOption::Hash variant: [0, datum_hash]
        enc.array(2).ok();
        enc.u32(0).ok();
        enc.bytes(datum_hash).ok();
    }

    // 3: script_ref (if present)
    // Wire: 3 => tag(24) bstr(<encode_script_ref bytes>)
    // This is the Babbage/Conway CDDL `script_ref = #6.24(bytes .cbor script)`.
    // `encode_script_ref` returns `array(2)[variant_tag, script_bytes]`.
    if let Some(ref script_ref) = utxo.script_ref {
        enc.u32(3).ok();
        enc.tag(minicbor::data::Tag::new(24)).ok();
        let script_cbor = dugite_serialization::encode_script_ref(script_ref);
        enc.bytes(&script_cbor).ok();
    }
}

fn encode_utxo_by_address(enc: &mut minicbor::Encoder<&mut Vec<u8>>, utxos: &[UtxoSnapshot]) {
    // Cardano wire format: Map<[tx_hash, index], TransactionOutput>
    enc.map(utxos.len() as u64).ok();
    for utxo in utxos {
        // Key: [tx_hash, index]
        enc.array(2).ok();
        enc.bytes(&utxo.tx_hash).ok();
        enc.u32(utxo.output_index).ok();

        // Value: use pre-encoded raw CBOR if available (preserves original
        // wire format from the ledger), otherwise re-encode from snapshot fields.
        if let Some(raw) = &utxo.raw_cbor {
            enc.writer_mut().extend_from_slice(raw);
        } else {
            encode_utxo_output(enc, utxo);
        }
    }
}

// ─── Protocol parameters encoding ────────────────────────────────────────────

/// Encode protocol parameters as a positional CBOR array(31) per Haskell ConwayPParams.
///
/// The Haskell reference uses `encCBOR` derived from `eraPParams @ConwayEra`
/// (cardano-ledger eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs), which
/// emits one entry per `PParam` lens in the order they are declared in the
/// `cppHKDLensMap`. The protocolVersion is appended LAST via
/// `ppGovProtocolVersion` since Conway moved it out of the updatable map.
///
/// V21+ field order (matches cardano-cli 10.15 decoder, see issue #336):
///   [0] txFeePerByte,    [1] txFeeFixed,      [2] maxBBSize,
///   [3] maxTxSize,       [4] maxBHSize,       [5] keyDeposit,
///   [6] poolDeposit,     [7] eMax,            [8] nOpt,
///   [9] a0,              [10] rho,            [11] tau,
///   [12] minPoolCost,    [13] coinsPerUTxOByte, [14] costModels,
///   [15] prices,         [16] maxTxExUnits,   [17] maxBlockExUnits,
///   [18] maxValSize,     [19] collateralPct,  [20] maxCollateralInputs,
///   [21] poolVotingThresholds(5),
///   [22] drepVotingThresholds(10),
///   [23] committeeMinSize, [24] committeeMaxTermLength,
///   [25] govActionLifetime, [26] govActionDeposit,
///   [27] drepDeposit,    [28] drepActivity,
///   [29] minFeeRefScriptCostPerByte,
///   [30] protocolVersion = array(2)[major, minor]   (LAST — see oracle)
pub(crate) fn encode_protocol_params_cbor(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pp: &ProtocolParamsSnapshot,
) {
    enc.array(31).ok();

    // [0] txFeePerByte (min_fee_a)
    enc.u64(pp.min_fee_a).ok();
    // [1] txFeeFixed (min_fee_b)
    enc.u64(pp.min_fee_b).ok();
    // [2] maxBlockBodySize
    enc.u64(pp.max_block_body_size).ok();
    // [3] maxTxSize
    enc.u64(pp.max_tx_size).ok();
    // [4] maxBlockHeaderSize
    enc.u64(pp.max_block_header_size).ok();
    // [5] keyDeposit
    enc.u64(pp.key_deposit).ok();
    // [6] poolDeposit
    enc.u64(pp.pool_deposit).ok();
    // [7] eMax
    enc.u64(pp.e_max).ok();
    // [8] nOpt
    enc.u64(pp.n_opt).ok();

    // [9] a0 (rational as tag 30)
    encode_tagged_rational(enc, pp.a0_num, pp.a0_den);
    // [10] rho
    encode_tagged_rational(enc, pp.rho_num, pp.rho_den);
    // [11] tau
    encode_tagged_rational(enc, pp.tau_num, pp.tau_den);

    // [12] protocolVersion = array(2)[major, minor]
    // Per Haskell's `eraPParams @ConwayEra`, `ppGovProtocolVersion` sits at
    // position 12 (between tau and minPoolCost). It is a `HKDNoUpdate`
    // field — present in PParams but not in PParamsUpdate.
    enc.array(2).ok();
    enc.u64(pp.protocol_version_major).ok();
    enc.u64(pp.protocol_version_minor).ok();

    // [13] minPoolCost
    enc.u64(pp.min_pool_cost).ok();
    // [14] coinsPerUTxOByte (utxoCostPerByte)
    enc.u64(pp.ada_per_utxo_byte).ok();

    // [15] costModels — Haskell `EncCBOR CostModels` = `encCBOR .
    // flattenCostModels`, a CBOR map keyed by ascending Word8 language:
    // {0: [v1], 1: [v2], 2: [v3], 3: [v4], <unknown keys ≥ 4>}.
    {
        let cm_count = pp.cost_models_v1.is_some() as u64
            + pp.cost_models_v2.is_some() as u64
            + pp.cost_models_v3.is_some() as u64
            + pp.cost_models_v4.is_some() as u64
            + pp.cost_models_unknown.len() as u64;
        enc.map(cm_count).ok();
        if let Some(ref v1) = pp.cost_models_v1 {
            enc.u32(0).ok();
            enc.array(v1.len() as u64).ok();
            for cost in v1 {
                enc.i64(*cost).ok();
            }
        }
        if let Some(ref v2) = pp.cost_models_v2 {
            enc.u32(1).ok();
            enc.array(v2.len() as u64).ok();
            for cost in v2 {
                enc.i64(*cost).ok();
            }
        }
        if let Some(ref v3) = pp.cost_models_v3 {
            enc.u32(2).ok();
            enc.array(v3.len() as u64).ok();
            for cost in v3 {
                enc.i64(*cost).ok();
            }
        }
        // PlutusV4 (Dijkstra, key 3).
        if let Some(ref v4) = pp.cost_models_v4 {
            enc.u32(3).ok();
            enc.array(v4.len() as u64).ok();
            for cost in v4 {
                enc.i64(*cost).ok();
            }
        }
        // #770: unknown-language entries (keys ≥ 4) in ascending key order.
        for (key, costs) in &pp.cost_models_unknown {
            enc.u32(u32::from(*key)).ok();
            enc.array(costs.len() as u64).ok();
            for cost in costs {
                enc.i64(*cost).ok();
            }
        }
    }

    // [16] prices [mem_price, step_price] as tagged rationals
    enc.array(2).ok();
    encode_tagged_rational(enc, pp.execution_costs_mem_num, pp.execution_costs_mem_den);
    encode_tagged_rational(
        enc,
        pp.execution_costs_step_num,
        pp.execution_costs_step_den,
    );

    // [17] maxTxExUnits [mem, steps]
    enc.array(2).ok();
    enc.u64(pp.max_tx_ex_mem).ok();
    enc.u64(pp.max_tx_ex_steps).ok();

    // [18] maxBlockExUnits [mem, steps]
    enc.array(2).ok();
    enc.u64(pp.max_block_ex_mem).ok();
    enc.u64(pp.max_block_ex_steps).ok();

    // [19] maxValSize
    enc.u64(pp.max_val_size).ok();
    // [20] collateralPercentage
    enc.u64(pp.collateral_percentage).ok();
    // [21] maxCollateralInputs
    enc.u64(pp.max_collateral_inputs).ok();

    // [22] poolVotingThresholds (5 tagged rationals)
    enc.array(5).ok();
    encode_tagged_rational(
        enc,
        pp.pvt_motion_no_confidence_num,
        pp.pvt_motion_no_confidence_den,
    );
    encode_tagged_rational(
        enc,
        pp.pvt_committee_normal_num,
        pp.pvt_committee_normal_den,
    );
    encode_tagged_rational(
        enc,
        pp.pvt_committee_no_confidence_num,
        pp.pvt_committee_no_confidence_den,
    );
    encode_tagged_rational(enc, pp.pvt_hard_fork_num, pp.pvt_hard_fork_den);
    encode_tagged_rational(
        enc,
        pp.pvt_pp_security_group_num,
        pp.pvt_pp_security_group_den,
    );

    // [23] drepVotingThresholds (10 tagged rationals)
    enc.array(10).ok();
    encode_tagged_rational(enc, pp.dvt_no_confidence_num, pp.dvt_no_confidence_den);
    encode_tagged_rational(
        enc,
        pp.dvt_committee_normal_num,
        pp.dvt_committee_normal_den,
    );
    encode_tagged_rational(
        enc,
        pp.dvt_committee_no_confidence_num,
        pp.dvt_committee_no_confidence_den,
    );
    encode_tagged_rational(enc, pp.dvt_constitution_num, pp.dvt_constitution_den);
    encode_tagged_rational(enc, pp.dvt_hard_fork_num, pp.dvt_hard_fork_den);
    encode_tagged_rational(
        enc,
        pp.dvt_pp_network_group_num,
        pp.dvt_pp_network_group_den,
    );
    encode_tagged_rational(
        enc,
        pp.dvt_pp_economic_group_num,
        pp.dvt_pp_economic_group_den,
    );
    encode_tagged_rational(
        enc,
        pp.dvt_pp_technical_group_num,
        pp.dvt_pp_technical_group_den,
    );
    encode_tagged_rational(enc, pp.dvt_pp_gov_group_num, pp.dvt_pp_gov_group_den);
    encode_tagged_rational(
        enc,
        pp.dvt_treasury_withdrawal_num,
        pp.dvt_treasury_withdrawal_den,
    );

    // [24] committeeMinSize
    enc.u64(pp.committee_min_size).ok();
    // [25] committeeMaxTermLength
    enc.u64(pp.committee_max_term_length).ok();
    // [26] govActionLifetime
    enc.u64(pp.gov_action_lifetime).ok();
    // [27] govActionDeposit
    enc.u64(pp.gov_action_deposit).ok();
    // [28] drepDeposit
    enc.u64(pp.drep_deposit).ok();
    // [29] drepActivity
    enc.u64(pp.drep_activity).ok();

    // [30] minFeeRefScriptCostPerByte (NonNegativeInterval, tagged rational)
    encode_tagged_rational(
        enc,
        pp.min_fee_ref_script_cost_per_byte_num,
        pp.min_fee_ref_script_cost_per_byte_den,
    );
}

/// Helper to encode a tagged rational number: `tag(30)[numerator, denominator]`
/// Encode a stake fraction the way Haskell's `Rational` reaches the wire.
///
/// `individualPoolStake` is a genuine `Data.Ratio.Ratio Integer`, and `%`
/// always reduces via gcd, so cardano-node emits the fraction in lowest terms.
/// Emitting `stake/total` unreduced produces a numerically equal but
/// byte-different response (#905).
pub(crate) fn encode_reduced_rational(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    num: u64,
    den: u64,
) {
    fn gcd(mut a: u64, mut b: u64) -> u64 {
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        a
    }
    if den == 0 {
        encode_tagged_rational(enc, 0, 1);
        return;
    }
    let g = gcd(num, den).max(1);
    encode_tagged_rational(enc, num / g, den / g);
}

pub(crate) fn encode_tagged_rational(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    num: u64,
    den: u64,
) {
    enc.tag(minicbor::data::Tag::new(30)).ok();
    enc.array(2).ok();
    enc.u64(num).ok();
    enc.u64(den).ok();
}

// ─── Pool encoding ────────────────────────────────────────────────────────────

/// Encode a `Map<pool_hash(28), PoolParams>` for pool state queries.
pub(crate) fn encode_pool_params_map(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    params: &[crate::node::n2c_query::types::PoolParamsSnapshot],
) {
    enc.map(params.len() as u64).ok();
    for pool in params {
        enc.bytes(&pool.pool_id).ok();
        enc.array(9).ok();
        enc.bytes(&pool.pool_id).ok(); // operator
        enc.bytes(&pool.vrf_keyhash).ok();
        enc.u64(pool.pledge).ok();
        enc.u64(pool.cost).ok();
        encode_tagged_rational(enc, pool.margin_num, pool.margin_den);
        enc.bytes(&pool.reward_account).ok();
        // owners as tag(258) set — sorted for canonical CBOR
        let mut sorted_owners = pool.owners.clone();
        sorted_owners.sort();
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(sorted_owners.len() as u64).ok();
        for owner in &sorted_owners {
            enc.bytes(owner).ok();
        }
        // relays
        enc.array(pool.relays.len() as u64).ok();
        for relay in &pool.relays {
            encode_relay_cbor(enc, relay);
        }
        // metadata
        if let Some(url) = &pool.metadata_url {
            enc.array(2).ok();
            enc.str(url).ok();
            if let Some(hash) = &pool.metadata_hash {
                enc.bytes(hash).ok();
            } else {
                enc.bytes(&[0u8; 32]).ok();
            }
        } else {
            enc.null().ok();
        }
    }
}

fn encode_pool_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pool_params: &[crate::node::n2c_query::types::PoolParamsSnapshot],
    future_pool_params: &[crate::node::n2c_query::types::PoolParamsSnapshot],
    retiring: &[(Vec<u8>, u64)],
    deposits: &[(Vec<u8>, u64)],
) {
    // QueryPoolStateResult: array(4) [poolParams, futurePoolParams, retiring, deposits]
    enc.array(4).ok();
    // Map 0: current pool params
    encode_pool_params_map(enc, pool_params);
    // Map 1: future pool params
    encode_pool_params_map(enc, future_pool_params);
    // Map 2: retiring pools -> epoch
    enc.map(retiring.len() as u64).ok();
    for (pool_id, epoch) in retiring {
        enc.bytes(pool_id).ok();
        enc.u64(*epoch).ok();
    }
    // Map 3: deposits
    enc.map(deposits.len() as u64).ok();
    for (pool_id, coin) in deposits {
        enc.bytes(pool_id).ok();
        enc.u64(*coin).ok();
    }
}

/// `GetStakeDistribution2` (tag 37) — `poolsByTotalStakeFraction`.
///
/// Wire-identical to `encode_pool_distr2` but a different computation, and the
/// two must not be merged back together (#964).
///
///   poolsByTotalStakeFraction globals nes = PoolDistr poolsByTotalStake totalActiveStake
///     where stakeRatio  = totalActiveStake %? getTotalStake globals nes
///           poolsByTotalStake = Map.map (\(IndividualPoolStake s c vrf) ->
///                                 IndividualPoolStake (s * stakeRatio) c vrf) ...
///
/// `s` is already `stake / activeStake`, so `s * (activeStake / circulation)`
/// is `stake / circulation` — which is what #905 established and what this
/// encoder writes. `individualTotalPoolStake` and `pdTotalActiveStake` are NOT
/// rescaled; only the ratio is. `currentSnapshot` is built from the LIVE
/// instant stake, so `stake_pools` (live) is the right source here.
fn encode_stake_distribution2(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pools: &[crate::node::n2c_query::types::StakePoolSnapshot],
    total_active_stake: u64,
) {
    enc.array(2).ok();
    let live: Vec<_> = pools.iter().filter(|p| p.stake > 0).collect();
    enc.map(live.len() as u64).ok();
    for pool in live {
        enc.bytes(&pool.pool_id).ok();
        enc.array(3).ok();
        encode_reduced_rational(enc, pool.stake, pool.total_circulation);
        enc.u64(pool.stake).ok();
        enc.bytes(&pool.vrf_keyhash).ok();
    }
    enc.u64(total_active_stake).ok();
}

/// `GetPoolDistr` (tag 21) — the pre-V21 wire shape of the same distribution.
///
/// `IndividualPoolStake` here is `array(2)` (ratio + VRF hash) rather than
/// tag 36's `array(3)`, but the DATA is identical: same `set` snapshot, same
/// `spssStake / ssTotalActiveStake` ratio, same zero-delegator filter.
fn encode_pool_distr_legacy(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pools: &[crate::node::n2c_query::types::PoolDistrEntry],
    total_active_stake: u64,
) {
    let total = total_active_stake.max(1);
    let included: Vec<_> = pools.iter().filter(|p| p.delegator_count > 0).collect();
    enc.map(included.len() as u64).ok();
    for pool in included {
        enc.bytes(&pool.pool_id).ok();
        enc.array(2).ok();
        // Reduced, like every other rational on this wire: a Haskell `Rational`
        // is in lowest terms by construction (`%` normalises), and
        // `spssStakeRatio` is built with `%.`. This arm wrote the raw
        // numerator/denominator, so it disagreed with tag 36 byte-for-byte even
        // once both used the same denominator.
        encode_reduced_rational(enc, pool.stake, total);
        enc.bytes(&pool.vrf_keyhash).ok();
    }
}

fn encode_pool_distr2(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pools: &[crate::node::n2c_query::types::PoolDistrEntry],
    total_active_stake: u64,
) {
    // SL.PoolDistr: array(2)[pool_map, total_active_stake]
    // Each pool entry: array(3)[stake_rational, compact_lovelace, vrf_hash]
    enc.array(2).ok();
    //
    // #964: the ratio is `spssStakeRatio = spssStake / ssTotalActiveStake` of
    // the `set` snapshot, with NO rescale.
    //
    //   calculatePoolDistr' includeHash (SnapShot _ activeStake stakePoolSnapShot) =
    //     ... IndividualPoolStake { individualPoolStake = spssStakeRatio spss, ... }
    //     ... pdTotalActiveStake = activeStake
    //
    // This used to divide by TOTAL CIRCULATION, citing the
    // `GetStakeDistribution` arm. That citation is real but belongs to a
    // *different* function: `poolsByTotalStakeFraction` (tag 37) computes
    // `calculatePoolDistr` and then rescales every ratio by
    // `totalActiveStake / circulation`, which is #905. `calculatePoolDistr'`
    // (tags 21/36) performs no such rescale.
    //
    // Since circulation ≫ active stake, the wrong denominator shrank σ by that
    // whole factor — and `cardano-cli query leadership-schedule` reads σ
    // straight out of this answer, so a pool was told it led roughly half the
    // slots cardano-node said it led.
    //
    // Note the two encoders DISAGREED with each other: the tag-21 arm above
    // divides by `total_active_stake` and is right, while tag 36 — the only
    // one a V21+ client can reach, since tag 21 is deprecated there — was
    // wrong. The reachable arm broken and the dead arm correct is the #978
    // inversion, again.
    //
    // `guard (spssNumDelegators spss > 0)` drops pools with NO DELEGATORS, not
    // pools with no stake: a pool with delegators whose stake is zero stays in
    // the map at ratio 0.
    let included: Vec<_> = pools.iter().filter(|p| p.delegator_count > 0).collect();
    enc.map(included.len() as u64).ok();
    for pool in included {
        enc.bytes(&pool.pool_id).ok();
        enc.array(3).ok();
        encode_reduced_rational(enc, pool.stake, total_active_stake.max(1));
        // `individualTotalPoolStake` — the pool's absolute stake.
        enc.u64(pool.stake).ok();
        // `individualPoolStakeVrf`.
        enc.bytes(&pool.vrf_keyhash).ok();
    }
    // `pdTotalActiveStake`.
    enc.u64(total_active_stake).ok();
}

// ─── Stake encoding ───────────────────────────────────────────────────────────

fn encode_stake_address_info(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    addrs: &[crate::node::n2c_query::types::StakeAddressSnapshot],
) {
    // Wire format: array(2) [delegations_map, rewards_map]
    // delegations_map: Map<Credential, pool_hash(28)>
    // rewards_map: Map<Credential, Coin>
    // Credential: [0, hash(28)] for KeyHash
    let delegated: Vec<_> = addrs
        .iter()
        .filter(|a| a.delegated_pool.is_some())
        .collect();
    enc.array(2).ok();
    // Delegations map
    enc.map(delegated.len() as u64).ok();
    for addr in &delegated {
        // Credential key
        enc.array(2).ok();
        enc.u32(0).ok(); // KeyHashObj
        enc.bytes(&addr.credential_hash).ok();
        // Pool hash value
        if let Some(pool) = addr.delegated_pool.as_ref() {
            enc.bytes(pool).ok();
        } else {
            enc.bytes(&[]).ok();
        }
    }
    // Rewards map
    enc.map(addrs.len() as u64).ok();
    for addr in addrs {
        // Credential key
        enc.array(2).ok();
        enc.u32(0).ok(); // KeyHashObj
        enc.bytes(&addr.credential_hash).ok();
        // Reward balance value
        enc.u64(addr.reward_balance).ok();
    }
}

fn encode_stake_snapshots(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    snapshots: &crate::node::n2c_query::types::StakeSnapshotsResult,
) {
    // Wire format: array(4) [pool_map, mark_total, set_total, go_total]
    // pool_map: Map<pool_hash(28), array(3) [mark_stake, set_stake, go_stake]>
    enc.array(4).ok();
    enc.map(snapshots.pools.len() as u64).ok();
    for pool in &snapshots.pools {
        enc.bytes(&pool.pool_id).ok();
        enc.array(3).ok();
        enc.u64(pool.mark_stake).ok();
        enc.u64(pool.set_stake).ok();
        enc.u64(pool.go_stake).ok();
    }
    // Totals (NonZero Coin — must be >= 1)
    enc.u64(snapshots.total_mark_stake.max(1)).ok();
    enc.u64(snapshots.total_set_stake.max(1)).ok();
    enc.u64(snapshots.total_go_stake.max(1)).ok();
}

fn encode_stake_pools(enc: &mut minicbor::Encoder<&mut Vec<u8>>, pool_ids: &[Vec<u8>]) {
    // Wire format: tag(258) Set<KeyHash StakePool>
    // CBOR canonical Set requires elements in sorted order
    let mut sorted_ids = pool_ids.to_owned();
    sorted_ids.sort();
    enc.tag(minicbor::data::Tag::new(258)).ok();
    enc.array(sorted_ids.len() as u64).ok();
    for pid in &sorted_ids {
        enc.bytes(pid).ok();
    }
}

fn encode_drep_stake_distr(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    entries: &[crate::node::n2c_query::types::DRepStakeEntry],
) {
    // Wire format: Map<DRep, Coin>
    // DRep: [0, keyhash(28)] | [1, scripthash(28)] | [2] | [3]
    enc.map(entries.len() as u64).ok();
    for entry in entries {
        match entry.drep_type {
            0 | 1 => {
                enc.array(2).ok();
                enc.u8(entry.drep_type).ok();
                if let Some(ref h) = entry.drep_hash {
                    enc.bytes(h).ok();
                }
            }
            _ => {
                enc.array(1).ok();
                enc.u8(entry.drep_type).ok();
            }
        }
        enc.u64(entry.stake).ok();
    }
}

fn encode_filtered_vote_delegatees(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    delegatees: &[crate::node::n2c_query::types::VoteDelegateeEntry],
) {
    // Wire format: Map<Credential, DRep>
    // Credential: [0|1, hash(28)]
    // DRep: [0, keyhash(28)] | [1, scripthash(28)] | [2] | [3]
    enc.map(delegatees.len() as u64).ok();
    for entry in delegatees {
        // Key: Credential
        enc.array(2).ok();
        enc.u8(entry.credential_type).ok();
        enc.bytes(&entry.credential_hash).ok();
        // Value: DRep
        match entry.drep_type {
            0 | 1 => {
                enc.array(2).ok();
                enc.u8(entry.drep_type).ok();
                if let Some(ref h) = entry.drep_hash {
                    enc.bytes(h).ok();
                }
            }
            _ => {
                enc.array(1).ok();
                enc.u8(entry.drep_type).ok();
            }
        }
    }
}

/// Encode `GetDRepDelegations` (tag 39, V23+) response.
///
/// Wire format per Haskell `ouroboros-consensus-cardano`
/// `Shelley/Ledger/Query.hs`:
///
/// ```text
/// Map<DRep, Set<Credential Staking>>
///   key   DRep        = array(2) [0|1, bstr(28)]
///                     | array(1) [2|3]                  (AlwaysAbstain / AlwaysNoConfidence)
///   value Set<Cred>   = tag(258) array(n) [Credential...]
///   Credential        = array(2) [0|1, bstr(28)]
/// ```
///
/// This is the OPPOSITE orientation of `GetFilteredVoteDelegatees` (tag 28),
/// which is keyed by stake credential.
fn encode_drep_delegations(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    groups: &[DRepDelegationGroup],
) {
    enc.map(groups.len() as u64).ok();
    for group in groups {
        // Key: DRep
        encode_drep_key(enc, &group.drep);
        // Value: Set<Credential> = tag(258) array(n) [array(2) [type, hash], ...]
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(group.credentials.len() as u64).ok();
        for (cred_type, cred_hash) in &group.credentials {
            enc.array(2).ok();
            enc.u8(*cred_type).ok();
            enc.bytes(cred_hash).ok();
        }
    }
}

/// Encode a wire-format DRep value (used as the map key for tag 39).
fn encode_drep_key(enc: &mut minicbor::Encoder<&mut Vec<u8>>, drep: &DRepKey) {
    match drep.drep_type {
        0 | 1 => {
            enc.array(2).ok();
            enc.u8(drep.drep_type).ok();
            if let Some(ref h) = drep.drep_hash {
                enc.bytes(h).ok();
            } else {
                // Defensive: a KeyHash/ScriptHash DRep with no hash is malformed.
                enc.bytes(&[]).ok();
            }
        }
        _ => {
            enc.array(1).ok();
            enc.u8(drep.drep_type).ok();
        }
    }
}

// ─── Governance encoding ──────────────────────────────────────────────────────

fn encode_gov_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    gov: &crate::node::n2c_query::types::GovStateSnapshot,
) {
    // ConwayGovState = array(7):
    //   [0] Proposals, [1] Committee, [2] Constitution,
    //   [3] curPParams, [4] prevPParams, [5] FuturePParams,
    //   [6] DRepPulsingState
    enc.array(7).ok();

    // [0] Proposals = array(2) [roots, values]
    enc.array(2).ok();
    // roots = array(4) of StrictMaybe GovPurposeId
    // Order: [PParamUpdate, HardFork, Committee, Constitution]
    enc.array(4).ok();
    let roots = [
        &gov.enacted_pparam_update,
        &gov.enacted_hard_fork,
        &gov.enacted_committee,
        &gov.enacted_constitution,
    ];
    for root in &roots {
        match root {
            Some((tx_hash, action_index)) => {
                // StrictMaybe Just = array(1) [GovActionId]
                enc.array(1).ok();
                enc.array(2).ok();
                enc.bytes(tx_hash).ok();
                enc.u32(*action_index).ok();
            }
            None => {
                enc.array(0).ok(); // StrictMaybe Nothing = array(0)
            }
        }
    }
    // values = array(n) of GovActionState
    enc.array(gov.proposals.len() as u64).ok();
    for p in &gov.proposals {
        encode_gov_action_state(enc, p);
    }

    // [1] Committee = StrictMaybe(array(2) [Map<ColdCred,EpochNo>, UnitInterval])
    if gov.committee.members.is_empty() && gov.committee.threshold.is_none() {
        enc.array(0).ok(); // StrictMaybe Nothing
    } else {
        enc.array(1).ok(); // StrictMaybe Just
        enc.array(2).ok();
        // Map<ColdCredential, EpochNo>
        enc.map(gov.committee.members.len() as u64).ok();
        for m in &gov.committee.members {
            // Key: Credential [type, hash]
            enc.array(2).ok();
            enc.u8(m.cold_credential_type).ok();
            enc.bytes(&m.cold_credential).ok();
            // Value: expiry epoch
            enc.u64(m.expiry_epoch.unwrap_or(0)).ok();
        }
        // UnitInterval (quorum threshold)
        if let Some((num, den)) = gov.committee.threshold {
            encode_tagged_rational(enc, num, den);
        } else {
            encode_tagged_rational(enc, 2, 3); // default 2/3
        }
    }

    // [2] Constitution = array(2) [Anchor, StrictMaybe ScriptHash]
    enc.array(2).ok();
    // Anchor = array(2) [url, hash]
    enc.array(2).ok();
    enc.str(&gov.constitution_url).ok();
    enc.bytes(&gov.constitution_hash).ok();
    // StrictMaybe ScriptHash (null-encoded: null=Nothing, bytes=Just)
    if let Some(ref script) = gov.constitution_script {
        enc.bytes(script).ok();
    } else {
        enc.null().ok();
    }

    // [3] curPParams = array(31)
    encode_protocol_params_cbor(enc, &gov.cur_pparams);

    // [4] prevPParams = array(31)
    encode_protocol_params_cbor(enc, &gov.prev_pparams);

    // [5] FuturePParams (#977) — a real tagged sum, not a constant.
    //
    // Verified against real preview epoch-1259 bytes by
    // `dugite-serialization`'s `decode_future_pparams`:
    //
    //   NoPParamsUpdate          -> array(1) [0]
    //   DefinitePParamsUpdate pp -> array(2) [1, pp]
    //   PotentialPParamsUpdate m -> array(2) [2, <array(0) | array(1) [pp]>]
    //
    // The inner value of tag 2 is a `StrictMaybe`: `array(0)` for SNothing,
    // `array(1) [pp]` for SJust. This was hardcoded to tag 0 until #977, which
    // is right for the LATER part of every epoch and wrong for the earlier
    // part — on mainnet, the first ~40%.
    match (gov.future_pparams_tag, gov.future_pparams.as_ref()) {
        // Definite ALWAYS carries the params, directly — no StrictMaybe
        // wrapper, unlike Potential below.
        (1, Some(pp)) => {
            enc.array(2).ok();
            enc.u32(1).ok();
            encode_protocol_params_cbor(enc, pp);
        }
        // A Definite update with no payload is not representable upstream, so
        // the tag must NOT be written at all: degrade wholly to
        // NoPParamsUpdate rather than emit a frame cardano-cli cannot decode.
        (1, None) => {
            enc.array(1).ok();
            enc.u32(0).ok();
        }
        (2, payload) => {
            enc.array(2).ok();
            enc.u32(2).ok();
            match payload {
                Some(pp) => {
                    enc.array(1).ok(); // SJust
                    encode_protocol_params_cbor(enc, pp);
                }
                None => {
                    enc.array(0).ok(); // SNothing
                }
            }
        }
        _ => {
            enc.array(1).ok();
            enc.u32(0).ok();
        }
    }

    // [6] DRepPulsingState — always the `DRComplete` form (#992).
    //
    // ```haskell
    // instance EncCBOR (DRepPulsingState era) where
    //   encCBOR (DRComplete x y) = encode (Rec DRComplete !> To x !> To y)
    //   encCBOR x@(DRPulsing (DRepPulser {})) = encode (Rec DRComplete !> To snap !> To ratstate)
    //     where (snap, ratstate) = finishDRepPulser x
    // ```
    //
    // A `Rec` with no constructor tag: array(2) [PulsingSnapshot, RatifyState].
    //
    // This was hardcoded EMPTY — four empty collections and `rsDelayed =
    // false` — beside an `EnactState` that was a second hand-written copy of
    // the one in `encode_ratify_state`. Both halves of that are now gone: the
    // real pulser is encoded, and the `RatifyState` (EnactState included) is
    // produced by the SAME function tag 32 uses, so tag 24 and tag 32 cannot
    // drift apart.
    enc.array(2).ok();

    // PulsingSnapshot = array(4):
    //   [0] psProposals:  StrictSeq GovActionState (CBOR array, not map)
    //   [1] psDRepDistr:  Map DRep (CompactForm Coin)
    //   [2] psDRepState:  Map (Credential DRepRole) DRepState
    //   [3] psPoolDistr:  Map (KeyHash StakePool) (CompactForm Coin)
    enc.array(4).ok();
    enc.array(gov.pulser_proposals.len() as u64).ok();
    for p in &gov.pulser_proposals {
        encode_gov_action_state(enc, p);
    }
    encode_drep_stake_distr(enc, &gov.pulser_drep_distr);
    encode_drep_state(enc, &gov.pulser_drep_state);
    enc.map(gov.pulser_pool_distr.len() as u64).ok();
    for pool in &gov.pulser_pool_distr {
        enc.bytes(&pool.pool_id).ok();
        enc.u64(pool.stake).ok();
    }

    // RatifyState — shared with tag 32.
    encode_ratify_state(
        enc,
        gov,
        &gov.ratify_enacted,
        &gov.ratify_expired,
        gov.ratify_delayed,
    );
}

fn encode_drep_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    dreps: &[crate::node::n2c_query::types::DRepSnapshot],
) {
    // Wire format: Map<Credential, DRepState>
    //   Credential: [0|1, hash(28)]
    //   DRepState: array(4) [expiry, maybe_anchor, deposit, tag(258)[delegators]]
    enc.map(dreps.len() as u64).ok();
    for drep in dreps {
        // Key: Credential
        enc.array(2).ok();
        enc.u8(drep.credential_type).ok();
        enc.bytes(&drep.credential_hash).ok();
        // Value: DRepState array(4)
        enc.array(4).ok();
        // [0] drepExpiry (EpochNo)
        enc.u64(drep.expiry_epoch).ok();
        // [1] drepAnchor (StrictMaybe Anchor)
        if let (Some(url), Some(hash)) = (&drep.anchor_url, &drep.anchor_hash) {
            enc.array(1).ok(); // SJust
            enc.array(2).ok(); // Anchor
            enc.str(url).ok();
            enc.bytes(hash).ok();
        } else {
            enc.array(0).ok(); // SNothing
        }
        // [2] drepDeposit (Coin)
        enc.u64(drep.deposit).ok();
        // [3] drepDelegs: tag(258) Set of Credential — sorted for canonical CBOR
        let mut sorted_delegators = drep.delegator_hashes.clone();
        sorted_delegators.sort();
        enc.tag(minicbor::data::Tag::new(258)).ok();
        enc.array(sorted_delegators.len() as u64).ok();
        for dh in &sorted_delegators {
            enc.array(2).ok();
            enc.u8(0).ok(); // KeyHashObj
            enc.bytes(dh).ok();
        }
    }
}

fn encode_committee_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    committee: &crate::node::n2c_query::types::CommitteeSnapshot,
) {
    // Wire format: array(3) [map_members, maybe_threshold, epoch]
    enc.array(3).ok();
    // [0] Map<ColdCredential, CommitteeMemberState>
    enc.map(committee.members.len() as u64).ok();
    for member in &committee.members {
        // Key: Credential [type, hash(28)]
        enc.array(2).ok();
        enc.u8(member.cold_credential_type).ok();
        enc.bytes(&member.cold_credential).ok();
        // Value: CommitteeMemberState array(4)
        enc.array(4).ok();
        // [0] HotCredAuthStatus (Sum type)
        match member.hot_status {
            0 => {
                // MemberAuthorized: [0, credential]
                enc.array(2).ok();
                enc.u32(0).ok();
                if let Some(hot) = &member.hot_credential {
                    enc.array(2).ok();
                    enc.u8(member.hot_credential_type).ok(); // 0=KeyHashObj, 1=ScriptHashObj
                    enc.bytes(hot).ok();
                }
            }
            1 => {
                // MemberNotAuthorized: [1]
                enc.array(1).ok();
                enc.u32(1).ok();
            }
            _ => {
                // MemberResigned: [2, maybe_anchor]
                enc.array(2).ok();
                enc.u32(2).ok();
                enc.array(0).ok(); // SNothing anchor
            }
        }
        // [1] MemberStatus enum (0=Active, 1=Expired, 2=Unrecognized)
        enc.u8(member.member_status).ok();
        // [2] Maybe EpochNo (expiration)
        if let Some(exp) = member.expiry_epoch {
            enc.array(1).ok();
            enc.u64(exp).ok();
        } else {
            enc.array(0).ok();
        }
        // [3] NextEpochChange: NoChangeExpected [2]
        enc.array(1).ok();
        enc.u32(2).ok();
    }
    // [1] Maybe UnitInterval (threshold)
    if let Some((num, den)) = committee.threshold {
        enc.array(1).ok();
        encode_tagged_rational(enc, num, den);
    } else {
        enc.array(0).ok();
    }
    // [2] Current epoch
    enc.u64(committee.current_epoch).ok();
}

fn encode_constitution(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    url: &str,
    data_hash: &[u8],
    script_hash: Option<&[u8]>,
) {
    // Constitution = array(2) [Anchor, StrictMaybe ScriptHash]
    enc.array(2).ok();
    // Anchor = array(2) [url, hash]
    enc.array(2).ok();
    enc.str(url).ok();
    enc.bytes(data_hash).ok();
    // StrictMaybe ScriptHash (null-encoded)
    if let Some(script) = script_hash {
        enc.bytes(script).ok();
    } else {
        enc.null().ok();
    }
}

/// Encode a single `GovActionState` as CBOR `array(7)`.
pub(crate) fn encode_gov_action_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    p: &ProposalSnapshot,
) {
    // GovActionState = array(7)
    //   [0] gasId, [1] committeeVotes, [2] drepVotes,
    //   [3] spoVotes, [4] procedure, [5] proposedIn, [6] expiresAfter
    enc.array(7).ok();
    // [0] GovActionId = array(2) [tx_hash, action_index]
    enc.array(2).ok();
    enc.bytes(&p.tx_id).ok();
    enc.u32(p.action_index).ok();
    // [1] committeeVotes = Map<Credential, Vote>
    // Credential = [cred_type, hash(28)], Vote = uint (0=No, 1=Yes, 2=Abstain)
    enc.map(p.committee_votes.len() as u64).ok();
    for (hash, cred_type, vote) in &p.committee_votes {
        enc.array(2).ok();
        enc.u8(*cred_type).ok();
        enc.bytes(hash).ok();
        enc.u8(*vote).ok();
    }
    // [2] drepVotes = Map<Credential, Vote>
    enc.map(p.drep_votes.len() as u64).ok();
    for (hash, cred_type, vote) in &p.drep_votes {
        enc.array(2).ok();
        enc.u8(*cred_type).ok();
        enc.bytes(hash).ok();
        enc.u8(*vote).ok();
    }
    // [3] spoVotes = Map<KeyHash, Vote>
    // SPO uses bare KeyHash (28 bytes), not wrapped Credential
    enc.map(p.spo_votes.len() as u64).ok();
    for (pool_hash, vote) in &p.spo_votes {
        enc.bytes(pool_hash).ok();
        enc.u8(*vote).ok();
    }
    // [4] ProposalProcedure = array(4) [deposit, return_addr, gov_action, anchor]
    enc.array(4).ok();
    enc.u64(p.deposit).ok();
    enc.bytes(&p.return_addr).ok();
    // gov_action = sum type tagged by action type
    encode_gov_action(enc, &p.gov_action);
    // anchor = array(2) [url, hash]
    enc.array(2).ok();
    enc.str(&p.anchor_url).ok();
    enc.bytes(&p.anchor_hash).ok();
    // [5] proposedIn (EpochNo)
    enc.u64(p.proposed_epoch).ok();
    // [6] expiresAfter (EpochNo)
    enc.u64(p.expires_epoch).ok();
}

/// Encode a `GovAction` as a CBOR sum type tag.
///
/// We encode a simplified version since we only have the action type string.
/// Encode a `GovAction` exactly as `EncCBOR (GovAction era)` does
/// (`Conway/Governance/Procedures.hs:815-947`).
///
/// `Sum Ctor n !> f1 !> .. !> fN` is
/// `encodeListLen (N+1) <> encodeWord8 n <> f1 <> .. <> fN`, so every variant is
/// an array whose length is its field count plus one for the tag:
///
///   [0, gid|null, ppu, policy|null]              ParameterChange
///   [1, gid|null, [major, minor]]                HardForkInitiation
///   [2, {acct => coin}, policy|null]             TreasuryWithdrawals
///   [3, gid|null]                                NoConfidence
///   [4, gid|null, removeSet, {cred => epoch}, q] UpdateCommittee
///   [5, gid|null, [anchor, script|null]]         NewConstitution
///   [6]                                          InfoAction
///
/// `gid` is `encodeNullStrictMaybe`: null when there is no previous action.
///
/// This previously took only the action-type *string* and emitted hardcoded
/// placeholders — nulls, empty maps, a zero ProtVer, an empty anchor and a
/// literal 2/3 threshold — so every payload came back structurally valid but
/// substantively empty (#906). `ProposalSnapshot` has carried the real
/// `GovAction` all along.
fn encode_gov_action(enc: &mut minicbor::Encoder<&mut Vec<u8>>, action: &GovAction) {
    match action {
        GovAction::ParameterChange {
            prev_action_id,
            protocol_param_update,
            policy_hash,
        } => {
            enc.array(4).ok();
            enc.u32(0).ok();
            encode_opt_gov_action_id(enc, prev_action_id.as_ref());
            // Reuse the canonical tx-side encoder so the ParameterChange body is
            // byte-identical to the one the ledger accepted on chain.
            let ppu =
                dugite_serialization::encode::encode_protocol_param_update(protocol_param_update);
            enc.writer_mut().extend_from_slice(&ppu);
            encode_opt_script_hash(enc, policy_hash.as_ref());
        }
        GovAction::HardForkInitiation {
            prev_action_id,
            protocol_version,
        } => {
            enc.array(3).ok();
            enc.u32(1).ok();
            encode_opt_gov_action_id(enc, prev_action_id.as_ref());
            // ProtVer is a CBORGroup: one slot holding array(2)[major, minor].
            enc.array(2).ok();
            enc.u64(protocol_version.0).ok();
            enc.u64(protocol_version.1).ok();
        }
        GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash,
        } => {
            enc.array(3).ok();
            enc.u32(2).ok();
            // Map AccountAddress -> Coin. The key is the raw 29-byte reward
            // account (header byte + 28-byte credential) as one bytestring.
            enc.map(withdrawals.len() as u64).ok();
            for (acct, coin) in withdrawals {
                enc.bytes(acct).ok();
                enc.u64(coin.0).ok();
            }
            encode_opt_script_hash(enc, policy_hash.as_ref());
        }
        GovAction::NoConfidence { prev_action_id } => {
            enc.array(2).ok();
            enc.u32(3).ok();
            encode_opt_gov_action_id(enc, prev_action_id.as_ref());
        }
        GovAction::UpdateCommittee {
            prev_action_id,
            members_to_remove,
            members_to_add,
            threshold,
        } => {
            enc.array(5).ok();
            enc.u32(4).ok();
            encode_opt_gov_action_id(enc, prev_action_id.as_ref());
            // Set (Credential ColdCommitteeRole) — tag 258 wrapped array.
            enc.tag(minicbor::data::Tag::new(258)).ok();
            enc.array(members_to_remove.len() as u64).ok();
            for cred in members_to_remove {
                encode_credential(enc, cred);
            }
            // Map (Credential ColdCommitteeRole) EpochNo
            enc.map(members_to_add.len() as u64).ok();
            for (cred, epoch) in members_to_add {
                encode_credential(enc, cred);
                enc.u64(*epoch).ok();
            }
            encode_tagged_rational(enc, threshold.numerator, threshold.denominator);
        }
        GovAction::NewConstitution {
            prev_action_id,
            constitution,
        } => {
            enc.array(3).ok();
            enc.u32(5).ok();
            encode_opt_gov_action_id(enc, prev_action_id.as_ref());
            // Constitution = array(2)[anchor, guardrails script hash | null]
            enc.array(2).ok();
            enc.array(2).ok();
            enc.str(&constitution.anchor.url).ok();
            enc.bytes(constitution.anchor.data_hash.as_ref()).ok();
            encode_opt_script_hash(enc, constitution.script_hash.as_ref());
        }
        GovAction::InfoAction => {
            enc.array(1).ok();
            enc.u32(6).ok();
        }
    }
}

/// `encodeNullStrictMaybe encCBOR` over a `GovActionId`: null, or array(2).
fn encode_opt_gov_action_id(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    id: Option<&dugite_primitives::transaction::GovActionId>,
) {
    match id {
        None => {
            enc.null().ok();
        }
        Some(id) => {
            enc.array(2).ok();
            enc.bytes(id.transaction_id.as_ref()).ok();
            enc.u32(id.action_index).ok();
        }
    }
}

/// `encodeNullStrictMaybe encCBOR` over a script hash: null, or bytes(28).
fn encode_opt_script_hash(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    hash: Option<&dugite_primitives::hash::Hash28>,
) {
    match hash {
        None => {
            enc.null().ok();
        }
        Some(h) => {
            enc.bytes(h.as_ref()).ok();
        }
    }
}

/// Credential = array(2)[0=key|1=script, hash(28)].
fn encode_credential(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    cred: &dugite_primitives::credentials::Credential,
) {
    use dugite_primitives::credentials::Credential;
    enc.array(2).ok();
    match cred {
        Credential::VerificationKey(h) => {
            enc.u8(0).ok();
            enc.bytes(h.as_ref()).ok();
        }
        Credential::Script(h) => {
            enc.u8(1).ok();
            enc.bytes(h.as_ref()).ok();
        }
    }
}

fn encode_ratify_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    gov: &crate::node::n2c_query::types::GovStateSnapshot,
    enacted: &[(ProposalSnapshot, GovActionId)],
    expired: &[GovActionId],
    delayed: bool,
) {
    // Haskell RatifyState = array(4):
    //   [0] EnactState(array(7))
    //   [1] rsEnacted: Seq GovActionState (plain array)
    //   [2] rsExpired: Set GovActionId (tag(258) + array)
    //   [3] rsDelayed: Bool
    enc.array(4).ok();

    // [0] EnactState = array(7) — reuses the same encoding as the embedded
    // version in encode_gov_state. See lines 991-1059 for the canonical
    // EnactState field order.
    enc.array(7).ok();
    // ensCommittee: StrictMaybe Committee
    if gov.committee.members.is_empty() && gov.committee.threshold.is_none() {
        enc.array(0).ok(); // SNothing
    } else {
        enc.array(1).ok(); // SJust
        enc.array(2).ok();
        enc.map(gov.committee.members.len() as u64).ok();
        for m in &gov.committee.members {
            enc.array(2).ok();
            enc.u8(m.cold_credential_type).ok();
            enc.bytes(&m.cold_credential).ok();
            enc.u64(m.expiry_epoch.unwrap_or(0)).ok();
        }
        if let Some((num, den)) = gov.committee.threshold {
            encode_tagged_rational(enc, num, den);
        } else {
            encode_tagged_rational(enc, 2, 3);
        }
    }
    // ensConstitution: array(2) [Anchor, StrictMaybe ScriptHash]
    enc.array(2).ok();
    enc.array(2).ok();
    enc.str(&gov.constitution_url).ok();
    enc.bytes(&gov.constitution_hash).ok();
    if let Some(ref script) = gov.constitution_script {
        enc.bytes(script).ok();
    } else {
        enc.null().ok();
    }
    // ensCurPParams
    encode_protocol_params_cbor(enc, &gov.cur_pparams);
    // ensPrevPParams
    encode_protocol_params_cbor(enc, &gov.prev_pparams);
    // ensTreasury
    enc.u64(gov.treasury).ok();
    // ensWithdrawals: empty map
    enc.map(0).ok();
    // ensPrevGovActionIds: GovRelation StrictMaybe = array(4)
    enc.array(4).ok();
    let roots = [
        &gov.enacted_pparam_update,
        &gov.enacted_hard_fork,
        &gov.enacted_committee,
        &gov.enacted_constitution,
    ];
    for root in &roots {
        if let Some((tx_id, action_index)) = root {
            enc.array(1).ok(); // SJust
            enc.array(2).ok();
            enc.bytes(tx_id).ok();
            enc.u32(*action_index).ok();
        } else {
            enc.array(0).ok(); // SNothing
        }
    }

    // [1] rsEnacted: Seq of GovActionState (plain array, no tag 258)
    enc.array(enacted.len() as u64).ok();
    for (proposal, action_id) in enacted {
        enc.array(2).ok();
        encode_gov_action_state(enc, proposal);
        enc.array(2).ok();
        enc.bytes(&action_id.tx_id).ok();
        enc.u32(action_id.action_index).ok();
    }
    // [2] rsExpired: Set of GovActionId (tag(258) + array per Haskell Set encoding)
    enc.tag(minicbor::data::Tag::new(258)).ok();
    enc.array(expired.len() as u64).ok();
    for action_id in expired {
        enc.array(2).ok();
        enc.bytes(&action_id.tx_id).ok();
        enc.u32(action_id.action_index).ok();
    }
    // [3] rsDelayed
    enc.bool(delayed).ok();
}

// ─── Relay encoding ───────────────────────────────────────────────────────────

/// Encode a `LedgerRelayAccessPoint` for `LedgerPeerSnapshot`.
///
/// Haskell wire format:
///   DNS domain:   `array(3) [0, port_integer, domain_bytestring]`
///   IPv4 address: `array(3) [1, port_integer, array(4)[o1, o2, o3, o4]]`
///   IPv6 address: `array(3) [2, port_integer, array(4)[w1, w2, w3, w4]]`
fn encode_ledger_relay(enc: &mut minicbor::Encoder<&mut Vec<u8>>, relay: &RelaySnapshot) {
    match relay {
        RelaySnapshot::SingleHostAddr { port, ipv4, ipv6 } => {
            let p = port.unwrap_or(3001) as i64;
            if let Some(ip4) = ipv4 {
                // IPv4: [1, port, [o1, o2, o3, o4]]
                enc.array(3).ok();
                enc.u32(1).ok();
                enc.i64(p).ok();
                enc.array(4).ok();
                for octet in ip4 {
                    enc.i64(*octet as i64).ok();
                }
            } else if let Some(ip6) = ipv6 {
                // IPv6: [2, port, [w1, w2, w3, w4]] as 4 x 32-bit words
                enc.array(3).ok();
                enc.u32(2).ok();
                enc.i64(p).ok();
                enc.array(4).ok();
                for chunk in ip6.chunks(4) {
                    let w = u32::from_be_bytes([
                        chunk.first().copied().unwrap_or(0),
                        chunk.get(1).copied().unwrap_or(0),
                        chunk.get(2).copied().unwrap_or(0),
                        chunk.get(3).copied().unwrap_or(0),
                    ]);
                    enc.i64(w as i64).ok();
                }
            } else {
                // No IP — encode as IPv4 0.0.0.0
                enc.array(3).ok();
                enc.u32(1).ok();
                enc.i64(p).ok();
                enc.array(4).ok();
                for _ in 0..4 {
                    enc.i64(0).ok();
                }
            }
        }
        RelaySnapshot::SingleHostName { port, dns_name } => {
            // DNS: [0, port, domain_bytes]
            enc.array(3).ok();
            enc.u32(0).ok();
            enc.i64(port.unwrap_or(3001) as i64).ok();
            enc.bytes(dns_name.as_bytes()).ok();
        }
        RelaySnapshot::MultiHostName { dns_name } => {
            // DNS: [0, port=3001, domain_bytes]
            enc.array(3).ok();
            enc.u32(0).ok();
            enc.i64(3001).ok();
            enc.bytes(dns_name.as_bytes()).ok();
        }
    }
}

/// Encode a relay in the standard PoolParams relay encoding.
///
/// This is distinct from `encode_ledger_relay` which is used for
/// `LedgerPeerSnapshot` and uses a different byte layout for IP addresses.
fn encode_relay_cbor(enc: &mut minicbor::Encoder<&mut Vec<u8>>, relay: &RelaySnapshot) {
    match relay {
        RelaySnapshot::SingleHostAddr { port, ipv4, ipv6 } => {
            enc.array(4).ok();
            enc.u32(0).ok();
            match port {
                Some(p) => {
                    enc.u16(*p).ok();
                }
                None => {
                    enc.null().ok();
                }
            }
            match ipv4 {
                Some(ip) => {
                    enc.bytes(ip).ok();
                }
                None => {
                    enc.null().ok();
                }
            }
            match ipv6 {
                Some(ip) => {
                    enc.bytes(ip).ok();
                }
                None => {
                    enc.null().ok();
                }
            }
        }
        RelaySnapshot::SingleHostName { port, dns_name } => {
            enc.array(3).ok();
            enc.u32(1).ok();
            match port {
                Some(p) => {
                    enc.u16(*p).ok();
                }
                None => {
                    enc.null().ok();
                }
            }
            enc.str(dns_name).ok();
        }
        RelaySnapshot::MultiHostName { dns_name } => {
            enc.array(2).ok();
            enc.u32(2).ok();
            enc.str(dns_name).ok();
        }
    }
}

// ─── SnapShot encoding ────────────────────────────────────────────────────────

/// Encode a single Cardano `SnapShot` as `array(3)` per Haskell wire format.
///
/// SnapShot = array(3):
///   [0] stake_map       — `Map<Credential(29B), Lovelace>`
///   [1] delegation_map  — `Map<Credential(29B), pool_id(28B)>`
///   [2] pool_params_map — `Map<pool_id(28B), PoolParams(array(9))>`
///
/// Credential (29 bytes) = 1-byte type prefix (0x00=KeyHash, 0x01=ScriptHash)
/// followed by 28 bytes of the hash.
///
/// cncli reads these maps to compute the leader schedule for a pool operator.
pub(crate) fn encode_snap_shot(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    snap: &SnapshotStakeData,
) {
    enc.array(3).ok();

    // [0] stake_map: Map<Credential(29B), Lovelace>
    enc.map(snap.stake_entries.len() as u64).ok();
    for (cred_type, cred_hash, lovelace) in &snap.stake_entries {
        // Credential key: 1-byte type prefix || 28-byte hash
        let mut key = Vec::with_capacity(29);
        key.push(*cred_type);
        key.extend_from_slice(cred_hash);
        enc.bytes(&key).ok();
        enc.u64(*lovelace).ok();
    }

    // [1] delegation_map: Map<Credential(29B), pool_id(28B)>
    enc.map(snap.delegation_entries.len() as u64).ok();
    for (cred_type, cred_hash, pool_id) in &snap.delegation_entries {
        let mut key = Vec::with_capacity(29);
        key.push(*cred_type);
        key.extend_from_slice(cred_hash);
        enc.bytes(&key).ok();
        enc.bytes(pool_id).ok();
    }

    // [2] pool_params_map: Map<pool_id(28B), PoolParams>
    encode_pool_params_map(enc, &snap.pool_params);
}

// ─── Debug query encoding ─────────────────────────────────────────────────────

/// Encode `DebugEpochState` (tag 8) as the Haskell `EpochState` CBOR structure.
///
/// Haskell `EpochState` is a 4-element positional record:
/// ```text
/// array(4) [
///   ChainAccountState,   -- array(2) [treasury, reserves]
///   LedgerState,         -- simplified placeholder (CBOR-skippable)
///   SnapShots,           -- array(4) [mark, set, go, fee]
///   NonMyopic,           -- array(2) [likelihoods_map, reward_pot_coin]
/// ]
/// ```
///
/// References:
///   `cardano-ledger / eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs`
///   `encCBOR (EpochState acnt ls ss nm) = ... encodeListLen 4 <> ...`
fn encode_debug_epoch_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    treasury: u64,
    reserves: u64,
    snap_mark: &SnapshotStakeData,
    snap_set: &SnapshotStakeData,
    snap_go: &SnapshotStakeData,
    snap_fee: u64,
) {
    // EpochState = array(4) [AccountState, LedgerState, SnapShots, NonMyopic]
    enc.array(4).ok();

    // [0] ChainAccountState = array(2) [treasury, reserves]
    enc.array(2).ok();
    enc.u64(treasury).ok();
    enc.u64(reserves).ok();

    // [1] LedgerState — simplified CBOR-skippable placeholder.
    //
    // In Conway, `LedgerState = array(2) [UTxOState, CertState]`.
    // We emit a minimal but structurally valid representation so that a
    // strict CBOR parser can decode past it to reach SnapShots at [2].
    //
    // UTxOState = array(5) [utxo_map, deposited, fees, gov_state, donation]
    // CertState = array(3) [VState, PState, DState]
    //
    // Haskell references:
    //   `Cardano.Ledger.Shelley.LedgerState.UTxOState` (encodeListLen 5)
    //   `Cardano.Ledger.Shelley.LedgerState.CertState` (encodeListLen 3)
    enc.array(2).ok();
    // UTxOState: array(5) with all-zero / empty contents
    enc.array(5).ok();
    enc.map(0).ok(); // empty UTxO map
    enc.u64(0).ok(); // deposited lovelace = 0
    enc.u64(0).ok(); // fees = 0
                     // GovState placeholder: ConwayGovState = array(7) — emit array(0) as a
                     // skippable marker; parsers that only read LedgerState[1] (CertState) skip
                     // this via decodeSkip before reaching CertState.
    enc.array(0).ok();
    enc.u64(0).ok(); // donation = 0
                     // CertState: array(3) [VState, PState, DState] — all empty
    enc.array(3).ok();
    enc.array(0).ok(); // VState placeholder
    enc.array(0).ok(); // PState placeholder
    enc.array(0).ok(); // DState placeholder

    // [2] SnapShots = array(4) [mark, set, go, fee]
    enc.array(4).ok();
    encode_snap_shot(enc, snap_mark);
    encode_snap_shot(enc, snap_set);
    encode_snap_shot(enc, snap_go);
    enc.u64(snap_fee).ok();

    // [3] NonMyopic = array(2) [likelihoods_map, reward_pot_coin]
    //
    // `NonMyopic` stores per-pool likelihood histories used for non-myopic
    // pool ranking.  We emit an empty likelihoods map and a zero reward pot.
    // Reference: `Cardano.Ledger.Shelley.PoolRank` (encodeListLen 2)
    enc.array(2).ok();
    enc.map(0).ok(); // empty likelihoods map
    enc.u64(0).ok(); // reward pot coin = 0
}

#[allow(clippy::too_many_arguments)]
fn encode_debug_new_epoch_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    epoch: u64,
    blocks_made_prev: &[(Vec<u8>, u64)],
    blocks_made_cur: &[(Vec<u8>, u64)],
    treasury: u64,
    reserves: u64,
    snap_mark: &SnapshotStakeData,
    snap_set: &SnapshotStakeData,
    snap_go: &SnapshotStakeData,
    snap_fee: u64,
    total_active_stake: u64,
    pool_distr: &[crate::node::n2c_query::types::StakePoolSnapshot],
) {
    // Full Haskell-compatible NewEpochState (array(7)):
    //
    //   [0] EpochNo
    //   [1] BlocksMade (prev epoch) — Map<pool_id_28B, u64>
    //   [2] BlocksMade (cur  epoch) — Map<pool_id_28B, u64>
    //   [3] EpochState — array(4) [AccountState, LedgerState, SnapShots, NonMyopic]
    //   [4] StrictMaybe RewardUpdate — array(0) for Nothing
    //   [5] PoolDistr — Map<pool_id_28B, IndividualPoolStake>
    //   [6] Extra — array(0) (Conway-era field, empty)
    //
    // cncli reads [3][2] (SnapShots) to extract the per-credential
    // stake distribution for leader-schedule computation.
    enc.array(7).ok();

    // [0] EpochNo
    enc.u64(epoch).ok();

    // [1] BlocksMade previous epoch: Map<pool_id(28B), u64>
    enc.map(blocks_made_prev.len() as u64).ok();
    for (pool_id, count) in blocks_made_prev {
        enc.bytes(pool_id).ok();
        enc.u64(*count).ok();
    }

    // [2] BlocksMade current epoch: Map<pool_id(28B), u64>
    enc.map(blocks_made_cur.len() as u64).ok();
    for (pool_id, count) in blocks_made_cur {
        enc.bytes(pool_id).ok();
        enc.u64(*count).ok();
    }

    // [3] EpochState = array(4)
    enc.array(4).ok();

    // [3][0] AccountState = array(2) [treasury, reserves]
    enc.array(2).ok();
    enc.u64(treasury).ok();
    enc.u64(reserves).ok();

    // [3][1] LedgerState — simplified empty placeholder.
    // cncli does not parse this field; it only inspects [3][2].
    // We encode a minimal valid array(2) [UTxOState, CertState] with
    // empty contents so that a CBOR parser can skip past it.
    enc.array(2).ok();
    // UTxOState = array(5): utxo_map, deposited, fees, gov_state, donation
    enc.array(5).ok();
    enc.map(0).ok(); // empty UTxO map
    enc.u64(0).ok(); // deposited = 0
    enc.u64(0).ok(); // fees = 0
                     // GovState placeholder (array(0))
    enc.array(0).ok();
    enc.u64(0).ok(); // donation = 0
                     // CertState = array(3): VState, PState, DState (all empty)
    enc.array(3).ok();
    enc.array(0).ok(); // VState placeholder
    enc.array(0).ok(); // PState placeholder
    enc.array(0).ok(); // DState placeholder

    // [3][2] SnapShots = array(4) [mark, set, go, fee]
    enc.array(4).ok();
    encode_snap_shot(enc, snap_mark);
    encode_snap_shot(enc, snap_set);
    encode_snap_shot(enc, snap_go);
    enc.u64(snap_fee).ok();

    // [3][3] NonMyopic = array(2) [likelihoods_map, reward_pot_coin]
    // cncli does not inspect this field, but we encode it correctly for
    // strict parsers.  Reference: Cardano.Ledger.Shelley.PoolRank (encodeListLen 2)
    enc.array(2).ok();
    enc.map(0).ok(); // empty likelihoods map
    enc.u64(0).ok(); // reward pot coin = 0

    // [4] StrictMaybe RewardUpdate = Nothing = array(0)
    enc.array(0).ok();

    // [5] PoolDistr: Map<pool_id(28B), IndividualPoolStake>
    // IndividualPoolStake = array(2) [tag(30)[num,den], vrf_hash(32B)]
    let total = total_active_stake.max(1);
    enc.map(pool_distr.len() as u64).ok();
    for pool in pool_distr {
        enc.bytes(&pool.pool_id).ok();
        enc.array(2).ok();
        encode_tagged_rational(enc, pool.stake, total);
        enc.bytes(&pool.vrf_keyhash).ok();
    }

    // [6] Extra = array(0)
    enc.array(0).ok();
}

/// Encode `DebugChainDepState` (tag 13) as the Haskell `PraosState` CBOR structure.
///
/// Haskell uses `encodeVersion 0` (from `Ouroboros.Consensus.Util.Versioned`),
/// which wraps any payload as `array(2) [version, payload]`.  The PraosState
/// payload is `array(8)` of the eight fields listed below.
///
/// Field layout (`ouroboros-consensus-protocol-3.0.1.0`, shipped with
/// cardano-node 11.0.1 — `Ouroboros/Consensus/Protocol/Praos.hs`):
///   [0] praosStateLastSlot              — WithOrigin SlotNo
///   [1] praosStateOCertCounters         — Map<KeyHash BlockIssuer, Word64>
///   [2] praosStateEvolvingNonce         — Nonce
///   [3] praosStateCandidateNonce        — Nonce
///   [4] praosStateEpochNonce            — Nonce
///   [5] praosStatePreviousEpochNonce    — Nonce
///   [6] praosStateLabNonce              — Nonce
///   [7] praosStateLastEpochBlockNonce   — Nonce
///
/// This emitted `array(7)` (no `praosStatePreviousEpochNonce`) until #902, on
/// the assumption that the field was unreleased.  cardano-node 11.0.x decodes
/// with `enforceSize "PraosState" 8` and registers only one version, so there
/// is no 7-field fallback and every 11.x client rejected the short form with
/// `Size mismatch when decoding PraosState. Expected 8, but found 7.`
///
/// All nonce values use the Haskell `Nonce` encoding:
///   - `NeutralNonce` → `array(1) [0]`
///   - `Nonce hash`   → `array(2) [1, bytes32]`
///
/// Empty or all-zero slices are treated as `NeutralNonce`.
///
/// The `WithOrigin SlotNo` encoding:
///   - `Origin` → `array(1) [0]`
///   - `At slot` → `array(2) [1, slot_u64]`
///
/// References:
///   `ouroboros-consensus-protocol 0.13.0.0 / Ouroboros/Consensus/Protocol/Praos.hs`
///   `ouroboros-consensus / Ouroboros/Consensus/Util/Versioned.hs`
///   `cardano-ledger / libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs`
#[allow(clippy::too_many_arguments)]
fn encode_debug_chain_dep_state(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    last_slot: u64,
    last_slot_is_origin: bool,
    ocert_counters: &[(Vec<u8>, u64)],
    evolving_nonce: &[u8],
    candidate_nonce: &[u8],
    epoch_nonce: &[u8],
    previous_epoch_nonce: &[u8],
    lab_nonce: &[u8],
    last_epoch_block_nonce: &[u8],
) {
    // encodeVersion 0: array(2) [0, payload]
    enc.array(2).ok();
    enc.u8(0).ok(); // version number

    // PraosState: array(8) [lastSlot, ocertCounters, evolvingNonce,
    //   candidateNonce, epochNonce, previousEpochNonce, labNonce,
    //   lastEpochBlockNonce]
    enc.array(8).ok();

    // [0] praosStateLastSlot: WithOrigin SlotNo
    // WithOrigin<T> via generic Serialise: Origin=[0], At slot=[1, slot]
    if last_slot_is_origin {
        enc.array(1).ok();
        enc.u8(0).ok();
    } else {
        enc.array(2).ok();
        enc.u8(1).ok();
        enc.u64(last_slot).ok();
    }

    // [1] praosStateOCertCounters: Map<KeyHash BlockIssuer, Word64>
    // KeyHash is a 28-byte hash; we use the raw bytes as map key.
    enc.map(ocert_counters.len() as u64).ok();
    for (pool_hash, counter) in ocert_counters {
        enc.bytes(pool_hash).ok();
        enc.u64(*counter).ok();
    }

    // Helper: encode a Cardano `Nonce` value.
    // NeutralNonce: empty or all-zero bytes → array(1)[0]
    // Nonce(hash):  any non-zero 32-byte slice → array(2)[1, bytes32]
    let encode_nonce = |enc: &mut minicbor::Encoder<&mut Vec<u8>>, nonce: &[u8]| {
        let is_neutral = nonce.is_empty() || nonce.iter().all(|&b| b == 0);
        if is_neutral {
            enc.array(1).ok();
            enc.u8(0).ok();
        } else {
            enc.array(2).ok();
            enc.u8(1).ok();
            enc.bytes(nonce).ok();
        }
    };

    // [2] praosStateEvolvingNonce
    encode_nonce(enc, evolving_nonce);
    // [3] praosStateCandidateNonce
    encode_nonce(enc, candidate_nonce);
    // [4] praosStateEpochNonce
    encode_nonce(enc, epoch_nonce);
    // [5] praosStatePreviousEpochNonce
    encode_nonce(enc, previous_epoch_nonce);
    // [6] praosStateLabNonce
    encode_nonce(enc, lab_nonce);
    // [7] praosStateLastEpochBlockNonce
    encode_nonce(enc, last_epoch_block_nonce);
}

fn encode_reward_info_pools(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pools: &[crate::node::n2c_query::types::PoolRewardInfo],
) {
    // Map<pool_hash(28), PoolRewardInfo>
    // PoolRewardInfo: array(7) [stake, owner_stake, pool_reward, leader_reward,
    //                           member_reward, margin_rational, cost]
    enc.map(pools.len() as u64).ok();
    for pool in pools {
        enc.bytes(&pool.pool_id).ok();
        enc.array(7).ok();
        enc.u64(pool.stake).ok();
        enc.u64(pool.owner_stake).ok();
        enc.u64(pool.pool_reward).ok();
        enc.u64(pool.leader_reward).ok();
        enc.u64(pool.member_reward).ok();
        enc.tag(minicbor::data::Tag::new(30)).ok();
        enc.array(2).ok();
        enc.u64(pool.margin.0).ok();
        enc.u64(pool.margin.1).ok();
        enc.u64(pool.cost).ok();
    }
}

// ─── Protocol / era history encoding ─────────────────────────────────────────

/// Parse an ISO-8601 UTC timestamp to `(year, dayOfYear, picosecondsOfDay)`.
///
/// Input format: `"2022-04-01T00:00:00Z"` or similar.
pub(crate) fn parse_utctime(s: &str) -> (u64, u64, u64) {
    // Try to parse "YYYY-MM-DDThh:mm:ssZ"
    let s = s.trim_end_matches('Z');
    let parts: Vec<&str> = s.split('T').collect();
    if parts.len() != 2 {
        return (2017, 266, 0); // fallback: mainnet system start
    }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let time_parts: Vec<u64> = parts[1].split(':').filter_map(|p| p.parse().ok()).collect();

    if date_parts.len() < 3 || time_parts.len() < 3 {
        return (2017, 266, 0);
    }

    let (year, month, day) = (date_parts[0], date_parts[1], date_parts[2]);

    // Calculate day of year
    let days_in_months: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let mut day_of_year = day;
    for (i, &days) in days_in_months.iter().enumerate().take((month - 1) as usize) {
        day_of_year += days;
        if i == 1 && is_leap {
            day_of_year += 1;
        }
    }

    // Picoseconds of day
    let picos = (time_parts[0] * 3600 + time_parts[1] * 60 + time_parts[2]) * 1_000_000_000_000;

    (year, day_of_year, picos)
}

/// Encode `SystemStart` as `UTCTime`: `[year, day_of_year, pico_of_day]`
fn encode_system_start(enc: &mut minicbor::Encoder<&mut Vec<u8>>, time_str: &str) {
    let (year, day_of_year, picos) = parse_utctime(time_str);
    enc.array(3).ok();
    enc.u64(year).ok();
    enc.u64(day_of_year).ok();
    enc.u64(picos).ok();
}

/// Encode legacy Shelley PParams as `array(18)` (N2C V16-V20 legacy format).
/// Encode Shelley PParams in legacy format (V16-V20).
///
/// `array(18)` with ProtocolVersion as two flat integers at [14] and [15].
pub(crate) fn encode_shelley_pparams(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pp: &ShelleyPParamsSnapshot,
) {
    encode_shelley_pparams_common(enc, pp, false);
}

/// Encode Shelley PParams in new format (V21+).
///
/// `array(17)` with ProtocolVersion as `array(2) [major, minor]` at [14].
pub(crate) fn encode_shelley_pparams_v21(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pp: &ShelleyPParamsSnapshot,
) {
    encode_shelley_pparams_common(enc, pp, true);
}

/// Shared Shelley PParams encoding. When `v21_protver` is true, uses
/// `array(17)` with bundled ProtocolVersion; otherwise `array(18)` with
/// flat major/minor fields per the legacy encoding.
fn encode_shelley_pparams_common(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    pp: &ShelleyPParamsSnapshot,
    v21_protver: bool,
) {
    enc.array(if v21_protver { 17 } else { 18 }).ok();
    enc.u64(pp.min_fee_a).ok(); // [0] txFeePerByte
    enc.u64(pp.min_fee_b).ok(); // [1] txFeeFixed
    enc.u32(pp.max_block_body_size).ok(); // [2] maxBBSize
    enc.u32(pp.max_tx_size).ok(); // [3] maxTxSize
    enc.u16(pp.max_block_header_size).ok(); // [4] maxBHSize
    enc.u64(pp.key_deposit).ok(); // [5] keyDeposit
    enc.u64(pp.pool_deposit).ok(); // [6] poolDeposit
    enc.u32(pp.e_max).ok(); // [7] eMax
    enc.u16(pp.n_opt).ok(); // [8] nOpt
    encode_tagged_rational(enc, pp.a0_num, pp.a0_den); // [9] a0
    encode_tagged_rational(enc, pp.rho_num, pp.rho_den); // [10] rho
    encode_tagged_rational(enc, pp.tau_num, pp.tau_den); // [11] tau
    encode_tagged_rational(enc, pp.d_num, pp.d_den); // [12] d (decentralization)
                                                     // [13] extraEntropy: NeutralNonce = [0]
    enc.array(1).ok();
    enc.u32(0).ok();
    if v21_protver {
        // V21+: ProtocolVersion = array(2) [major, minor] at [14]
        enc.array(2).ok();
        enc.u64(pp.protocol_version_major).ok();
        enc.u64(pp.protocol_version_minor).ok();
    } else {
        // V16-V20: ProtocolVersion as two flat integers at [14] and [15]
        enc.u64(pp.protocol_version_major).ok();
        enc.u64(pp.protocol_version_minor).ok();
    }
    // [15/16] minUTxOValue
    enc.u64(pp.min_utxo_value).ok();
    // [16/17] minPoolCost
    enc.u64(pp.min_pool_cost).ok();
}

/// Encode a picosecond timestamp as a CBOR integer.
///
/// Values that fit in u64 are encoded as a normal CBOR unsigned integer.
/// Larger values (e.g., mainnet Byron end time ~9e19) are encoded as a CBOR
/// positive bignum (tag 2 + big-endian byte string), matching Haskell's
/// Serialise instance for `Fixed E12` (Pico).
fn encode_pico(enc: &mut minicbor::Encoder<&mut Vec<u8>>, value: u128) {
    if value <= u64::MAX as u128 {
        enc.u64(value as u64).ok();
    } else {
        // CBOR tag 2 = positive bignum, followed by big-endian bytes.
        enc.tag(minicbor::data::Tag::new(2)).ok();
        let bytes = value.to_be_bytes();
        // Strip leading zero bytes.
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        enc.bytes(&bytes[start..]).ok();
    }
}

fn encode_era_history(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    summaries: &[crate::node::n2c_query::types::EraSummary],
) {
    // Wire format matching Haskell ouroboros-consensus Serialise instances:
    // Indefinite-length array of EraSummary entries.
    // Each EraSummary = array(3) [start_bound, era_end, era_params]
    // Bound = array(3) [relative_time_pico, slot_no, epoch_no]
    // EraEnd: EraEnd(bound) = encode bound directly; EraUnbounded = null (0xf6)
    // EraParams = array(4) [epoch_size, slot_length_ms, safe_zone, genesis_window]
    // SafeZone: StandardSafeZone(n) = array(3) [0, n, array(1)[0]]
    //           UnsafeIndefiniteSafeZone = array(1) [1]
    //
    // Peras (issue #459): Haskell ouroboros-consensus 1.0.0.0 extends `Bound`
    // and the flat `EraParams` projection with an optional `peras_round` /
    // `peras_round_length` field, encoded by varying the array length
    // (3→4 and 4→5 respectively). dugite stays on the pre-Peras forms below
    // until Peras activates on a network; see
    // `dugite_consensus::peras_wire::{encode_bound, encode_era_params}` for
    // the variable-length helpers and `decode_bound` / `decode_era_params`
    // for the symmetric decoders.
    enc.begin_array().ok(); // indefinite-length array (0x9f)
    for (i, summary) in summaries.iter().enumerate() {
        enc.array(3).ok();
        // Start bound: [time_pico, slot, epoch]
        enc.array(3).ok();
        encode_pico(enc, summary.start_time_pico);
        enc.u64(summary.start_slot).ok();
        enc.u64(summary.start_epoch).ok();
        // Era end: EraEnd(bound) = Bound directly, EraUnbounded = null
        if let Some(end) = &summary.end {
            enc.array(3).ok();
            encode_pico(enc, end.time_pico);
            enc.u64(end.slot).ok();
            enc.u64(end.epoch).ok();
        } else {
            enc.null().ok();
        }
        // Era params: [epoch_size, slot_length_ms, safe_zone, genesis_window]
        enc.array(4).ok();
        enc.u64(summary.epoch_size).ok();
        enc.u64(summary.slot_length_ms).ok();
        // Safe zone encoding
        let is_last = i == summaries.len() - 1;
        if is_last && summary.end.is_none() {
            // Current/unbounded era: UnsafeIndefiniteSafeZone = array(1) [1]
            enc.array(1).ok();
            enc.u8(1).ok();
        } else {
            // Past era or bounded: StandardSafeZone(n) = array(3) [0, n, [0]]
            enc.array(3).ok();
            enc.u8(0).ok();
            enc.u64(summary.safe_zone).ok();
            enc.array(1).ok();
            enc.u8(0).ok();
        }
        enc.u64(summary.genesis_window).ok();
    }
    enc.end().ok(); // end indefinite-length array (0xff)
}

fn encode_genesis_config(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    gc: &crate::node::n2c_query::types::GenesisConfigSnapshot,
    n2c_version: u16,
) {
    // CompactGenesis: array(15) matching ShelleyGenesis CBOR wire format
    enc.array(15).ok();

    // [0] systemStart: UTCTime = array(3) [year, dayOfYear, picosecondsOfDay]
    let (year, day_of_year, picos) = parse_utctime(&gc.system_start);
    enc.array(3).ok();
    enc.u64(year).ok();
    enc.u64(day_of_year).ok();
    enc.u64(picos).ok();

    // [1] networkMagic: u32
    enc.u32(gc.network_magic).ok();

    // [2] networkId: 0=Testnet, 1=Mainnet
    enc.u8(gc.network_id).ok();

    // [3] activeSlotsCoeff: [num, den] (NO tag(30))
    enc.array(2).ok();
    enc.u64(gc.active_slots_coeff_num).ok();
    enc.u64(gc.active_slots_coeff_den).ok();

    // [4] securityParam: u64
    enc.u64(gc.security_param).ok();

    // [5] epochLength: u64
    enc.u64(gc.epoch_length).ok();

    // [6] slotsPerKESPeriod: u64
    enc.u64(gc.slots_per_kes_period).ok();

    // [7] maxKESEvolutions: u64
    enc.u64(gc.max_kes_evolutions).ok();

    // [8] slotLength: Fixed E6 integer (microseconds)
    enc.u64(gc.slot_length_micros).ok();

    // [9] updateQuorum: u64
    enc.u64(gc.update_quorum).ok();

    // [10] maxLovelaceSupply: u64
    enc.u64(gc.max_lovelace_supply).ok();

    // [11] protocolParams: version-gated encoding
    // V16-V20: array(18) with flat ProtocolVersion at [14] and [15]
    // V21+: array(17) with ProtocolVersion as array(2) [major, minor] at [14]
    if n2c_version >= 21 {
        encode_shelley_pparams_v21(enc, &gc.protocol_params);
    } else {
        encode_shelley_pparams(enc, &gc.protocol_params);
    }

    // [12] genDelegs: Map<hash28 -> array(2)[hash28, hash32]>
    enc.map(gc.gen_delegs.len() as u64).ok();
    for (genesis_hash, delegate_hash, vrf_hash) in &gc.gen_delegs {
        enc.bytes(genesis_hash).ok();
        enc.array(2).ok();
        enc.bytes(delegate_hash).ok();
        enc.bytes(vrf_hash).ok();
    }

    // [13] initialFunds: empty map (CompactGenesis)
    enc.map(0).ok();

    // [14] staking: array(2) [empty_map, empty_map] (CompactGenesis)
    enc.array(2).ok();
    enc.map(0).ok();
    enc.map(0).ok();
}

// ─── Ledger peer snapshot encoding ───────────────────────────────────────────

fn encode_ledger_peer_snapshot(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    peers: &[crate::node::n2c_query::types::LedgerPeerEntry],
) {
    // LedgerPeerSnapshotV2 (version 1): array(2) [1, array(2)[WithOrigin, pools_indef]]
    // Haskell: (WithOrigin SlotNo, [(AccPoolStake, (PoolStake, NonEmpty relay))])
    // Stakes are Rational: array(2)[numerator, denominator]
    // Only include "big ledger peers" — top pools controlling 90% of stake.

    // Sort by stake descending and filter to big peers (top 90%)
    let mut sorted: Vec<_> = peers.iter().filter(|p| p.stake > 0).collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.stake));
    let total_stake: u64 = sorted.iter().map(|p| p.stake).sum();
    let cutoff = total_stake * 9 / 10; // 90% threshold
    let mut acc_raw: u64 = 0;
    let mut big_peers = Vec::new();
    for peer in &sorted {
        big_peers.push(*peer);
        acc_raw += peer.stake;
        if acc_raw >= cutoff {
            break;
        }
    }

    enc.array(2).ok();
    enc.u32(1).ok(); // version 1
    enc.array(2).ok();
    // WithOrigin: Origin = [0]  (we don't track the snapshot slot)
    enc.array(1).ok();
    enc.u32(0).ok();
    // pools: indefinite-length array
    enc.begin_array().ok();
    let mut acc_num: u64 = 0;
    for peer in &big_peers {
        acc_num += peer.stake;
        enc.array(2).ok();
        // AccPoolStake as Rational (accumulated stake / total)
        enc.array(2).ok();
        enc.u64(acc_num).ok();
        enc.u64(total_stake.max(1)).ok();
        // (PoolStake, relays)
        enc.array(2).ok();
        // PoolStake as Rational (relative stake)
        enc.array(2).ok();
        enc.u64(peer.stake).ok();
        enc.u64(total_stake.max(1)).ok();
        // relays: indefinite-length array (NonEmpty)
        enc.begin_array().ok();
        for relay in &peer.relays {
            encode_ledger_relay(enc, relay);
        }
        enc.end().ok();
    }
    enc.end().ok(); // end pool list
}

// ─── V23 LedgerPeerSnapshot encoders (issue #456) ────────────────────────────

/// Encode a `Point RawBlockHash` for the V23 ledger-peer snapshot.
///
/// Haskell wire layout (ouroboros-network `Ouroboros.Network.Block`):
///   * `Origin`           → `array(1)[0]`
///   * `Block slot hash`  → `array(3)[1, slot, bstr(32)]`
fn encode_point_raw_block_hash(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    point: &dugite_primitives::block::Point,
) {
    use dugite_primitives::block::Point;
    match point {
        Point::Origin => {
            enc.array(1).ok();
            enc.u32(0).ok();
        }
        Point::Specific(slot, hash) => {
            enc.array(3).ok();
            enc.u32(1).ok();
            enc.u64(slot.0).ok();
            enc.bytes(hash.as_ref()).ok();
        }
    }
}

/// Filter `peers` to the "big ledger peers" set (top pools controlling 90% of
/// stake) and return them along with the total stake (denominator for the
/// rational shares).
fn select_big_peers(
    peers: &[crate::node::n2c_query::types::LedgerPeerEntry],
) -> (Vec<&crate::node::n2c_query::types::LedgerPeerEntry>, u64) {
    let mut sorted: Vec<_> = peers.iter().filter(|p| p.stake > 0).collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.stake));
    let total: u64 = sorted.iter().map(|p| p.stake).sum();
    let cutoff = total * 9 / 10;
    let mut acc: u64 = 0;
    let mut out = Vec::new();
    for peer in sorted {
        out.push(peer);
        acc += peer.stake;
        if acc >= cutoff {
            break;
        }
    }
    (out, total)
}

/// V23 BigLedgerPeers (outer discriminator `uint(2)`).
///
/// `array(2)[ uint(2), array(3)[Point, NetworkMagic, pools_indef] ]`
/// where each pool is `array(3)[AccPoolStake_rat, PoolStake_rat, relays_indef]`.
pub(crate) fn encode_ledger_peer_snapshot_v23_big(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    anchor: &dugite_primitives::block::Point,
    network_magic: u32,
    peers: &[crate::node::n2c_query::types::LedgerPeerEntry],
) {
    let (big_peers, total_stake) = select_big_peers(peers);
    let denom = total_stake.max(1);

    enc.array(2).ok();
    enc.u32(2).ok(); // V23 Big discriminator
    enc.array(3).ok();
    encode_point_raw_block_hash(enc, anchor);
    enc.u32(network_magic).ok();
    enc.begin_array().ok();
    let mut acc_num: u64 = 0;
    for peer in &big_peers {
        acc_num += peer.stake;
        enc.array(3).ok();
        // AccPoolStake (running cumulative share)
        enc.array(2).ok();
        enc.u64(acc_num).ok();
        enc.u64(denom).ok();
        // PoolStake (individual share)
        enc.array(2).ok();
        enc.u64(peer.stake).ok();
        enc.u64(denom).ok();
        // Relays (NonEmpty, indefinite list)
        enc.begin_array().ok();
        for relay in &peer.relays {
            encode_ledger_relay(enc, relay);
        }
        enc.end().ok();
    }
    enc.end().ok();
}

/// V23 AllLedgerPeers (outer discriminator `uint(3)`).
///
/// `array(2)[ uint(3), array(3)[Point, NetworkMagic, pools_indef] ]`
/// where each pool is `array(2)[PoolStake_rat, relays_indef]`.  Note no
/// `AccPoolStake` field (the All variant lists every pool, not just the
/// 90%-stake-weighted prefix).
pub(crate) fn encode_ledger_peer_snapshot_v23_all(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    anchor: &dugite_primitives::block::Point,
    network_magic: u32,
    peers: &[crate::node::n2c_query::types::LedgerPeerEntry],
) {
    let mut sorted: Vec<_> = peers.iter().filter(|p| p.stake > 0).collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.stake));
    let total_stake: u64 = sorted.iter().map(|p| p.stake).sum();
    let denom = total_stake.max(1);

    enc.array(2).ok();
    enc.u32(3).ok(); // V23 All discriminator
    enc.array(3).ok();
    encode_point_raw_block_hash(enc, anchor);
    enc.u32(network_magic).ok();
    enc.begin_array().ok();
    for peer in &sorted {
        enc.array(2).ok();
        enc.array(2).ok();
        enc.u64(peer.stake).ok();
        enc.u64(denom).ok();
        enc.begin_array().ok();
        for relay in &peer.relays {
            encode_ledger_relay(enc, relay);
        }
        enc.end().ok();
    }
    enc.end().ok();
}

// ─── Hash-size regression tests ───────────────────────────────────────────────
//
// These tests verify that all credential/pool-ID/DRep hashes are encoded as
// exactly 28 bytes in N2C query responses, matching the Cardano wire format.
//
// Background: internally the ledger stores Blake2b-224 (28-byte) hashes as
// Hash32 (zero-padded to 32 bytes) for use as uniform HashMap keys.  When
// building N2C responses these must be truncated back to 28 bytes.  Sending
// 32-byte hashes causes cardano-cli to reject with "hash bytes wrong size".
//
// See GitHub issue #97.
/// Strip the `MsgResult [4, [payload]]` + HFC success wrappers from a full
/// `encode_query_result` output, leaving the inner payload.
///
/// Shared with sibling test modules (`stake.rs` asserts on the encoded σ, which
/// is the only place the #964 denominator is observable).
#[cfg(test)]
pub(crate) fn strip_wrappers_for_test(cbor: &[u8]) -> Vec<u8> {
    let mut dec = minicbor::Decoder::new(cbor);
    dec.array().unwrap();
    dec.u32().unwrap(); // 4 = MsgResult
    dec.array().unwrap(); // HFC EitherMismatch success wrapper
    cbor[dec.position()..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::n2c_query::types::{
        CommitteeMemberSnapshot, CommitteeSnapshot, DRepDelegationGroup, DRepKey, DRepSnapshot,
        DRepStakeEntry, PoolDistrEntry, PoolParamsSnapshot, PoolStakeSnapshotEntry,
        StakeAddressSnapshot, StakeDelegDepositEntry, StakePoolSnapshot, StakeSnapshotsResult,
        VoteDelegateeEntry,
    };
    use minicbor::Decoder;

    /// GOLDEN (#977): `futurePParams` must encode as a real tagged sum.
    ///
    /// Shapes verified against real preview epoch-1259 bytes by
    /// `dugite-serialization`'s `decode_future_pparams`:
    ///
    /// ```text
    /// NoPParamsUpdate            -> array(1) [0]
    /// DefinitePParamsUpdate pp   -> array(2) [1, pp]
    /// PotentialPParamsUpdate m   -> array(2) [2, <array(0) | array(1) [pp]>]
    /// ```
    ///
    /// This was hardcoded to tag 0 — right for the LATER part of every epoch
    /// and wrong for the earlier part, which on mainnet is the first ~40%.
    #[test]
    fn golden_future_pparams_encodes_all_three_variants() {
        use crate::node::n2c_query::types::{GovStateSnapshot, ProtocolParamsSnapshot};

        fn gov_with(tag: u8, payload: bool) -> GovStateSnapshot {
            let pp = Box::new(ProtocolParamsSnapshot::default());
            GovStateSnapshot {
                proposals: Vec::new(),
                committee: CommitteeSnapshot::default(),
                constitution_url: String::new(),
                constitution_hash: vec![0u8; 32],
                constitution_script: None,
                cur_pparams: pp.clone(),
                prev_pparams: pp.clone(),
                enacted_pparam_update: None,
                enacted_hard_fork: None,
                enacted_committee: None,
                enacted_constitution: None,
                treasury: 0,
                future_pparams_tag: tag,
                future_pparams: payload.then_some(pp),
                pulser_proposals: Vec::new(),
                pulser_drep_distr: Vec::new(),
                pulser_drep_state: Vec::new(),
                pulser_pool_distr: Vec::new(),
                ratify_enacted: Vec::new(),
                ratify_expired: Vec::new(),
                ratify_delayed: false,
            }
        }

        /// Locate the futurePParams element by DECODING the GovState array up
        /// to index 5, rather than by byte-offset arithmetic that would drift
        /// silently if an earlier field changed size.
        fn future_pparams_bytes(gov: &GovStateSnapshot) -> Vec<u8> {
            let encoded = encode_query_result(&QueryResult::GovState(Box::new(gov.clone())));
            let inner = strip_wrappers_for_test(&encoded);
            let mut dec = Decoder::new(&inner);
            dec.array().unwrap();
            for _ in 0..5 {
                dec.skip().unwrap();
            }
            let start = dec.position();
            dec.skip().unwrap();
            inner[start..dec.position()].to_vec()
        }

        assert_eq!(
            future_pparams_bytes(&gov_with(0, false)),
            vec![0x81, 0x00],
            "NoPParamsUpdate"
        );
        assert_eq!(
            future_pparams_bytes(&gov_with(2, false)),
            vec![0x82, 0x02, 0x80],
            "PotentialPParamsUpdate Nothing"
        );

        let potential_just = future_pparams_bytes(&gov_with(2, true));
        assert_eq!(
            &potential_just[..3],
            &[0x82, 0x02, 0x81],
            "PotentialPParamsUpdate Just wraps the params in a StrictMaybe array(1)"
        );

        let definite = future_pparams_bytes(&gov_with(1, true));
        assert_eq!(&definite[..2], &[0x82, 0x01], "DefinitePParamsUpdate");
        assert_ne!(
            definite[2], 0x81,
            "Definite carries the params DIRECTLY — no StrictMaybe wrapper, \
             unlike Potential"
        );

        // A Definite with no payload is not representable upstream; degrade to
        // NoPParamsUpdate rather than emit a frame cardano-cli cannot decode.
        assert_eq!(
            future_pparams_bytes(&gov_with(1, false)),
            vec![0x81, 0x00],
            "Definite without params degrades to NoPParamsUpdate"
        );
    }

    /// #992 — `GetGovState`'s embedded `DRepPulsingState` must carry the REAL
    /// pulser, and its `RatifyState` half must be byte-identical to the one
    /// `GetRatifyState` (tag 32) serves.
    ///
    /// Both were hand-written: tag 32 encoded the real values while tag 24
    /// encoded a hardcoded empty pulser next to a second copy of `EnactState`.
    /// Nothing compared them, and on a devnet where nothing is enacting the
    /// empty answer is indistinguishable from the right one — the #977 shape.
    /// Asserting the BYTES match is what makes the drift inexpressible; it
    /// fails whichever copy someone edits.
    #[test]
    fn gov_state_embeds_the_same_ratify_state_tag_32_serves() {
        use crate::node::n2c_query::types::{
            GovActionId, GovStateSnapshot, ProposalSnapshot, ProtocolParamsSnapshot,
        };

        let enacted_id = GovActionId {
            tx_id: vec![0xAA; 32],
            action_index: 0,
        };
        let expired_id = GovActionId {
            tx_id: vec![0xBB; 32],
            action_index: 3,
        };
        let proposal = ProposalSnapshot {
            tx_id: vec![0xAA; 32],
            action_index: 0,
            action_type: "InfoAction".to_string(),
            proposed_epoch: 40,
            expires_epoch: 46,
            yes_votes: 5,
            no_votes: 1,
            abstain_votes: 0,
            deposit: 100_000_000_000,
            return_addr: vec![0xe0; 29],
            anchor_url: "https://example.invalid/a.json".to_string(),
            anchor_hash: vec![0x11; 32],
            gov_action: dugite_primitives::transaction::GovAction::InfoAction,
            committee_votes: Vec::new(),
            drep_votes: Vec::new(),
            spo_votes: Vec::new(),
        };

        let pp = Box::new(ProtocolParamsSnapshot::default());
        let gov = GovStateSnapshot {
            proposals: Vec::new(),
            committee: CommitteeSnapshot::default(),
            constitution_url: String::new(),
            constitution_hash: vec![0u8; 32],
            constitution_script: None,
            cur_pparams: pp.clone(),
            prev_pparams: pp,
            enacted_pparam_update: None,
            enacted_hard_fork: None,
            enacted_committee: None,
            enacted_constitution: None,
            treasury: 12_345,
            future_pparams_tag: 2,
            future_pparams: None,
            pulser_proposals: vec![proposal.clone()],
            pulser_drep_distr: Vec::new(),
            pulser_drep_state: Vec::new(),
            pulser_pool_distr: Vec::new(),
            ratify_enacted: vec![(proposal, enacted_id)],
            ratify_expired: vec![expired_id],
            ratify_delayed: true,
        };

        // Element 6 of ConwayGovState is the DRepPulsingState: array(2)
        // [PulsingSnapshot, RatifyState].
        let encoded = encode_query_result(&QueryResult::GovState(Box::new(gov.clone())));
        let inner = strip_wrappers_for_test(&encoded);
        let mut dec = Decoder::new(&inner);
        dec.array().unwrap();
        for _ in 0..6 {
            dec.skip().unwrap();
        }
        let pulser_start = dec.position();
        dec.skip().unwrap();
        let pulser = &inner[pulser_start..dec.position()];

        let mut pd = Decoder::new(pulser);
        assert_eq!(pd.array().unwrap(), Some(2), "DRComplete is array(2)");
        let snap_start = pd.position();
        pd.skip().unwrap();
        let snap = &pulser[snap_start..pd.position()];
        let rs_start = pd.position();
        pd.skip().unwrap();
        let embedded_ratify = &pulser[rs_start..pd.position()];

        // The PulsingSnapshot half must carry the frozen candidate set, not an
        // empty placeholder.
        let mut sd = Decoder::new(snap);
        assert_eq!(sd.array().unwrap(), Some(4), "PulsingSnapshot is array(4)");
        assert_eq!(
            sd.array().unwrap(),
            Some(1),
            "psProposals must carry the pulser's frozen proposals, not array(0)"
        );

        // And the RatifyState half must be exactly what tag 32 serves.
        let tag32 = encode_query_result(&QueryResult::RatifyState {
            gov: Box::new(gov.clone()),
            enacted: gov.ratify_enacted.clone(),
            expired: gov.ratify_expired.clone(),
            delayed: gov.ratify_delayed,
        });
        let tag32_inner = strip_wrappers_for_test(&tag32);
        assert_eq!(
            embedded_ratify,
            tag32_inner.as_slice(),
            "GetGovState's embedded RatifyState must be byte-identical to \
             GetRatifyState's (#992)"
        );

        // Belt and braces: the values really are non-empty, so the equality
        // above is not two empty encodings agreeing.
        let mut rd = Decoder::new(embedded_ratify);
        assert_eq!(rd.array().unwrap(), Some(4), "RatifyState is array(4)");
        rd.skip().unwrap(); // EnactState
        assert_eq!(rd.array().unwrap(), Some(1), "rsEnacted carries one action");
        rd.skip().unwrap();
        assert_eq!(rd.tag().unwrap().as_u64(), 258, "rsExpired is a tagged set");
        assert_eq!(rd.array().unwrap(), Some(1), "rsExpired carries one id");
        rd.skip().unwrap();
        assert!(rd.bool().unwrap(), "rsDelayed must be the real value");
    }

    // ── Helper: strip the MsgResult [4, [result]] wrappers from a full
    //    encode_query_result() output and return just the inner payload. ──────
    fn strip_wrappers(cbor: &[u8]) -> Vec<u8> {
        let mut dec = Decoder::new(cbor);
        // [4, [payload]]
        dec.array().unwrap();
        dec.u32().unwrap(); // 4 = MsgResult tag
        dec.array().unwrap(); // HFC EitherMismatch success wrapper (array(1))
        cbor[dec.position()..].to_vec()
    }

    /// Issue #902 — DebugChainDepState must encode the 8-field PraosState that
    /// cardano-node 11.0.x requires, in the exact order from
    /// ouroboros-consensus-protocol 3.0.1.0 Praos.hs:
    ///   encodeVersion 0 $ encodeListLen 8 <> lastSlot <> oCertCounters
    ///     <> evolving <> candidate <> epoch <> previousEpoch <> lab
    ///     <> lastEpochBlock
    ///
    /// A 7-element payload is rejected by `enforceSize "PraosState" 8` with
    /// `Size mismatch when decoding PraosState. Expected 8, but found 7.`
    #[test]
    fn debug_chain_dep_state_encodes_eight_field_praos_state() {
        let epoch = vec![0x11u8; 32];
        let prev_epoch = vec![0x22u8; 32];
        let lab = vec![0x33u8; 32];
        let last_epoch_block = vec![0x44u8; 32];
        let result = QueryResult::DebugChainDepState {
            last_slot: 4242,
            last_slot_is_origin: false,
            ocert_counters: vec![(vec![0xAAu8; 28], 7)],
            evolving_nonce: vec![0x55u8; 32],
            candidate_nonce: vec![0x66u8; 32],
            epoch_nonce: epoch.clone(),
            previous_epoch_nonce: prev_epoch.clone(),
            lab_nonce: lab.clone(),
            last_epoch_block_nonce: last_epoch_block.clone(),
        };
        let buf = encode_query_result(&result);
        let payload = strip_wrappers(&buf);

        let mut d = Decoder::new(&payload);
        assert_eq!(
            d.array().unwrap(),
            Some(2),
            "encodeVersion wrapper is array(2)"
        );
        assert_eq!(d.u8().unwrap(), 0, "version tag must be 0");
        assert_eq!(
            d.array().unwrap(),
            Some(8),
            "PraosState must be array(8) — array(7) is rejected by cardano-node 11.0.x"
        );

        // [0] lastSlot: At 4242 -> array(2)[1, slot]
        assert_eq!(d.array().unwrap(), Some(2));
        assert_eq!(d.u8().unwrap(), 1);
        assert_eq!(d.u64().unwrap(), 4242);

        // [1] oCertCounters
        assert_eq!(d.map().unwrap(), Some(1));
        assert_eq!(d.bytes().unwrap(), &[0xAAu8; 28]);
        assert_eq!(d.u64().unwrap(), 7);

        // Helper: read one Nonce (array(1)[0] neutral, array(2)[1, bytes32]).
        let read_nonce = |d: &mut Decoder| -> Vec<u8> {
            let len = d.array().unwrap().unwrap();
            let tag = d.u8().unwrap();
            if len == 1 && tag == 0 {
                Vec::new()
            } else {
                d.bytes().unwrap().to_vec()
            }
        };

        assert_eq!(read_nonce(&mut d), vec![0x55u8; 32], "[2] evolving");
        assert_eq!(read_nonce(&mut d), vec![0x66u8; 32], "[3] candidate");
        assert_eq!(read_nonce(&mut d), epoch, "[4] epoch");
        assert_eq!(
            read_nonce(&mut d),
            prev_epoch,
            "[5] MUST be previousEpochNonce, between epoch and lab"
        );
        assert_eq!(read_nonce(&mut d), lab, "[6] lab");
        assert_eq!(read_nonce(&mut d), last_epoch_block, "[7] lastEpochBlock");
    }

    // ─── Stake distribution (tags 5, 10, 30) — pool IDs must be 28 bytes ────

    #[test]
    fn test_stake_distribution_pool_id_is_28_bytes() {
        // Build a query result with a pool ID stored as 28 bytes (normal path).
        let result = QueryResult::StakeDistribution(vec![StakePoolSnapshot {
            pool_id: vec![0xAB; 28],
            stake: 1_000_000,
            vrf_keyhash: vec![0u8; 32],
            total_circulation: 54_000_000_000_000_000,
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: map(1) { pool_id_bytes => array(2)[rational, vrf_hash] }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap(); // map header
        let pool_id_bytes = dec.bytes().unwrap();
        assert_eq!(
            pool_id_bytes.len(),
            28,
            "StakeDistribution pool_id must be 28 bytes, got {}",
            pool_id_bytes.len()
        );
    }

    #[test]
    fn test_pool_distr_pool_id_is_28_bytes() {
        let result = QueryResult::PoolDistr {
            pools: vec![PoolDistrEntry {
                pool_id: vec![0xCD; 28],
                stake: 500_000,
                vrf_keyhash: vec![0u8; 32],
                delegator_count: 1,
            }],
            total_active_stake: 1_000_000,
        };
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        let pool_id_bytes = dec.bytes().unwrap();
        assert_eq!(
            pool_id_bytes.len(),
            28,
            "PoolDistr pool_id must be 28 bytes, got {}",
            pool_id_bytes.len()
        );
    }

    // ─── DRep state (tag 25) — credential hashes must be 28 bytes ───────────

    #[test]
    fn test_drep_state_credential_hash_is_28_bytes() {
        let result = QueryResult::DRepState(vec![DRepSnapshot {
            credential_hash: vec![0x11; 28],
            credential_type: 0,
            deposit: 500_000_000,
            anchor_url: None,
            anchor_hash: None,
            expiry_epoch: 200,
            delegator_hashes: vec![],
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: map(1) { [cred_type, cred_hash_bytes] => DRepState }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap(); // map header
        dec.array().unwrap(); // Credential = array(2)
        dec.u8().unwrap(); // cred type
        let cred_hash_bytes = dec.bytes().unwrap();
        assert_eq!(
            cred_hash_bytes.len(),
            28,
            "DRepState credential_hash must be 28 bytes, got {}",
            cred_hash_bytes.len()
        );
    }

    #[test]
    fn test_drep_state_delegator_hash_is_28_bytes() {
        // A DRep with one delegator. The delegator credential hash must also be 28 bytes.
        let result = QueryResult::DRepState(vec![DRepSnapshot {
            credential_hash: vec![0x22; 28],
            credential_type: 0,
            deposit: 500_000_000,
            anchor_url: None,
            anchor_hash: None,
            expiry_epoch: 200,
            delegator_hashes: vec![vec![0x33; 28]],
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Skip past the outer map key (credential)
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        // Key: array(2) [type, hash]
        dec.array().unwrap();
        dec.u8().unwrap();
        dec.bytes().unwrap();
        // Value: DRepState array(4) [expiry, anchor, deposit, delegators_set]
        dec.array().unwrap();
        dec.u64().unwrap(); // expiry
        dec.array().unwrap(); // anchor SNothing = array(0)
        dec.u64().unwrap(); // deposit
        dec.tag().unwrap(); // tag(258) set
        dec.array().unwrap(); // array(1)
                              // Delegator: array(2) [type, hash]
        dec.array().unwrap();
        dec.u8().unwrap();
        let delegator_hash = dec.bytes().unwrap();
        assert_eq!(
            delegator_hash.len(),
            28,
            "DRep delegator credential hash must be 28 bytes, got {}",
            delegator_hash.len()
        );
    }

    // ─── Committee state (tag 27) — cold/hot credentials must be 28 bytes ───

    #[test]
    fn test_committee_state_cold_credential_is_28_bytes() {
        let result = QueryResult::CommitteeState(CommitteeSnapshot {
            members: vec![CommitteeMemberSnapshot {
                cold_credential: vec![0x44; 28],
                cold_credential_type: 0,
                hot_status: 0,
                hot_credential: Some(vec![0x55; 28]),
                hot_credential_type: 0,
                member_status: 0,
                expiry_epoch: Some(500),
            }],
            threshold: Some((2, 3)),
            current_epoch: 100,
        });
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: array(3) [member_map, maybe_threshold, epoch]
        let mut dec = Decoder::new(&inner);
        dec.array().unwrap(); // array(3)
        dec.map().unwrap(); // member_map(1)
                            // Key: Credential array(2) [type, cold_hash]
        dec.array().unwrap();
        dec.u8().unwrap();
        let cold_hash = dec.bytes().unwrap();
        assert_eq!(
            cold_hash.len(),
            28,
            "CommitteeState cold credential hash must be 28 bytes, got {}",
            cold_hash.len()
        );
    }

    #[test]
    fn test_committee_state_hot_credential_is_28_bytes() {
        let result = QueryResult::CommitteeState(CommitteeSnapshot {
            members: vec![CommitteeMemberSnapshot {
                cold_credential: vec![0x66; 28],
                cold_credential_type: 0,
                hot_status: 0, // Authorized
                hot_credential: Some(vec![0x77; 28]),
                hot_credential_type: 0,
                member_status: 0,
                expiry_epoch: Some(500),
            }],
            threshold: Some((2, 3)),
            current_epoch: 100,
        });
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.array().unwrap(); // array(3)
        dec.map().unwrap(); // member_map(1)
                            // Skip map key (cold cred)
        dec.array().unwrap(); // Credential array(2)
        dec.u8().unwrap();
        dec.bytes().unwrap(); // cold hash

        // Value: CommitteeMemberState array(4)
        dec.array().unwrap();
        // [0] HotCredAuthStatus: MemberAuthorized = array(2) [0, credential]
        dec.array().unwrap(); // array(2)
        dec.u32().unwrap(); // 0 = Authorized
                            // Inner credential: array(2) [type, hot_hash]
        dec.array().unwrap();
        dec.u8().unwrap();
        let hot_hash = dec.bytes().unwrap();
        assert_eq!(
            hot_hash.len(),
            28,
            "CommitteeState hot credential hash must be 28 bytes, got {}",
            hot_hash.len()
        );
    }

    // ─── Stake address info (tag 10) — credential hashes must be 28 bytes ───

    #[test]
    fn test_stake_address_info_credential_is_28_bytes() {
        let result = QueryResult::StakeAddressInfo(vec![StakeAddressSnapshot {
            credential_hash: vec![0x88; 28],
            delegated_pool: Some(vec![0x99; 28]),
            reward_balance: 1_000_000,
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: array(2) [delegations_map, rewards_map]
        let mut dec = Decoder::new(&inner);
        dec.array().unwrap(); // array(2)
        dec.map().unwrap(); // delegations_map(1)
                            // Key: Credential array(2) [type, hash]
        dec.array().unwrap();
        dec.u32().unwrap(); // 0 = KeyHashObj
        let cred_hash = dec.bytes().unwrap();
        assert_eq!(
            cred_hash.len(),
            28,
            "StakeAddressInfo credential hash in delegations_map must be 28 bytes, got {}",
            cred_hash.len()
        );
    }

    // ─── Stake deleg deposits (tag 22) — credential hashes must be 28 bytes ─

    #[test]
    fn test_stake_deleg_deposits_credential_is_28_bytes() {
        let result = QueryResult::StakeDelegDeposits(vec![StakeDelegDepositEntry {
            credential_hash: vec![0xAA; 28],
            credential_type: 0,
            deposit: 2_000_000,
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: map(1) { array(2)[type, hash] => deposit }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        dec.array().unwrap(); // Credential array(2)
        dec.u8().unwrap();
        let cred_hash = dec.bytes().unwrap();
        assert_eq!(
            cred_hash.len(),
            28,
            "StakeDelegDeposits credential hash must be 28 bytes, got {}",
            cred_hash.len()
        );
    }

    // ─── DRep stake distribution (tag 26) — DRep hashes must be 28 bytes ────

    #[test]
    fn test_drep_stake_distr_keyhash_is_28_bytes() {
        let result = QueryResult::DRepStakeDistr(vec![DRepStakeEntry {
            drep_type: 0, // KeyHash
            drep_hash: Some(vec![0xBB; 28]),
            stake: 1_000_000,
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: map(1) { DRep => stake }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        // DRep: array(2) [0, hash]
        dec.array().unwrap();
        dec.u8().unwrap(); // 0 = KeyHash
        let drep_hash = dec.bytes().unwrap();
        assert_eq!(
            drep_hash.len(),
            28,
            "DRepStakeDistr KeyHash DRep hash must be 28 bytes, got {}",
            drep_hash.len()
        );
    }

    // ─── Filtered vote delegatees (tag 28) — credential hashes must be 28 B ─

    #[test]
    fn test_filtered_vote_delegatees_credential_is_28_bytes() {
        let result = QueryResult::FilteredVoteDelegatees(vec![VoteDelegateeEntry {
            credential_hash: vec![0xCC; 28],
            credential_type: 0,
            drep_type: 0, // KeyHash DRep
            drep_hash: Some(vec![0xDD; 28]),
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: map(1) { Credential => DRep }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        // Key: Credential array(2) [type, hash]
        dec.array().unwrap();
        dec.u8().unwrap();
        let cred_hash = dec.bytes().unwrap();
        assert_eq!(
            cred_hash.len(),
            28,
            "FilteredVoteDelegatees stake credential hash must be 28 bytes, got {}",
            cred_hash.len()
        );
    }

    #[test]
    fn test_filtered_vote_delegatees_drep_hash_is_28_bytes() {
        let result = QueryResult::FilteredVoteDelegatees(vec![VoteDelegateeEntry {
            credential_hash: vec![0xEE; 28],
            credential_type: 0,
            drep_type: 0, // KeyHash DRep
            drep_hash: Some(vec![0xFF; 28]),
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        // Skip map key (credential)
        dec.array().unwrap(); // Credential array(2)
        dec.u8().unwrap();
        dec.bytes().unwrap();
        // Value: DRep array(2) [type, hash]
        dec.array().unwrap();
        dec.u8().unwrap(); // 0 = KeyHash
        let drep_hash = dec.bytes().unwrap();
        assert_eq!(
            drep_hash.len(),
            28,
            "FilteredVoteDelegatees DRep KeyHash must be 28 bytes, got {}",
            drep_hash.len()
        );
    }

    // ─── Stake pools set (tag 16) — pool IDs must be 28 bytes ───────────────

    #[test]
    fn test_stake_pools_set_pool_id_is_28_bytes() {
        let result = QueryResult::StakePools(vec![vec![0x12; 28]]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: tag(258) array(1) [pool_id_bytes]
        let mut dec = Decoder::new(&inner);
        dec.tag().unwrap(); // tag(258)
        dec.array().unwrap();
        let pool_id = dec.bytes().unwrap();
        assert_eq!(
            pool_id.len(),
            28,
            "StakePools pool ID must be 28 bytes, got {}",
            pool_id.len()
        );
    }

    // ─── Pool params (tag 17/19) — pool IDs and owner hashes must be 28 B ───

    #[test]
    fn test_pool_params_pool_id_is_28_bytes() {
        let result = QueryResult::PoolParams(vec![PoolParamsSnapshot {
            pool_id: vec![0x34; 28],
            vrf_keyhash: vec![0u8; 32],
            pledge: 100_000_000,
            cost: 340_000_000,
            margin_num: 5,
            margin_den: 100,
            reward_account: vec![0u8; 29],
            owners: vec![vec![0x56; 28]],
            relays: vec![],
            metadata_url: None,
            metadata_hash: None,
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: map(1) { pool_id_bytes => PoolParams(array(9)) }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();
        let pool_id = dec.bytes().unwrap();
        assert_eq!(
            pool_id.len(),
            28,
            "PoolParams map key (pool_id) must be 28 bytes, got {}",
            pool_id.len()
        );

        // PoolParams array(9): [operator, vrf, pledge, cost, margin, reward_acct, owners, relays, metadata]
        dec.array().unwrap(); // array(9)
        let operator = dec.bytes().unwrap(); // operator = pool_id again
        assert_eq!(
            operator.len(),
            28,
            "PoolParams operator field must be 28 bytes, got {}",
            operator.len()
        );
        dec.bytes().unwrap(); // vrf_keyhash (32 bytes — genuine hash)
        dec.u64().unwrap(); // pledge
        dec.u64().unwrap(); // cost
        dec.tag().unwrap(); // tag(30) rational for margin
        dec.array().unwrap();
        dec.u64().unwrap();
        dec.u64().unwrap();
        dec.bytes().unwrap(); // reward_account
        dec.tag().unwrap(); // tag(258) owners set
        dec.array().unwrap(); // array(1)
        let owner_hash = dec.bytes().unwrap();
        assert_eq!(
            owner_hash.len(),
            28,
            "PoolParams owner hash must be 28 bytes, got {}",
            owner_hash.len()
        );
    }

    // ─── PoolDistr2 (tags 36/37) — pool IDs must be 28 bytes ───────────────

    #[test]
    fn test_pool_distr2_pool_id_is_28_bytes() {
        let result = QueryResult::PoolDistr2 {
            pools: vec![PoolDistrEntry {
                pool_id: vec![0x78; 28],
                stake: 1_000_000,
                vrf_keyhash: vec![0u8; 32],
                delegator_count: 1,
            }],
            total_active_stake: 2_000_000,
        };
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: array(2) [pool_map, total_active_stake]
        let mut dec = Decoder::new(&inner);
        dec.array().unwrap(); // array(2)
        dec.map().unwrap(); // pool_map(1)
        let pool_id = dec.bytes().unwrap();
        assert_eq!(
            pool_id.len(),
            28,
            "PoolDistr2 pool_id must be 28 bytes, got {}",
            pool_id.len()
        );
    }

    // ─── Stake snapshots (tag 20) — golden vector (issue #406) ──────────────

    /// Golden vector for `GetStakeSnapshots` response encoding.
    ///
    /// Haskell's `StakeSnapshots` record encodes as `encodeListLen 4` followed by
    /// `ssStakeSnapshots` (a CBOR map from pool key hash to `StakeSnapshot`),
    /// then `ssMarkTotal`, `ssSetTotal`, `ssGoTotal` (all `NonZero Coin`).
    /// The inner `StakeSnapshot` encodes as `encodeListLen 3` of `[sMark, sSet, sGo]`.
    ///
    /// See `cardano-ledger-api`'s `Cardano.Ledger.Api.State.Query` for the reference.
    ///
    /// Layout verified: `array(4) [map(1){hash => array(3)[m,s,g]}, mark_total, set_total, go_total]`.
    #[test]
    fn test_stake_snapshots_golden_vector() {
        let result = QueryResult::StakeSnapshots(StakeSnapshotsResult {
            pools: vec![PoolStakeSnapshotEntry {
                pool_id: vec![0xAA; 28],
                mark_stake: 100,
                set_stake: 200,
                go_stake: 300,
            }],
            total_mark_stake: 1_000,
            total_set_stake: 2_000,
            total_go_stake: 3_000,
        });
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Hand-built expected bytes.
        let mut expected: Vec<u8> = Vec::new();
        expected.push(0x84); // array(4)
        expected.push(0xa1); // map(1)
        expected.extend_from_slice(&[0x58, 0x1c]); // bstr(28)
        expected.extend_from_slice(&[0xAA; 28]);
        expected.push(0x83); // array(3)
        expected.extend_from_slice(&[0x18, 100]); // uint 100
        expected.extend_from_slice(&[0x18, 200]); // uint 200
        expected.extend_from_slice(&[0x19, 0x01, 0x2c]); // uint 300 (go stake)
        expected.extend_from_slice(&[0x19, 0x03, 0xe8]); // total mark 1000
        expected.extend_from_slice(&[0x19, 0x07, 0xd0]); // total set 2000
        expected.extend_from_slice(&[0x19, 0x0b, 0xb8]); // total go 3000

        assert_eq!(
            hex::encode(&inner),
            hex::encode(&expected),
            "StakeSnapshots golden vector mismatch"
        );
    }

    /// Totals in `StakeSnapshots` are `NonZero Coin` — they must serialise as at
    /// least 1 even if the underlying value was 0. This prevents cardano-cli
    /// from choking on a zeroed out "total" field while the node is still
    /// bootstrapping epoch snapshots.
    #[test]
    fn test_stake_snapshots_zero_totals_encode_as_one() {
        let result = QueryResult::StakeSnapshots(StakeSnapshotsResult {
            pools: vec![],
            total_mark_stake: 0,
            total_set_stake: 0,
            total_go_stake: 0,
        });
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        assert_eq!(dec.array().unwrap(), Some(4));
        assert_eq!(dec.map().unwrap(), Some(0));
        assert_eq!(dec.u64().unwrap(), 1);
        assert_eq!(dec.u64().unwrap(), 1);
        assert_eq!(dec.u64().unwrap(), 1);
    }

    // ─── Stake snapshots (tag 20) — pool IDs must be 28 bytes ───────────────

    #[test]
    fn test_stake_snapshots_pool_id_is_28_bytes() {
        let result = QueryResult::StakeSnapshots(StakeSnapshotsResult {
            pools: vec![PoolStakeSnapshotEntry {
                pool_id: vec![0x9A; 28],
                mark_stake: 100,
                set_stake: 200,
                go_stake: 300,
            }],
            total_mark_stake: 100,
            total_set_stake: 200,
            total_go_stake: 300,
        });
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: array(4) [pool_map, mark_total, set_total, go_total]
        let mut dec = Decoder::new(&inner);
        dec.array().unwrap();
        dec.map().unwrap(); // pool_map(1)
        let pool_id = dec.bytes().unwrap();
        assert_eq!(
            pool_id.len(),
            28,
            "StakeSnapshots pool_id must be 28 bytes, got {}",
            pool_id.len()
        );
    }

    // ─── Default vote (tag 35) — bare word8 encoding ───────────────────────

    #[test]
    fn test_stake_pool_default_vote_bare_word8() {
        let result = QueryResult::StakePoolDefaultVote(1); // DefaultAbstain
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Inner: bare word8 (0=DefaultNo, 1=DefaultAbstain, 2=DefaultNoConfidence)
        let mut dec = Decoder::new(&inner);
        assert_eq!(dec.u8().unwrap(), 1);
    }

    // ─── SPO stake distribution (tag 30) — Map<pool_hash, Coin> ─────────

    #[test]
    fn test_spo_stake_distr_map_encoding() {
        let result = QueryResult::SPOStakeDistr(vec![
            (vec![0x33; 28], 1_000_000),
            (vec![0x44; 28], 2_000_000),
        ]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 2);
        // First entry
        assert_eq!(dec.bytes().unwrap(), &[0x33; 28]);
        assert_eq!(dec.u64().unwrap(), 1_000_000);
        // Second entry
        assert_eq!(dec.bytes().unwrap(), &[0x44; 28]);
        assert_eq!(dec.u64().unwrap(), 2_000_000);
    }

    // ─── DRep delegations (tag 39, V23+) — Map<DRep, Set<Credential>> ───

    /// Helper: decode one Set<Credential> = tag(258) array(n) [array(2) [type, hash(28)], ...].
    fn decode_credential_set(dec: &mut Decoder) -> Vec<(u8, Vec<u8>)> {
        let tag = dec.tag().expect("set tag");
        assert_eq!(tag.as_u64(), 258, "credential-set must be tagged 258");
        let n = dec.array().unwrap().unwrap();
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let inner_len = dec.array().unwrap().unwrap();
            assert_eq!(inner_len, 2, "Credential is array(2) [type, hash]");
            let ct = dec.u8().unwrap();
            let h = dec.bytes().unwrap().to_vec();
            assert_eq!(h.len(), 28, "Credential hash must be 28 bytes");
            out.push((ct, h));
        }
        out
    }

    /// KeyHash DRep mapped to a single KeyHash staking credential.
    /// Verifies the corrected outer shape `Map<DRep, Set<Credential>>`.
    #[test]
    fn test_drep_delegations_keyhash_drep_with_one_delegator() {
        let result = QueryResult::DRepDelegations(vec![DRepDelegationGroup {
            drep: DRepKey {
                drep_type: 0,
                drep_hash: Some(vec![0xBB; 28]),
            },
            credentials: vec![(0, vec![0xAA; 28])],
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 1, "Expected exactly one map entry");

        // Key: DRep = array(2) [0, hash(28)]
        let drep_len = dec.array().unwrap().unwrap();
        assert_eq!(drep_len, 2);
        assert_eq!(dec.u8().unwrap(), 0, "DRep type should be 0 (KeyHash)");
        let drep_hash = dec.bytes().unwrap();
        assert_eq!(drep_hash.len(), 28, "DRep hash must be 28 bytes");
        assert_eq!(drep_hash, &[0xBB; 28]);

        // Value: Set<Credential>
        let creds = decode_credential_set(&mut dec);
        assert_eq!(creds, vec![(0u8, vec![0xAA; 28])]);
    }

    /// ScriptHash DRep with two delegators (mixed KeyHash / ScriptHash creds).
    #[test]
    fn test_drep_delegations_scripthash_drep_multi_delegators() {
        let result = QueryResult::DRepDelegations(vec![DRepDelegationGroup {
            drep: DRepKey {
                drep_type: 1,
                drep_hash: Some(vec![0x55; 28]),
            },
            credentials: vec![(0, vec![0x11; 28]), (1, vec![0x22; 28])],
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();

        let drep_len = dec.array().unwrap().unwrap();
        assert_eq!(drep_len, 2);
        assert_eq!(dec.u8().unwrap(), 1, "DRep type should be 1 (ScriptHash)");
        assert_eq!(dec.bytes().unwrap(), &[0x55; 28]);

        let creds = decode_credential_set(&mut dec);
        assert_eq!(creds.len(), 2);
        assert_eq!(creds[0], (0u8, vec![0x11; 28]));
        assert_eq!(creds[1], (1u8, vec![0x22; 28]));
    }

    /// AlwaysAbstain (type 2) DRep — `array(1) [2]` key with one delegator.
    #[test]
    fn test_drep_delegations_always_abstain() {
        let result = QueryResult::DRepDelegations(vec![DRepDelegationGroup {
            drep: DRepKey {
                drep_type: 2,
                drep_hash: None,
            },
            credentials: vec![(0, vec![0xCC; 28])],
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();

        // Key: DRep = array(1) [2]
        let drep_arr_len = dec.array().unwrap().unwrap();
        assert_eq!(
            drep_arr_len, 1,
            "AlwaysAbstain DRep should encode as array(1)"
        );
        assert_eq!(dec.u8().unwrap(), 2, "AlwaysAbstain DRep type should be 2");

        // Value: Set<Credential>
        let creds = decode_credential_set(&mut dec);
        assert_eq!(creds, vec![(0u8, vec![0xCC; 28])]);
    }

    /// AlwaysNoConfidence (type 3) DRep with a ScriptHash delegator.
    #[test]
    fn test_drep_delegations_always_no_confidence() {
        let result = QueryResult::DRepDelegations(vec![DRepDelegationGroup {
            drep: DRepKey {
                drep_type: 3,
                drep_hash: None,
            },
            credentials: vec![(1, vec![0xDD; 28])],
        }]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.map().unwrap();

        let drep_arr_len = dec.array().unwrap().unwrap();
        assert_eq!(
            drep_arr_len, 1,
            "AlwaysNoConfidence DRep should encode as array(1)"
        );
        assert_eq!(
            dec.u8().unwrap(),
            3,
            "AlwaysNoConfidence DRep type should be 3"
        );

        let creds = decode_credential_set(&mut dec);
        assert_eq!(creds, vec![(1u8, vec![0xDD; 28])]);
    }

    /// Multiple DRep groups covering each variant (KeyHash, ScriptHash,
    /// AlwaysAbstain, AlwaysNoConfidence) plus an empty-credential-set group.
    #[test]
    fn test_drep_delegations_multi_group_all_variants() {
        let result = QueryResult::DRepDelegations(vec![
            DRepDelegationGroup {
                drep: DRepKey {
                    drep_type: 0,
                    drep_hash: Some(vec![0x10; 28]),
                },
                credentials: vec![(0, vec![0x11; 28])],
            },
            DRepDelegationGroup {
                drep: DRepKey {
                    drep_type: 1,
                    drep_hash: Some(vec![0x20; 28]),
                },
                credentials: vec![],
            },
            DRepDelegationGroup {
                drep: DRepKey {
                    drep_type: 2,
                    drep_hash: None,
                },
                credentials: vec![(0, vec![0x33; 28]), (1, vec![0x34; 28])],
            },
            DRepDelegationGroup {
                drep: DRepKey {
                    drep_type: 3,
                    drep_hash: None,
                },
                credentials: vec![(0, vec![0x44; 28])],
            },
        ]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 4, "Four groups should produce map(4)");

        // group 0: KeyHash DRep, 1 cred
        assert_eq!(dec.array().unwrap().unwrap(), 2);
        assert_eq!(dec.u8().unwrap(), 0);
        assert_eq!(dec.bytes().unwrap(), &[0x10; 28]);
        assert_eq!(decode_credential_set(&mut dec).len(), 1);

        // group 1: ScriptHash DRep, empty cred set
        assert_eq!(dec.array().unwrap().unwrap(), 2);
        assert_eq!(dec.u8().unwrap(), 1);
        assert_eq!(dec.bytes().unwrap(), &[0x20; 28]);
        assert!(decode_credential_set(&mut dec).is_empty());

        // group 2: AlwaysAbstain DRep, 2 creds
        assert_eq!(dec.array().unwrap().unwrap(), 1);
        assert_eq!(dec.u8().unwrap(), 2);
        assert_eq!(decode_credential_set(&mut dec).len(), 2);

        // group 3: AlwaysNoConfidence DRep, 1 cred
        assert_eq!(dec.array().unwrap().unwrap(), 1);
        assert_eq!(dec.u8().unwrap(), 3);
        assert_eq!(decode_credential_set(&mut dec).len(), 1);
    }

    /// Empty result encodes as `map(0)`.
    #[test]
    fn test_drep_delegations_empty_is_empty_map() {
        let result = QueryResult::DRepDelegations(vec![]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 0, "Empty DRepDelegations should encode as map(0)");
    }

    /// Roundtrip: encode → decode → re-encode and ensure byte-equality.
    /// This is a structural roundtrip (we re-build the same `DRepDelegationGroup`
    /// values from the decoded CBOR and compare).
    #[test]
    fn test_drep_delegations_roundtrip() {
        let original = vec![
            DRepDelegationGroup {
                drep: DRepKey {
                    drep_type: 0,
                    drep_hash: Some(vec![0x77; 28]),
                },
                credentials: vec![(0, vec![0x01; 28]), (1, vec![0x02; 28])],
            },
            DRepDelegationGroup {
                drep: DRepKey {
                    drep_type: 2,
                    drep_hash: None,
                },
                credentials: vec![(0, vec![0x03; 28])],
            },
        ];
        let result = QueryResult::DRepDelegations(original.clone());
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        // Decode back into Vec<DRepDelegationGroup>.
        let mut dec = Decoder::new(&inner);
        let map_len = dec.map().unwrap().unwrap();
        let mut decoded: Vec<DRepDelegationGroup> = Vec::with_capacity(map_len as usize);
        for _ in 0..map_len {
            let arr_len = dec.array().unwrap().unwrap();
            let drep_type = dec.u8().unwrap();
            let drep_hash = if arr_len == 2 {
                Some(dec.bytes().unwrap().to_vec())
            } else {
                None
            };
            let credentials = decode_credential_set(&mut dec);
            decoded.push(DRepDelegationGroup {
                drep: DRepKey {
                    drep_type,
                    drep_hash,
                },
                credentials,
            });
        }
        assert_eq!(decoded.len(), original.len());
        for (a, b) in decoded.iter().zip(original.iter()) {
            assert_eq!(a.drep, b.drep);
            assert_eq!(a.credentials, b.credentials);
        }

        // Re-encode and confirm byte-identical CBOR (canonical roundtrip).
        let re_encoded = encode_query_result(&QueryResult::DRepDelegations(decoded));
        assert_eq!(re_encoded, encoded, "roundtrip must be byte-identical");
    }

    // ── N2C protocol wire-format regression tests (Haskell compatibility) ─────
    //
    // These tests verify that Dugite's N2C encoding matches the Haskell
    // cardano-node wire format exactly, so that cardano-cli works against our node.
    //
    // Reference: ouroboros-network/protocols/lib/Ouroboros/Network/Protocol/
    //            LocalStateQuery/Codec.hs and ouroboros-consensus HFC encoding.

    /// MsgResult outer structure: array(2)[4, payload]
    ///
    /// Haskell: `encode (StateQuerying query) (MsgResult result) =
    ///   encodeListLen 2 <> encodeWord 4 <> encodeResult query result`
    #[test]
    fn test_msg_result_outer_tag_is_4() {
        let result = QueryResult::EpochNo(100);
        let encoded = encode_query_result(&result);

        let mut dec = Decoder::new(&encoded);
        let arr_len = dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 2, "MsgResult must be a 2-element array");

        let tag = dec.u32().unwrap();
        assert_eq!(
            tag, 4,
            "MsgResult tag must be 4 (not 6, which is MsgReAcquire)"
        );
    }

    /// HFC EitherMismatch success wrapper for BlockQuery results: array(1)[result]
    ///
    /// Haskell `encodeEitherMismatch HardForkNodeToClientEnabled (Right a) =
    ///   encodeListLen 1 <> enc a`
    /// Discriminant is array LENGTH (1=success, 2=mismatch), NOT a leading integer tag.
    #[test]
    fn test_hfc_either_mismatch_success_is_array1() {
        // Use a BlockQuery result (needs EitherMismatch wrapping)
        let result = QueryResult::EpochNo(42);
        let encoded = encode_query_result(&result);

        let mut dec = Decoder::new(&encoded);
        dec.array().unwrap(); // outer array(2)
        dec.u32().unwrap(); // tag 4

        // The HFC success wrapper must be array(1), not array(2) with a leading "1" tag
        let wrapper_len = dec.array().unwrap().unwrap();
        assert_eq!(
            wrapper_len, 1,
            "HFC EitherMismatch success wrapper must be array(1), not array(2) with tag"
        );
    }

    /// Top-level queries (SystemStart, ChainBlockNo, ChainPoint) must NOT have the
    /// HFC EitherMismatch wrapper — Haskell encodes them as [4, result] directly.
    #[test]
    fn test_top_level_query_no_hfc_wrapper() {
        let result = QueryResult::SystemStart("2022-04-01T00:00:00Z".to_string());
        let encoded = encode_query_result(&result);

        let mut dec = Decoder::new(&encoded);
        dec.array().unwrap(); // outer array(2)
        dec.u32().unwrap(); // tag 4

        // For SystemStart, the payload must NOT start with array(1) — it starts
        // directly with an array(3) [year, day_of_year, pico_of_day]
        let inner_type = dec.datatype().unwrap();
        assert_eq!(
            inner_type,
            minicbor::data::Type::Array,
            "SystemStart result should be array(3) directly, no HFC wrapper"
        );
        let inner_len = dec.array().unwrap().unwrap();
        assert_eq!(
            inner_len, 3,
            "SystemStart must be array(3)[year, day, pico]"
        );
    }

    /// GetUTxOByAddress: empty UTxO set encodes as [4, [map(0)]]
    ///
    /// Verified against Haskell: empty map = CBOR a0.
    /// Full wire bytes: 82 04 81 a0
    ///   82 = array(2), 04 = MsgResult tag, 81 = array(1), a0 = map(0)
    #[test]
    fn test_utxo_empty_wire_format() {
        let result = QueryResult::UtxoByAddress(vec![]);
        let encoded = encode_query_result(&result);

        // Verify exact bytes: 82 04 81 a0
        assert_eq!(
            encoded,
            vec![0x82, 0x04, 0x81, 0xa0],
            "Empty UTxO MsgResult must be exactly [0x82, 0x04, 0x81, 0xa0] = array(2)[4, array(1)[map(0)]]"
        );
    }

    /// GetCurrentEpoch: epoch number 100 encodes as [4, [100]]
    ///
    /// Full wire bytes: 82 04 81 18 64
    ///   82=array(2), 04=tag, 81=array(1), 18 64=uint(100)
    #[test]
    fn test_epoch_no_wire_format() {
        let result = QueryResult::EpochNo(100);
        let encoded = encode_query_result(&result);

        assert_eq!(
            encoded,
            vec![0x82, 0x04, 0x81, 0x18, 0x64],
            "EpochNo(100) MsgResult must be [0x82, 0x04, 0x81, 0x18, 0x64]"
        );
    }

    /// Verify strip_wrappers helper correctly removes [4, []] envelope.
    #[test]
    fn test_strip_wrappers_correctness() {
        // Build [4, [42]] — MsgResult with EpochNo(42) inner value
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(4).unwrap(); // MsgResult tag
        enc.array(1).unwrap(); // HFC success wrapper
        enc.u32(42).unwrap(); // inner value
        let inner = strip_wrappers(&buf);

        let mut dec = Decoder::new(&inner);
        let val = dec.u32().unwrap();
        assert_eq!(
            val, 42,
            "strip_wrappers must correctly expose inner payload"
        );
    }

    /// GetLedgerTip golden vector (issue #407).
    ///
    /// Captured from cardano-node 10.6.2, 43-byte payload:
    ///   82 04 81 82 1a 06884258 5820 344bc3f7b7b3686a181a3c73e4a4050122b888e1b596f2c3a398a6a7fc2c9602
    ///
    /// Structure:
    ///   82 04 — MsgResult [4, ...]
    ///   81    — HFC EitherMismatch success wrapper array(1)
    ///   82    — Point array(2)
    ///   1a 06884258 — slot = 109_576_792
    ///   5820 <32B> — hash bytes
    ///
    /// GetLedgerTip returns a bare Point, NOT a Tip — no block_no in the payload.
    #[test]
    fn test_ledger_tip_wire_format_bare_point() {
        let hash_hex = "344bc3f7b7b3686a181a3c73e4a4050122b888e1b596f2c3a398a6a7fc2c9602";
        let hash: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&hash_hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let result = QueryResult::LedgerTip {
            slot: 109_576_792,
            hash: hash.clone(),
        };
        let encoded = encode_query_result(&result);

        let mut expected = vec![0x82, 0x04, 0x81, 0x82, 0x1a];
        expected.extend_from_slice(&109_576_792u32.to_be_bytes());
        expected.push(0x58);
        expected.push(0x20);
        expected.extend_from_slice(&hash);

        assert_eq!(
            encoded, expected,
            "GetLedgerTip MsgResult must match the captured cardano-node 10.6.2 bare-Point wire format (issue #407)"
        );
        assert_eq!(encoded.len(), 43, "captured payload length is 43 bytes");
    }

    /// QueryAnytime result (CurrentEra) must NOT be wrapped in HFC EitherMismatch.
    /// Wire format: [4, era_word]
    #[test]
    fn test_current_era_no_hfc_wrapper() {
        let result = QueryResult::CurrentEra(6); // 6 = Conway
        let encoded = encode_query_result(&result);

        let mut dec = Decoder::new(&encoded);
        dec.array().unwrap(); // array(2)
        dec.u32().unwrap(); // tag 4

        // CurrentEra is a bare word (not wrapped in array(1))
        let era = dec.u32().unwrap();
        assert_eq!(
            era, 6,
            "CurrentEra must be a bare word(6) with no HFC wrapper"
        );
    }

    // ── V23 LedgerPeerSnapshot golden tests (issue #456) ─────────────────────

    /// V23 BigLedgerPeers outer discriminator must be `uint(2)` and the
    /// payload must be `array(3)[Point, NetworkMagic, indef pools]`.
    #[test]
    fn test_ledger_peer_snapshot_v23_big_golden() {
        use crate::node::n2c_query::types::{LedgerPeerEntry, RelaySnapshot};
        use dugite_primitives::block::Point;
        let peers = vec![LedgerPeerEntry {
            pool_id: vec![1u8; 28],
            stake: 1_000_000,
            relays: vec![RelaySnapshot::SingleHostName {
                port: Some(3001),
                dns_name: "r.example".to_string(),
            }],
        }];
        let result = QueryResult::LedgerPeerSnapshotV23 {
            big: true,
            anchor: Point::Origin,
            network_magic: 2,
            peers,
        };
        let encoded = encode_query_result(&result);
        let payload = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&payload);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 2, "Big variant discriminator must be 2");
        assert_eq!(dec.array().unwrap(), Some(3));
        // Point: Origin → array(1)[0]
        assert_eq!(dec.array().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 0);
        // NetworkMagic
        assert_eq!(dec.u32().unwrap(), 2);
        // Pools (indefinite array)
        assert_eq!(dec.array().unwrap(), None, "pools must be indef-length");
    }

    /// V23 AllLedgerPeers outer discriminator must be `uint(3)`, each pool entry
    /// must be `array(2)[PoolStake_rat, relays]` (no AccPoolStake), and the
    /// Block-point case must encode as `array(3)[1, slot, bstr(32)]`.
    #[test]
    fn test_ledger_peer_snapshot_v23_all_golden_with_block_point() {
        use crate::node::n2c_query::types::{LedgerPeerEntry, RelaySnapshot};
        use dugite_primitives::block::Point;
        use dugite_primitives::hash::Hash;
        use dugite_primitives::time::SlotNo;
        let anchor = Point::Specific(SlotNo(12345), Hash::<32>([7u8; 32]));
        let peers = vec![LedgerPeerEntry {
            pool_id: vec![1u8; 28],
            stake: 1_000_000,
            relays: vec![RelaySnapshot::SingleHostName {
                port: Some(3001),
                dns_name: "r.example".to_string(),
            }],
        }];
        let result = QueryResult::LedgerPeerSnapshotV23 {
            big: false,
            anchor,
            network_magic: 764824073,
            peers,
        };
        let encoded = encode_query_result(&result);
        let payload = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&payload);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 3, "All variant discriminator must be 3");
        assert_eq!(dec.array().unwrap(), Some(3));
        // Point: Block → array(3)[1, slot, hash]
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 1);
        assert_eq!(dec.u64().unwrap(), 12345);
        let hash = dec.bytes().unwrap();
        assert_eq!(hash.len(), 32);
        assert_eq!(hash, &[7u8; 32]);
        // NetworkMagic
        assert_eq!(dec.u32().unwrap(), 764824073);
        // Pools indef
        assert_eq!(dec.array().unwrap(), None);
        // First pool: array(2)[PoolStake_rat, relays_indef] — no AccPoolStake
        assert_eq!(
            dec.array().unwrap(),
            Some(2),
            "All variant pool entry must be array(2), not array(3)"
        );
        assert_eq!(dec.array().unwrap(), Some(2)); // rational
        let _num = dec.u64().unwrap();
        let _den = dec.u64().unwrap();
        assert_eq!(dec.array().unwrap(), None, "relays must be indef-length");
    }

    // ─── Issue #434: Conway PParams positional order ────────────────────────
    //
    // Conway PParams encodes as a 31-element positional array where the order
    // is determined by `eraPParams @ConwayEra` in
    // `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs`.
    // Per current ledger master, `ppGovProtocolVersion` sits at index **12**
    // (right after `tau` and before `ppMinPoolCost`) — NOT at the end.
    //
    // An earlier note in this file claimed protocolVersion was last; that was
    // based on an outdated oracle snapshot and broke `gov-state` decoding in
    // cardano-cli 10.15 against the ConwayEra schema. See issue #434.
    #[test]
    fn test_pparams_conway_positional_order_issue_434() {
        let mut pp = ProtocolParamsSnapshot {
            // Use small distinct sentinel values so a slot mismatch is obvious.
            min_fee_a: 0xA0,
            min_fee_b: 0xA1,
            max_block_body_size: 0xA2,
            max_tx_size: 0xA3,
            max_block_header_size: 0xA4,
            key_deposit: 0xA5,
            pool_deposit: 0xA6,
            e_max: 0xA7,
            n_opt: 0xA8,
            min_pool_cost: 0xC1,
            ada_per_utxo_byte: 0xC2,
            cost_models_v3: Some(vec![100, 200, 300]),
            protocol_version_major: 11,
            protocol_version_minor: 0,
            ..Default::default()
        };
        // Drop V1/V2 to keep the cost-model map deterministic.
        pp.cost_models_v1 = None;
        pp.cost_models_v2 = None;

        let result = QueryResult::ProtocolParams(Box::new(pp));
        let encoded = encode_query_result(&result);
        let payload = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&payload);
        assert_eq!(
            dec.array().unwrap(),
            Some(31),
            "Conway PParams must be a 31-element positional array"
        );

        // [0..=8] flat uints
        assert_eq!(dec.u64().unwrap(), 0xA0); // txFeePerByte
        assert_eq!(dec.u64().unwrap(), 0xA1); // txFeeFixed
        assert_eq!(dec.u64().unwrap(), 0xA2); // maxBBSize
        assert_eq!(dec.u64().unwrap(), 0xA3); // maxTxSize
        assert_eq!(dec.u64().unwrap(), 0xA4); // maxBHSize
        assert_eq!(dec.u64().unwrap(), 0xA5); // keyDeposit
        assert_eq!(dec.u64().unwrap(), 0xA6); // poolDeposit
        assert_eq!(dec.u64().unwrap(), 0xA7); // eMax
        assert_eq!(dec.u64().unwrap(), 0xA8); // nOpt

        // [9..=11] tagged rationals
        for _ in 0..3 {
            let tag = dec.tag().unwrap();
            assert_eq!(tag, minicbor::data::Tag::new(30));
            assert_eq!(dec.array().unwrap(), Some(2));
            let _ = dec.u64().unwrap();
            let _ = dec.u64().unwrap();
        }

        // [12] protocolVersion = array(2)[major, minor] (issue #434)
        assert_eq!(
            dec.array().unwrap(),
            Some(2),
            "index 12 must be protocolVersion as array(2); if this is a uint, protocolVersion is misplaced"
        );
        assert_eq!(dec.u64().unwrap(), 11);
        assert_eq!(dec.u64().unwrap(), 0);

        // [13] minPoolCost
        assert_eq!(dec.u64().unwrap(), 0xC1);
        // [14] coinsPerUTxOByte
        assert_eq!(dec.u64().unwrap(), 0xC2);

        // [15] costModels map
        assert_eq!(dec.map().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 2); // PlutusV3 = 2
        assert_eq!(dec.array().unwrap(), Some(3));
        for _ in 0..3 {
            let _ = dec.i64().unwrap();
        }

        // [16] prices [tag30, tag30]
        assert_eq!(dec.array().unwrap(), Some(2));
        for _ in 0..2 {
            let _ = dec.tag().unwrap();
            assert_eq!(dec.array().unwrap(), Some(2));
            let _ = dec.u64().unwrap();
            let _ = dec.u64().unwrap();
        }

        // [17] maxTxExUnits [mem, steps]
        assert_eq!(dec.array().unwrap(), Some(2));
        let _ = dec.u64().unwrap();
        let _ = dec.u64().unwrap();

        // [18] maxBlockExUnits [mem, steps]
        assert_eq!(dec.array().unwrap(), Some(2));
        let _ = dec.u64().unwrap();
        let _ = dec.u64().unwrap();

        // [19..=21] flat uints
        let _ = dec.u64().unwrap(); // maxValSize
        let _ = dec.u64().unwrap(); // collateralPct
        let _ = dec.u64().unwrap(); // maxCollateralInputs

        // [22] poolVotingThresholds array(5) of tag30
        assert_eq!(dec.array().unwrap(), Some(5));
        for _ in 0..5 {
            let _ = dec.tag().unwrap();
            assert_eq!(dec.array().unwrap(), Some(2));
            let _ = dec.u64().unwrap();
            let _ = dec.u64().unwrap();
        }

        // [23] drepVotingThresholds array(10) of tag30
        assert_eq!(dec.array().unwrap(), Some(10));
        for _ in 0..10 {
            let _ = dec.tag().unwrap();
            assert_eq!(dec.array().unwrap(), Some(2));
            let _ = dec.u64().unwrap();
            let _ = dec.u64().unwrap();
        }

        // [24..=29] flat uints (committee + gov action + drep)
        for _ in 0..6 {
            let _ = dec.u64().unwrap();
        }

        // [30] minFeeRefScriptCostPerByte (tag30) — last per current ledger master
        let _ = dec.tag().unwrap();
        assert_eq!(dec.array().unwrap(), Some(2));
        let _ = dec.u64().unwrap();
        let _ = dec.u64().unwrap();
    }

    // ─── Reference script (CIP-33) encoding tests ──────────────────────────────

    /// Build a `UtxoSnapshot` with the given `ScriptRef` and no other optional fields,
    /// then encode it via `encode_query_result` and return the inner CBOR map bytes
    /// (key stripped of the MsgResult + HFC wrapper + outer UTxO map header + TxIn key).
    fn encode_utxo_with_script_ref(
        script_ref: Option<dugite_primitives::transaction::ScriptRef>,
    ) -> Vec<u8> {
        let utxo = UtxoSnapshot {
            tx_hash: vec![0xAA; 32],
            output_index: 0,
            address_bytes: vec![0x61u8; 29],
            lovelace: 5_000_000,
            multi_asset: vec![],
            datum_hash: None,
            inline_datum: None,
            script_ref,
            raw_cbor: None,
        };
        let result = QueryResult::UtxoByAddress(vec![utxo]);
        let encoded = encode_query_result(&result);
        // Strip [4, [ map(1) [ [tx_hash, 0] -> output_cbor ] ]]
        // Strip MsgResult + HFC wrapper:
        let inner = strip_wrappers(&encoded);
        // inner = map(1) { [tx_hash, idx] => output }
        let mut dec = Decoder::new(&inner);
        dec.map().unwrap(); // map(1)
        dec.array().unwrap(); // [tx_hash, idx]
        dec.bytes().unwrap(); // tx_hash
        dec.u32().unwrap(); // idx
                            // remaining bytes = output CBOR
        inner[dec.position()..].to_vec()
    }

    /// UTxO output without a script_ref must encode as map(2) {0: addr, 1: value}.
    #[test]
    fn utxo_output_no_script_ref_is_map_2() {
        let output_cbor = encode_utxo_with_script_ref(None);
        let mut dec = Decoder::new(&output_cbor);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(
            map_len, 2,
            "Output without script_ref must be map(2) {{0: addr, 1: value}}"
        );
        // key 0 (address) and key 1 (value) must be present, no key 3
        let k0 = dec.u32().unwrap();
        assert_eq!(k0, 0);
        dec.bytes().unwrap(); // address bytes
        let k1 = dec.u32().unwrap();
        assert_eq!(k1, 1);
        dec.skip().unwrap(); // value
    }

    /// UTxO output with a PlutusV2 script_ref must encode as map(3) with key 3 =
    /// tag(24) bstr([2, script_bytes]).  This is the most common reference-script
    /// variant on Conway-era Cardano.
    ///
    /// cardano-cli parses this and emits:
    /// ```json
    /// "referenceScript": {
    ///   "script": {"cborHex": "<hex>", "description": "", "type": "PlutusScriptV2"},
    ///   "scriptLanguage": "PlutusScriptLanguage PlutusScriptV2"
    /// }
    /// ```
    #[test]
    fn utxo_output_plutus_v2_script_ref_roundtrip() {
        use dugite_primitives::transaction::ScriptRef;

        // Minimal PlutusV2 script bytes (always-true-v2 inner CBOR from local-devnet)
        let script_bytes = hex::decode("480100002221200101").unwrap();
        let output_cbor =
            encode_utxo_with_script_ref(Some(ScriptRef::PlutusV2(script_bytes.clone())));

        let mut dec = Decoder::new(&output_cbor);
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 3, "Output with PlutusV2 script_ref must be map(3)");

        // key 0: address
        assert_eq!(dec.u32().unwrap(), 0);
        dec.bytes().unwrap();
        // key 1: value
        assert_eq!(dec.u32().unwrap(), 1);
        dec.skip().unwrap();
        // key 3: script_ref = tag(24) bstr(array(2)[2, script_bytes])
        assert_eq!(dec.u32().unwrap(), 3, "Key 3 must be script_ref");
        let tag = dec.tag().unwrap();
        assert_eq!(tag.as_u64(), 24, "script_ref must be wrapped in tag(24)");
        let inner_cbor = dec.bytes().unwrap();
        // inner_cbor = encode_script_ref(PlutusV2) = array(2)[2, bstr(script_bytes)]
        let mut inner_dec = Decoder::new(inner_cbor);
        let arr_len = inner_dec.array().unwrap().unwrap();
        assert_eq!(arr_len, 2, "encode_script_ref must produce array(2)");
        let variant = inner_dec.u32().unwrap();
        assert_eq!(variant, 2, "PlutusV2 variant tag must be 2");
        let decoded_script = inner_dec.bytes().unwrap();
        assert_eq!(
            decoded_script, script_bytes,
            "script bytes must round-trip exactly"
        );
    }

    /// UTxO output with a PlutusV1 script_ref uses variant tag 1.
    #[test]
    fn utxo_output_plutus_v1_script_ref_roundtrip() {
        use dugite_primitives::transaction::ScriptRef;

        let script_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let output_cbor =
            encode_utxo_with_script_ref(Some(ScriptRef::PlutusV1(script_bytes.clone())));

        let mut dec = Decoder::new(&output_cbor);
        assert_eq!(dec.map().unwrap().unwrap(), 3);
        dec.u32().unwrap();
        dec.bytes().unwrap(); // addr
        dec.u32().unwrap();
        dec.skip().unwrap(); // value
        assert_eq!(dec.u32().unwrap(), 3); // key 3
        assert_eq!(dec.tag().unwrap().as_u64(), 24);
        let inner_cbor = dec.bytes().unwrap();
        let mut inner = Decoder::new(inner_cbor);
        inner.array().unwrap();
        assert_eq!(inner.u32().unwrap(), 1, "PlutusV1 variant tag must be 1");
        assert_eq!(inner.bytes().unwrap(), script_bytes);
    }

    /// UTxO output with a PlutusV3 script_ref uses variant tag 3.
    #[test]
    fn utxo_output_plutus_v3_script_ref_roundtrip() {
        use dugite_primitives::transaction::ScriptRef;

        let script_bytes = vec![0x01, 0x02, 0x03];
        let output_cbor =
            encode_utxo_with_script_ref(Some(ScriptRef::PlutusV3(script_bytes.clone())));

        let mut dec = Decoder::new(&output_cbor);
        assert_eq!(dec.map().unwrap().unwrap(), 3);
        dec.u32().unwrap();
        dec.bytes().unwrap();
        dec.u32().unwrap();
        dec.skip().unwrap();
        assert_eq!(dec.u32().unwrap(), 3); // key 3
        assert_eq!(dec.tag().unwrap().as_u64(), 24);
        let inner_cbor = dec.bytes().unwrap();
        let mut inner = Decoder::new(inner_cbor);
        inner.array().unwrap();
        assert_eq!(inner.u32().unwrap(), 3, "PlutusV3 variant tag must be 3");
        assert_eq!(inner.bytes().unwrap(), script_bytes);
    }

    /// UTxO output with a NativeScript reference uses variant tag 0 and encodes
    /// the script as a nested CBOR array, NOT raw bytes.
    ///
    /// cardano-cli parses this and emits:
    /// ```json
    /// "referenceScript": {
    ///   "script": {"cborHex": "<hex>", "description": "", "type": "SimpleScript"},
    ///   "scriptLanguage": "SimpleScriptLanguage"
    /// }
    /// ```
    #[test]
    fn utxo_output_native_script_ref_roundtrip() {
        use dugite_primitives::hash::Hash32;
        use dugite_primitives::transaction::{NativeScript, ScriptRef};

        // ScriptPubkey native script
        let key_hash = Hash32::from_bytes([0xCC; 32]);
        let native = NativeScript::ScriptPubkey(key_hash);
        let output_cbor =
            encode_utxo_with_script_ref(Some(ScriptRef::NativeScript(native.clone())));

        let mut dec = Decoder::new(&output_cbor);
        assert_eq!(dec.map().unwrap().unwrap(), 3);
        dec.u32().unwrap();
        dec.bytes().unwrap();
        dec.u32().unwrap();
        dec.skip().unwrap();
        assert_eq!(dec.u32().unwrap(), 3); // key 3
        assert_eq!(dec.tag().unwrap().as_u64(), 24);
        let inner_cbor = dec.bytes().unwrap();
        let mut inner = Decoder::new(inner_cbor);
        // NativeScript variant tag = 0, body = native script encoding
        let arr_len = inner.array().unwrap().unwrap();
        assert_eq!(arr_len, 2, "encode_script_ref must produce array(2)");
        assert_eq!(
            inner.u32().unwrap(),
            0,
            "NativeScript variant tag must be 0"
        );
        // Native ScriptPubkey encodes as array(2)[0, hash28]
        let ns_arr = inner.array().unwrap().unwrap();
        assert_eq!(ns_arr, 2);
        assert_eq!(inner.u32().unwrap(), 0); // ScriptPubkey tag
        let key_bytes = inner.bytes().unwrap();
        // encode_native_script truncates Hash32 to 28 bytes on the wire
        assert_eq!(key_bytes.len(), 28);
        assert_eq!(key_bytes, &[0xCC; 28]);
    }

    /// UTxO output with a datum_hash AND a script_ref must be map(3) with keys
    /// 0 (address), 1 (value), 2 (datum_hash), 3 (script_ref) — the map count
    /// must be 4 in this case.
    #[test]
    fn utxo_output_datum_hash_and_script_ref_is_map_4() {
        use dugite_primitives::transaction::ScriptRef;

        let script_bytes = vec![0x01];
        let utxo = UtxoSnapshot {
            tx_hash: vec![0xBB; 32],
            output_index: 1,
            address_bytes: vec![0x61u8; 29],
            lovelace: 2_000_000,
            multi_asset: vec![],
            datum_hash: Some(vec![0xDD; 32]),
            inline_datum: None,
            script_ref: Some(ScriptRef::PlutusV2(script_bytes)),
            raw_cbor: None,
        };
        let result = QueryResult::UtxoByAddress(vec![utxo]);
        let encoded = encode_query_result(&result);
        let inner = strip_wrappers(&encoded);

        let mut dec = Decoder::new(&inner);
        dec.map().unwrap(); // UTxO map(1)
        dec.array().unwrap();
        dec.bytes().unwrap(); // tx_hash
        dec.u32().unwrap(); // idx
                            // output map
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(
            map_len, 4,
            "Output with datum_hash + script_ref must be map(4)"
        );
        // key order: 0, 1, 2, 3
        let keys: Vec<u32> = (0..4)
            .map(|_| {
                let k = dec.u32().unwrap();
                dec.skip().unwrap();
                k
            })
            .collect();
        assert_eq!(keys, [0, 1, 2, 3], "Keys must appear in order 0,1,2,3");
    }
}
