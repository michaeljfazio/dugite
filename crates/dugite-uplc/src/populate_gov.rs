//! Translation: `dugite_primitives` governance + certificates →
//! [`crate::script_context`] equivalents.
//!
//! Phase-2 evaluation needs to lift the Conway-era governance fields
//! (votes, proposal procedures, certificates) into the shapes Plutus
//! V3 validators observe. The destination types are:
//!
//! - `Voter` (CommitteeVoter / DrepVoter / StakePoolVoter)
//! - `Vote` (No / Yes / Abstain)
//! - `GovActionId` (tx_id + idx)
//! - `ProposalProcedure(Data)` — opaque-Data wrapper
//! - `TxCert(Data)` — opaque-Data wrapper carrying the cert's
//!   Constr-encoded shape
//!
//! The `ProposalProcedure` / `TxCert` wrappers are deliberately
//! opaque on the script_context side — the Haskell reference exposes
//! them via `Data` decoding too, so the script-observable contract is
//! "give me a Constr-encoded blob of the expected shape." This
//! module owns that blob construction.
//!
//! ## Coverage
//!
//! The Conway-era cert variants are translated to their Plutus V3
//! `TxCert` constructors. Legacy variants (MIR, GenesisKeyDelegation)
//! surface a typed `Internal` error — Plutus phase-2 never sees them
//! in Conway because they are not legal on the chain after the era
//! transition.
//!
//! ## GovernanceAction Data encoding
//!
//! Haskell reference: `PlutusLedgerApi.V3.Contexts` (plutus master),
//! `makeIsDataSchemaIndexed ''GovernanceAction [...]`:
//!
//! | Constr | Haskell constructor | Fields |
//! |--------|---------------------|--------|
//! | 0 | ParameterChange | [Maybe GovActionId, ChangedParameters, Maybe ScriptHash] |
//! | 1 | HardForkInitiation | [Maybe GovActionId, ProtocolVersion] |
//! | 2 | TreasuryWithdrawals | [Map Credential Lovelace, Maybe ScriptHash] |
//! | 3 | NoConfidence | [Maybe GovActionId] |
//! | 4 | UpdateCommittee | [Maybe GovActionId, [ColdCredential], Map ColdCredential Epoch, Rational] |
//! | 5 | NewConstitution | [Maybe GovActionId, Constitution] |
//! | 6 | InfoAction | [] |
//!
//! Supporting types (all `makeIsDataSchemaIndexed` with index 0):
//! - `ProtocolVersion = Constr 0 [I major, I minor]`
//! - `Constitution    = Constr 0 [Maybe ScriptHash]`
//! - `GovernanceActionId = Constr 0 [B txid32, I idx]`  (V3 bare-txid form)
//! - `ColdCommitteeCredential` / `HotCommitteeCredential` / `DRepCredential` —
//!   all `newtype deriving ToData` from `V2.Credential` → encode as bare
//!   `Credential` data (`Constr 0/1 [B28]`)
//! - `Rational = Constr 0 [I numerator, I denominator]`
//!
//! ## ProposalProcedure Data encoding
//!
//! `makeIsDataSchemaIndexed ''ProposalProcedure [('ProposalProcedure, 0)]`
//! → `Constr 0 [deposit, returnAddr (Credential), governanceAction]`

use crate::data::Data;
use crate::phase_two::PhaseTwoError;
use crate::script_context::{
    GovActionId as PlGovActionId, ProposalProcedure as PlProposalProcedure, TxCert, Vote as PlVote,
    Voter as PlVoter,
};
use crate::tx_info_populate::credential_to_plutus;
use dugite_primitives::credentials::Credential as PrimCred;
use dugite_primitives::transaction::{
    Certificate as PrimCert, CostModels, DRep as PrimDRep, GovAction as PrimGovAction,
    GovActionId as PrimGovActionId, ProposalProcedure as PrimProposal, ProtocolParamUpdate,
    Rational, Vote as PrimVote, Voter as PrimVoter, VotingProcedure as PrimVotingProcedure,
};
use num_bigint::BigInt;
use std::collections::BTreeMap;

// ────────────────────────────────────────────────────────────────────
// Voter / Vote / GovActionId
// ────────────────────────────────────────────────────────────────────

/// Translate a primitive [`PrimVoter`] into the Plutus [`PlVoter`].
///
/// `ConstitutionalCommittee` → `CommitteeVoter`, `DRep` → `DrepVoter`,
/// `StakePool(Hash<32>)` → `StakePoolVoter(PubKeyHash)` (with the 4-byte
/// internal padding stripped — same convention as `required_signers`).
pub fn voter_to_plutus(v: &PrimVoter) -> PlVoter {
    match v {
        PrimVoter::ConstitutionalCommittee(c) => PlVoter::CommitteeVoter(credential_to_plutus(c)),
        PrimVoter::DRep(c) => PlVoter::DrepVoter(credential_to_plutus(c)),
        PrimVoter::StakePool(h) => {
            let mut bytes = [0u8; 28];
            bytes.copy_from_slice(&h.0[..28]);
            PlVoter::StakePoolVoter(bytes)
        }
    }
}

/// Translate a primitive [`PrimVote`] into the Plutus [`PlVote`]. The
/// three variants match across exactly.
pub fn vote_to_plutus(v: &PrimVote) -> PlVote {
    match v {
        PrimVote::No => PlVote::No,
        PrimVote::Yes => PlVote::Yes,
        PrimVote::Abstain => PlVote::Abstain,
    }
}

/// Translate a primitive [`PrimGovActionId`] into the Plutus
/// [`PlGovActionId`]. `transaction_id` (32 bytes) → `tx_id`,
/// `action_index` (u32) → `idx` (u64).
pub fn gov_action_id_to_plutus(g: &PrimGovActionId) -> PlGovActionId {
    PlGovActionId {
        tx_id: g.transaction_id.0,
        idx: g.action_index as u64,
    }
}

/// Translate the tx body's `voting_procedures` map into the
/// `Vec<(Voter, Vec<(GovActionId, Vote)>)>` shape that V3 TxInfo
/// exposes (`txInfoVotes`).
///
/// The OUTER `Voter` order must be the ledger `Map Voter` order, NOT the
/// dugite `BTreeMap<Voter,_>` iteration order. dugite's derived `Voter`
/// `Ord` tie-breaks CC/DRep inner credentials Key < Script, whereas the
/// Haskell ledger `Voter`/`Credential` derives Script < Key. We therefore
/// re-order the entries by [`PrimVoter::cmp_ledger`] (Script < Key inner)
/// to match `Map.toList` over the ledger `VotingProcedures` map. The inner
/// `GovActionId` order is preserved (lex by `(tx_id, idx)`), matching the
/// ledger `Map GovActionId` order and canonical CBOR.
pub fn voting_procedures_to_plutus(
    vp: &BTreeMap<PrimVoter, BTreeMap<PrimGovActionId, PrimVotingProcedure>>,
) -> Vec<(PlVoter, Vec<(PlGovActionId, PlVote)>)> {
    // Collect the voter entries and sort by the LEDGER Voter order
    // (Script < Key inner credential), rather than relying on dugite's
    // derived `BTreeMap<Voter,_>` iteration (Key < Script inner).
    let mut entries: Vec<(&PrimVoter, &BTreeMap<PrimGovActionId, PrimVotingProcedure>)> =
        vp.iter().collect();
    entries.sort_by(|a, b| a.0.cmp_ledger(b.0));
    let mut out: Vec<(PlVoter, Vec<(PlGovActionId, PlVote)>)> = Vec::with_capacity(entries.len());
    for (voter, votes) in entries {
        let pl_voter = voter_to_plutus(voter);
        let mut pl_votes: Vec<(PlGovActionId, PlVote)> = Vec::with_capacity(votes.len());
        for (gid, vp_inner) in votes {
            pl_votes.push((gov_action_id_to_plutus(gid), vote_to_plutus(&vp_inner.vote)));
        }
        out.push((pl_voter, pl_votes));
    }
    out
}

// ────────────────────────────────────────────────────────────────────
// ProposalProcedure
// ────────────────────────────────────────────────────────────────────

/// Translate a primitive [`PrimProposal`] into the Plutus
/// [`PlProposalProcedure`] (an opaque `Data` wrapper).
///
/// Haskell: `makeIsDataSchemaIndexed ''ProposalProcedure [('ProposalProcedure, 0)]`
/// → `Constr 0 [deposit, returnAddr, governanceAction]`
///
/// Reference: `plutus-ledger-api/src/PlutusLedgerApi/V3/Contexts.hs`
/// `cardano-ledger/eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs`
/// `transProposal` / `transGovAction`.
pub fn proposal_to_plutus(p: &PrimProposal) -> Result<PlProposalProcedure, PhaseTwoError> {
    let return_addr = dugite_primitives::address::Address::from_bytes(&p.return_addr)
        .map_err(|e| PhaseTwoError::Internal(format!("proposal_to_plutus: return_addr: {e}")))?;
    let return_stake_cred = match return_addr {
        dugite_primitives::address::Address::Reward(r) => credential_to_plutus(&r.stake),
        other => {
            return Err(PhaseTwoError::Internal(format!(
                "proposal_to_plutus: return_addr must be Reward, got {other:?}"
            )));
        }
    };
    let return_addr_data = return_stake_cred.to_data();
    let deposit_data = Data::I(BigInt::from(p.deposit.0));
    let gov_action_data = gov_action_to_data(&p.gov_action)?;
    Ok(PlProposalProcedure(Data::Constr(
        0,
        vec![deposit_data, return_addr_data, gov_action_data],
    )))
}

/// Encode a [`PrimGovAction`] as the Plutus V3 `GovernanceAction` Data value.
///
/// Haskell: `makeIsDataSchemaIndexed ''GovernanceAction` in
/// `PlutusLedgerApi.V3.Contexts`:
///
/// ```haskell
/// ('ParameterChange,   0)  -- [Maybe GovActionId, ChangedParameters, Maybe ScriptHash]
/// ('HardForkInitiation, 1) -- [Maybe GovActionId, ProtocolVersion]
/// ('TreasuryWithdrawals, 2)-- [Map Credential Lovelace, Maybe ScriptHash]
/// ('NoConfidence,      3)  -- [Maybe GovActionId]
/// ('UpdateCommittee,   4)  -- [Maybe GovActionId, [ColdCredential], Map ColdCred Epoch, Rational]
/// ('NewConstitution,   5)  -- [Maybe GovActionId, Constitution]
/// ('InfoAction,        6)  -- []
/// ```
///
/// Supporting encodings (all cross-validated against Haskell source):
///
/// - `Maybe GovernanceActionId`: `Nothing = Constr 1 []`, `Just id = Constr 0 [id.to_data()]`
///   where `id = Constr 0 [B txid32, I idx]` (V3 bare-txid, from `GovActionId` Constr 0 +
///   `TxId deriving newtype ToData`).
/// - `ProtocolVersion = Constr 0 [I major, I minor]`
///   (`makeIsDataSchemaIndexed ''ProtocolVersion [('ProtocolVersion, 0)]`)
/// - `Constitution = Constr 0 [Maybe ScriptHash]`
///   (`makeIsDataSchemaIndexed ''Constitution [('Constitution, 0)]`)
/// - `ChangedParameters` (ParameterChange body) — `Data::Map [(I ppuTag, value)]`
///   built by `ppu_to_changed_parameters_data` (#761), keyed by the Conway
///   `ppuTag` integer; the Conway guardrails scripts `unMapData` this field.
/// - `ColdCommitteeCredential` — `newtype deriving ToData` from `V2.Credential` →
///   bare `Credential` data (`Constr 0 [B28]` / `Constr 1 [B28]`)
/// - `Rational = Constr 0 [I numerator, I denominator]`
///   (`makeIsDataSchemaIndexed ''Rational [('Rational, 0)]` in plutus-tx)
/// - `TreasuryWithdrawals` map key is `V2.Credential` directly (the stake
///   credential from the reward address), mirroring `transAccountAddress`.
pub fn gov_action_to_data(action: &PrimGovAction) -> Result<Data, PhaseTwoError> {
    let d = match action {
        // Constr 0 [Maybe GovActionId, ChangedParameters, Maybe ScriptHash]
        PrimGovAction::ParameterChange {
            prev_action_id,
            protocol_param_update,
            policy_hash,
        } => Data::Constr(
            0,
            vec![
                maybe_gov_action_id(prev_action_id.as_ref()),
                // ChangedParameters (#761): the Conway guardrails scripts that
                // validate ParameterChange proposals `unMapData` this field and
                // inspect the changed parameters by integer key, so it MUST be a
                // Data::Map [(I ppuTag, value)] — NOT the old `Constr 0 []`
                // placeholder (which failed with "unMapData on non-Map Data" on
                // mainnet txs 51f495aa / b2a591ac, which propose a PlutusV3
                // cost-model change).
                ppu_to_changed_parameters_data(protocol_param_update),
                maybe_script_hash(policy_hash.as_ref()),
            ],
        ),
        // Constr 1 [Maybe GovActionId, ProtocolVersion]
        // ProtocolVersion = Constr 0 [I major, I minor]
        PrimGovAction::HardForkInitiation {
            prev_action_id,
            protocol_version: (major, minor),
        } => Data::Constr(
            1,
            vec![
                maybe_gov_action_id(prev_action_id.as_ref()),
                // ProtocolVersion: makeIsDataSchemaIndexed [('ProtocolVersion, 0)]
                Data::Constr(
                    0,
                    vec![Data::I(BigInt::from(*major)), Data::I(BigInt::from(*minor))],
                ),
            ],
        ),
        // Constr 2 [Map Credential Lovelace, Maybe ScriptHash]
        // Map key = Credential (stake cred extracted from reward address)
        // using transAccountAddress → transCred → bare Credential data.
        PrimGovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash,
        } => {
            // The Plutus V3 `Map Credential Lovelace` must follow the LEDGER
            // `Map RewardAccount Coin` order (`Credential` Script < Key), NOT
            // the raw 29-byte reward-account blob order (which `BTreeMap`
            // iteration gives — header high-nibble `0xE_`=key < `0xF_`=script
            // ⇒ Key < Script). Haskell `Conway.TxInfo.transGovAction`:
            // `transMap = PV3.unsafeFromList . map f . Map.toList` preserves
            // the ledger `Map`'s `Credential Ord` (ScriptHashObj < KeyHashObj).
            // Reuse the gauntlet-proven `ledger_ordered_withdrawals` helper
            // (issue #26, same `&BTreeMap<Vec<u8>, Lovelace>` input) — it
            // re-sorts the blob order to ledger order via `cmp_ledger`.
            let ordered = crate::tx_info_populate::ledger_ordered_withdrawals(withdrawals)?;
            let entries: Vec<(Data, Data)> = ordered
                .into_iter()
                .map(|(stake, amount)| {
                    (
                        credential_to_plutus(&stake).to_data(),
                        Data::I(BigInt::from(amount.0)),
                    )
                })
                .collect();
            Data::Constr(
                2,
                vec![Data::Map(entries), maybe_script_hash(policy_hash.as_ref())],
            )
        }
        // Constr 3 [Maybe GovActionId]
        PrimGovAction::NoConfidence { prev_action_id } => {
            Data::Constr(3, vec![maybe_gov_action_id(prev_action_id.as_ref())])
        }
        // Constr 4 [Maybe GovActionId, [ColdCredential], Map ColdCredential Epoch, Rational]
        // ColdCommitteeCredential: newtype deriving ToData from V2.Credential → bare Credential
        // Rational: makeIsDataSchemaIndexed [('Rational, 0)] → Constr 0 [I num, I den]
        PrimGovAction::UpdateCommittee {
            prev_action_id,
            members_to_remove,
            members_to_add,
            threshold,
        } => {
            // [ColdCredential] — list of credentials to remove.
            // Haskell `Conway.TxInfo.transGovAction` builds this from
            // `Set.toList membersToRemove`, which is in ledger `Credential`
            // Ord (Script < Key) and deduped. dugite stores it as a `Vec` in
            // wire order, so re-sort by `cmp_ledger` (Script < Key) and dedup
            // (Set parity — a no-op for valid txs) before building the list.
            let mut remove_sorted: Vec<&PrimCred> = members_to_remove.iter().collect();
            remove_sorted.sort_by(|a, b| a.cmp_ledger(b));
            remove_sorted.dedup();
            let remove_list: Vec<Data> = remove_sorted
                .into_iter()
                .map(|c| credential_to_plutus(c).to_data())
                .collect();
            // Map ColdCredential Epoch — credentials to add with their term
            // expiry epoch. Same ledger-order requirement: the Plutus map must
            // follow the ledger `Map Credential EpochNo` order (Script < Key),
            // NOT dugite's derived `BTreeMap<Credential, _>` iteration order
            // (Key < Script). `Conway.TxInfo.transGovAction` uses
            // `transMap = PV3.unsafeFromList . map f . Map.toList`, preserving
            // the ledger `Map`'s `Credential Ord`. Collect + re-sort by
            // `cmp_ledger` before mapping.
            let mut add_sorted: Vec<(&PrimCred, &u64)> = members_to_add.iter().collect();
            add_sorted.sort_by(|a, b| a.0.cmp_ledger(b.0));
            let add_map: Vec<(Data, Data)> = add_sorted
                .into_iter()
                .map(|(c, epoch)| {
                    (
                        credential_to_plutus(c).to_data(),
                        Data::I(BigInt::from(*epoch)),
                    )
                })
                .collect();
            // Rational: makeIsDataSchemaIndexed [('Rational, 0)] → Constr 0 [I num, I den].
            // #837 item 2: reduced to lowest terms — see `reduce_rational`.
            let (threshold_num, threshold_den) =
                reduce_rational(threshold.numerator, threshold.denominator);
            let rational_data = Data::Constr(
                0,
                vec![
                    Data::I(BigInt::from(threshold_num)),
                    Data::I(BigInt::from(threshold_den)),
                ],
            );
            Data::Constr(
                4,
                vec![
                    maybe_gov_action_id(prev_action_id.as_ref()),
                    Data::List(remove_list),
                    Data::Map(add_map),
                    rational_data,
                ],
            )
        }
        // Constr 5 [Maybe GovActionId, Constitution]
        // Constitution: makeIsDataSchemaIndexed [('Constitution, 0)] → Constr 0 [Maybe ScriptHash]
        PrimGovAction::NewConstitution {
            prev_action_id,
            constitution,
        } => {
            let constitution_data = Data::Constr(
                0,
                vec![maybe_script_hash(constitution.script_hash.as_ref())],
            );
            Data::Constr(
                5,
                vec![
                    maybe_gov_action_id(prev_action_id.as_ref()),
                    constitution_data,
                ],
            )
        }
        // Constr 6 []
        PrimGovAction::InfoAction => Data::Constr(6, vec![]),
    };
    Ok(d)
}

/// Reduce a raw wire `(numerator, denominator)` pair to lowest terms
/// (#837 item 2).
///
/// Haskell's on-chain `Rational`/`BoundedRatio` (`UnitInterval`,
/// `NonNegativeInterval`, and the plain governance-threshold `Rational`)
/// is always constructed via GHC's `Ratio` smart constructor (`%`), which
/// UNCONDITIONALLY reduces to lowest terms
/// (`libs/cardano-ledger-binary/.../DecCBOR.hs::decodeIntegralRational`
/// does `toInteger n % toInteger d`, and `%`'s definition divides both
/// sides by `gcd n d`). It is structurally impossible to observe a
/// non-reduced `Ratio` value anywhere downstream in cardano-ledger,
/// including at the `ToPlutusData`/ScriptContext boundary. dugite's wire
/// decoder (`dugite-serialization::read_rational`) preserves the raw
/// on-wire pair without reducing it, so a non-canonically-encoded
/// on-chain rational (e.g. `9/18`) would otherwise emit a byte-different
/// `Data` than Haskell's `1/2`. Normalize here, at the ScriptContext
/// construction boundary, to match.
///
/// `gcd(0, 0) == 0` is the only degenerate input (both zero); returned
/// unchanged rather than dividing by zero — this should never occur for
/// an already phase-1-validated `Rational` (a zero denominator is
/// rejected at CBOR decode time in Haskell and in dugite).
fn reduce_rational(numerator: u64, denominator: u64) -> (u64, u64) {
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let g = gcd(numerator, denominator);
    match (numerator.checked_div(g), denominator.checked_div(g)) {
        (Some(n), Some(d)) => (n, d),
        _ => (numerator, denominator),
    }
}

/// Conway `CostModels` → Plutus `Data::Map [(I lang, List [I cost..])]`,
/// languages in ascending order (V1=0, V2=1, V3=2, V4=3). Mirrors Haskell
/// `flattenCostModels` + `ToPlutusData (Map Word8 [Int64])`
/// (`Cardano.Ledger.Plutus.ToPlutusData`). Cost values are signed (the V3
/// model contains negative entries, e.g. -900) so each is `Data::I` of a
/// signed `BigInt`.
fn cost_models_to_data(cm: &CostModels) -> Data {
    let mut entries: Vec<(Data, Data)> = Vec::new();
    let mut push_lang = |lang: i64, costs: &Option<Vec<i64>>| {
        if let Some(c) = costs {
            entries.push((
                Data::I(BigInt::from(lang)),
                Data::List(c.iter().map(|x| Data::I(BigInt::from(*x))).collect()),
            ));
        }
    };
    push_lang(0, &cm.plutus_v1);
    push_lang(1, &cm.plutus_v2);
    push_lang(2, &cm.plutus_v3);
    push_lang(3, &cm.plutus_v4);
    // #770: unknown-language entries (keys ≥ 4) in ascending key order. Haskell
    // `toPlutusData . flattenCostModels` includes them, so a guardrail script
    // that reads the cost-models field of a ParameterChange sees them too.
    for (key, costs) in &cm.unknown_cost_models {
        entries.push((
            Data::I(BigInt::from(i64::from(*key))),
            Data::List(costs.iter().map(|x| Data::I(BigInt::from(*x))).collect()),
        ));
    }
    Data::Map(entries)
}

/// Build the Plutus V3 `ChangedParameters` Data for a Conway `ParameterChange`
/// action: a `Data::Map [(I ppuTag, value)]` containing ONLY the fields the
/// update actually sets, keyed by the Haskell `ppuTag` integer (= the Conway
/// PParamsUpdate CBOR sparse-map key), in ascending key order. (#761)
///
/// Haskell reference (cardano-ledger-core `Cardano.Ledger.Core.PParams`):
/// ```haskell
/// instance ConwayEraScript era => ToPlutusData (PParamsUpdate era) where
///   toPlutusData ppu = P.Map $ mapMaybe ppToData (eraPParams @era)
///     where ppToData PParam{ppUpdate} = do
///             PParamUpdate{ppuTag, ppuLens} <- ppUpdate
///             t <- strictMaybeToMaybe $ ppu ^. ppuLens
///             pure (P.I (toInteger ppuTag), toPlutusData t)
/// ```
///
/// Value encodings (`Cardano.Ledger.Plutus.ToPlutusData`):
/// - Coin / Word / EpochInterval: `I n`
/// - UnitInterval / NonNegativeInterval: `List [I num, I den]` (NOT `Constr`!)
/// - ExUnits: `List [I mem, I steps]` (mem first)
/// - Prices (ExUnitPrices): `List [memPrice, stepPrice]` (each a rational `List`)
/// - CostModels: `Map [(I lang, List [I cost..])]` (V1=0, V2=1, V3=2)
/// - PoolVotingThresholds: `List` of 5 rationals (fixed order)
/// - DRepVotingThresholds: `List` of 10 rationals (fixed order)
///
/// `SNothing` (unset) fields are omitted. `protocolVersion`, `d`, and
/// `extraEntropy` are NOT updatable Conway PParams and have no ppuTag.
fn ppu_to_changed_parameters_data(ppu: &ProtocolParamUpdate) -> Data {
    fn int(v: u64) -> Data {
        Data::I(BigInt::from(v))
    }
    // UnitInterval / NonNegativeInterval → List [I num, I den] (hand-written
    // `ToPlutusData Rational` in plutus-ledger-api — List, not Constr).
    //
    // #837 item 2: reduced to lowest terms before emission — see
    // `reduce_rational` doc comment for why this must match Haskell exactly.
    fn rat(r: &Rational) -> Data {
        let (num, den) = reduce_rational(r.numerator, r.denominator);
        Data::List(vec![Data::I(BigInt::from(num)), Data::I(BigInt::from(den))])
    }
    let mut e: Vec<(Data, Data)> = Vec::new();
    // 0–11: classic Shelley parameters.
    if let Some(v) = ppu.min_fee_a {
        e.push((int(0), int(v)));
    }
    if let Some(v) = ppu.min_fee_b {
        e.push((int(1), int(v)));
    }
    if let Some(v) = ppu.max_block_body_size {
        e.push((int(2), int(v)));
    }
    if let Some(v) = ppu.max_tx_size {
        e.push((int(3), int(v)));
    }
    if let Some(v) = ppu.max_block_header_size {
        e.push((int(4), int(v)));
    }
    if let Some(v) = ppu.key_deposit {
        e.push((int(5), int(v.0)));
    }
    if let Some(v) = ppu.pool_deposit {
        e.push((int(6), int(v.0)));
    }
    if let Some(v) = ppu.e_max {
        e.push((int(7), int(v)));
    }
    if let Some(v) = ppu.n_opt {
        e.push((int(8), int(v)));
    }
    if let Some(ref v) = ppu.a0 {
        e.push((int(9), rat(v)));
    }
    if let Some(ref v) = ppu.rho {
        e.push((int(10), rat(v)));
    }
    if let Some(ref v) = ppu.tau {
        e.push((int(11), rat(v)));
    }
    // 16–24: Alonzo/Babbage parameters (keys 12–15 do not exist in Conway).
    if let Some(v) = ppu.min_pool_cost {
        e.push((int(16), int(v.0)));
    }
    if let Some(v) = ppu.ada_per_utxo_byte {
        e.push((int(17), int(v.0)));
    }
    if let Some(ref cm) = ppu.cost_models {
        e.push((int(18), cost_models_to_data(cm)));
    }
    if let Some(ref p) = ppu.execution_costs {
        e.push((
            int(19),
            Data::List(vec![rat(&p.mem_price), rat(&p.step_price)]),
        ));
    }
    if let Some(ref u) = ppu.max_tx_ex_units {
        e.push((int(20), Data::List(vec![int(u.mem), int(u.steps)])));
    }
    if let Some(ref u) = ppu.max_block_ex_units {
        e.push((int(21), Data::List(vec![int(u.mem), int(u.steps)])));
    }
    if let Some(v) = ppu.max_val_size {
        e.push((int(22), int(v)));
    }
    if let Some(v) = ppu.collateral_percentage {
        e.push((int(23), int(v)));
    }
    if let Some(v) = ppu.max_collateral_inputs {
        e.push((int(24), int(v)));
    }
    // 25: poolVotingThresholds — 5 rationals, set together (all-or-nothing).
    // Order: motionNoConfidence, committeeNormal, committeeNoConfidence,
    //        hardForkInitiation, ppSecurityGroup.
    if let (Some(a), Some(b), Some(c), Some(d), Some(f)) = (
        ppu.pvt_motion_no_confidence.as_ref(),
        ppu.pvt_committee_normal.as_ref(),
        ppu.pvt_committee_no_confidence.as_ref(),
        ppu.pvt_hard_fork.as_ref(),
        ppu.pvt_pp_security_group.as_ref(),
    ) {
        e.push((
            int(25),
            Data::List(vec![rat(a), rat(b), rat(c), rat(d), rat(f)]),
        ));
    }
    // 26: dRepVotingThresholds — 10 rationals, set together. Order:
    //   motionNoConfidence, committeeNormal, committeeNoConfidence,
    //   updateToConstitution, hardForkInitiation, ppNetworkGroup,
    //   ppEconomicGroup, ppTechnicalGroup, ppGovGroup, treasuryWithdrawal.
    #[allow(clippy::type_complexity)]
    let dvt: Option<[&Rational; 10]> = match (
        ppu.dvt_no_confidence.as_ref(),
        ppu.dvt_committee_normal.as_ref(),
        ppu.dvt_committee_no_confidence.as_ref(),
        ppu.dvt_constitution.as_ref(),
        ppu.dvt_hard_fork.as_ref(),
        ppu.dvt_pp_network_group.as_ref(),
        ppu.dvt_pp_economic_group.as_ref(),
        ppu.dvt_pp_technical_group.as_ref(),
        ppu.dvt_pp_gov_group.as_ref(),
        ppu.dvt_treasury_withdrawal.as_ref(),
    ) {
        (
            Some(a),
            Some(b),
            Some(c),
            Some(d),
            Some(f),
            Some(g),
            Some(h),
            Some(j),
            Some(k),
            Some(l),
        ) => Some([a, b, c, d, f, g, h, j, k, l]),
        _ => None,
    };
    if let Some(t) = dvt {
        e.push((int(26), Data::List(t.iter().map(|r| rat(r)).collect())));
    }
    // 27–33: Conway governance parameters.
    if let Some(v) = ppu.min_committee_size {
        e.push((int(27), int(v)));
    }
    if let Some(v) = ppu.committee_term_limit {
        e.push((int(28), int(v)));
    }
    if let Some(v) = ppu.gov_action_lifetime {
        e.push((int(29), int(v)));
    }
    if let Some(v) = ppu.gov_action_deposit {
        e.push((int(30), int(v.0)));
    }
    if let Some(v) = ppu.drep_deposit {
        e.push((int(31), int(v.0)));
    }
    if let Some(v) = ppu.drep_activity {
        e.push((int(32), int(v)));
    }
    if let Some(ref v) = ppu.min_fee_ref_script_cost_per_byte {
        // NonNegativeInterval -> Haskell `ToPlutusData Rational` = List [I num, I den]
        // (key 33). Emit the full rational, mirroring a0/rho/tau via `rat()`.
        e.push((int(33), rat(v)));
    }
    Data::Map(e)
}

/// Encode `Maybe GovernanceActionId` as Plutus Data.
///
/// `Nothing = Constr 1 []`, `Just id = Constr 0 [id_data]`.
/// `GovernanceActionId` uses V3 bare-txid form:
/// `Constr 0 [B txid32, I action_idx]` (matching `GovActionId.to_data()` in
/// `script_context.rs`, which already encodes bare bytes for V3).
fn maybe_gov_action_id(id: Option<&PrimGovActionId>) -> Data {
    match id {
        None => Data::Constr(1, vec![]),
        Some(gid) => {
            // GovernanceActionId = Constr 0 [B txid32, I action_idx]
            // V3 TxId = bare BuiltinByteString (deriving newtype ToData).
            // This matches GovActionId::to_data() in script_context.rs.
            let id_data = Data::Constr(
                0,
                vec![
                    Data::B(gid.transaction_id.0.to_vec()),
                    Data::I(BigInt::from(gid.action_index)),
                ],
            );
            Data::Constr(0, vec![id_data])
        }
    }
}

/// Encode `Maybe ScriptHash` as Plutus Data.
///
/// `Nothing = Constr 1 []`, `Just h = Constr 0 [B28]`.
fn maybe_script_hash(h: Option<&dugite_primitives::hash::Hash28>) -> Data {
    match h {
        None => Data::Constr(1, vec![]),
        Some(sh) => Data::Constr(0, vec![Data::B(sh.0.to_vec())]),
    }
}

/// Translate the tx body's `proposal_procedures: Vec<ProposalProcedure>`
/// into `Vec<PlProposalProcedure>` preserving input order.
pub fn proposals_to_plutus(
    proposals: &[PrimProposal],
) -> Result<Vec<PlProposalProcedure>, PhaseTwoError> {
    proposals.iter().map(proposal_to_plutus).collect()
}

// ────────────────────────────────────────────────────────────────────
// Certificates
// ────────────────────────────────────────────────────────────────────

/// Translate a primitive [`PrimCert`] into the Plutus `TxCert(Data)`
/// wrapper.
///
/// Each variant maps to the Plutus V3 `TxCert` constructor with the
/// same tag the Haskell reference uses (see
/// `PlutusLedgerApi.V3.Contexts.TxCert`):
///
/// | Constr | Plutus shape                                            |
/// |--------|---------------------------------------------------------|
/// | 0      | `TxCertRegStaking(cred, Option<deposit>)`               |
/// | 1      | `TxCertUnRegStaking(cred, Option<refund>)`              |
/// | 2      | `TxCertDelegStaking(cred, Delegatee)`                   |
/// | 3      | `TxCertRegDeleg(cred, Delegatee, deposit)`              |
/// | 4      | `TxCertRegDRep(cred, deposit)`                          |
/// | 5      | `TxCertUpdateDRep(cred)`                                |
/// | 6      | `TxCertUnRegDRep(cred, refund)`                         |
/// | 7      | `TxCertPoolRegister(poolId, vrf)`                       |
/// | 8      | `TxCertPoolRetire(poolId, epoch)`                       |
/// | 9      | `TxCertAuthHotCommittee(cold, hot)`                     |
/// | 10     | `TxCertResignColdCommittee(cold)`                       |
///
/// Pre-Conway MIR + GenesisKeyDelegation certs surface a typed
/// `Internal` error — Conway phase-2 never sees them on chain.
pub fn certificate_to_plutus(c: &PrimCert) -> Result<TxCert, PhaseTwoError> {
    let data = match c {
        PrimCert::StakeRegistration(cred) => {
            Data::Constr(0, vec![cred_data(cred), option_int(None)])
        }
        PrimCert::StakeDeregistration(cred) => {
            Data::Constr(1, vec![cred_data(cred), option_int(None)])
        }
        PrimCert::ConwayStakeRegistration {
            credential,
            deposit,
        } => Data::Constr(0, vec![cred_data(credential), option_int(Some(deposit.0))]),
        PrimCert::ConwayStakeDeregistration { credential, refund } => {
            Data::Constr(1, vec![cred_data(credential), option_int(Some(refund.0))])
        }
        PrimCert::StakeDelegation {
            credential,
            pool_hash,
        } => Data::Constr(
            2,
            vec![cred_data(credential), delegatee_to_pool(&pool_hash.0)],
        ),
        PrimCert::RegStakeDeleg {
            credential,
            pool_hash,
            deposit,
        } => Data::Constr(
            3,
            vec![
                cred_data(credential),
                delegatee_to_pool(&pool_hash.0),
                Data::I(BigInt::from(deposit.0)),
            ],
        ),
        PrimCert::RegDRep {
            credential,
            deposit,
            ..
        } => Data::Constr(
            4,
            vec![cred_data(credential), Data::I(BigInt::from(deposit.0))],
        ),
        PrimCert::UpdateDRep { credential, .. } => Data::Constr(5, vec![cred_data(credential)]),
        PrimCert::UnregDRep { credential, refund } => Data::Constr(
            6,
            vec![cred_data(credential), Data::I(BigInt::from(refund.0))],
        ),
        PrimCert::PoolRegistration(params) => Data::Constr(
            7,
            vec![
                Data::B(params.operator.0.to_vec()),
                Data::B(params.vrf_keyhash.0.to_vec()),
            ],
        ),
        PrimCert::PoolRetirement { pool_hash, epoch } => Data::Constr(
            8,
            vec![Data::B(pool_hash.0.to_vec()), Data::I(BigInt::from(*epoch))],
        ),
        PrimCert::CommitteeHotAuth {
            cold_credential,
            hot_credential,
        } => Data::Constr(
            9,
            vec![cred_data(cold_credential), cred_data(hot_credential)],
        ),
        PrimCert::CommitteeColdResign {
            cold_credential, ..
        } => Data::Constr(10, vec![cred_data(cold_credential)]),
        // Combined certs: emit as TxCertRegDeleg / TxCertDelegStaking shapes,
        // with the DRep threaded through the `Delegatee` payload (#815).
        PrimCert::VoteDelegation { credential, drep } => {
            Data::Constr(2, vec![cred_data(credential), delegatee_vote(drep)])
        }
        PrimCert::StakeVoteDelegation {
            credential,
            pool_hash,
            drep,
        } => Data::Constr(
            2,
            vec![
                cred_data(credential),
                delegatee_stake_vote(&pool_hash.0, drep),
            ],
        ),
        PrimCert::RegStakeVoteDeleg {
            credential,
            pool_hash,
            deposit,
            drep,
        } => Data::Constr(
            3,
            vec![
                cred_data(credential),
                delegatee_stake_vote(&pool_hash.0, drep),
                Data::I(BigInt::from(deposit.0)),
            ],
        ),
        PrimCert::VoteRegDeleg {
            credential,
            deposit,
            drep,
        } => Data::Constr(
            3,
            vec![
                cred_data(credential),
                delegatee_vote(drep),
                Data::I(BigInt::from(deposit.0)),
            ],
        ),
        PrimCert::GenesisKeyDelegation { .. } => {
            return Err(PhaseTwoError::Internal(
                "certificate_to_plutus: GenesisKeyDelegation is pre-Conway-only".to_string(),
            ));
        }
        PrimCert::MoveInstantaneousRewards { .. } => {
            return Err(PhaseTwoError::Internal(
                "certificate_to_plutus: MIR cert is pre-Conway-only".to_string(),
            ));
        }
    };
    Ok(TxCert(data))
}

/// Encode a primitive Credential as the Plutus `Credential` Data
/// shape (`Constr 0 [PubKey]` / `Constr 1 [Script]`). Reuses the
/// existing `Credential::to_data` impl from script_context.
fn cred_data(c: &PrimCred) -> Data {
    credential_to_plutus(c).to_data()
}

/// Encode a primitive Credential as a V1/V2 Plutus `StakingCredential`
/// (`StakingHash Credential` = `Constr 0 [Credential]`). The V1/V2 `DCert`
/// type uses `StakingCredential` everywhere a credential appears, NOT the
/// bare `Credential` (and NOT the Conway V3 `TxCert` shapes).
fn staking_hash_data(c: &PrimCred) -> Data {
    Data::Constr(0, vec![cred_data(c)])
}

/// Translate a ledger certificate into the **PlutusV1/V2** `DCert` Data,
/// which is a completely different schema from the Conway V3 `TxCert` built
/// by [`certificate_to_plutus`]. Byte-exact with cardano-ledger
/// `Cardano.Ledger.Alonzo.Plutus.TxInfo::transTxCert` /
/// `PlutusLedgerApi.V1.DCert` (`makeIsDataSchemaIndexed ''DCert`):
///
/// ```text
/// DCertDelegRegKey   (StakingHash cred)        = Constr 0 [Constr 0 [Credential]]
/// DCertDelegDeRegKey (StakingHash cred)        = Constr 1 [Constr 0 [Credential]]
/// DCertDelegDelegate (StakingHash cred) poolId = Constr 2 [Constr 0 [Credential], B pool28]
/// DCertPoolRegister  poolId vrfKeyHash         = Constr 3 [B pool28, B vrf32]
/// DCertPoolRetire    poolId epoch              = Constr 4 [B pool28, I epoch]
/// DCertGenesis                                 = Constr 5 []
/// DCertMir                                     = Constr 6 []
/// ```
///
/// `Credential` is `Constr 0 [B]` (PubKey) / `Constr 1 [B]` (Script); the
/// delegatee pool key and pool ids are BARE `B` (PubKeyHash newtype).
///
/// Conway-only certs (deposit registration, DRep/committee, vote delegation)
/// cannot legally co-exist with a V1/V2 script — cardano-ledger fails the
/// translation (`transTxCertCommon` returns `Nothing`) — so they surface here
/// as an internal error rather than a silently-wrong shape.
pub fn certificate_to_plutus_v1v2(c: &PrimCert) -> Result<TxCert, PhaseTwoError> {
    let conway_only = |name: &str| {
        Err(PhaseTwoError::Internal(format!(
            "certificate_to_plutus_v1v2: {name} is a Conway-only cert and cannot \
             appear in a PlutusV1/V2 script context (ledger rejects the tx)"
        )))
    };
    let data = match c {
        PrimCert::StakeRegistration(cred) => Data::Constr(0, vec![staking_hash_data(cred)]),
        PrimCert::StakeDeregistration(cred) => Data::Constr(1, vec![staking_hash_data(cred)]),
        PrimCert::StakeDelegation {
            credential,
            pool_hash,
        } => Data::Constr(
            2,
            vec![staking_hash_data(credential), Data::B(pool_hash.0.to_vec())],
        ),
        PrimCert::PoolRegistration(params) => Data::Constr(
            3,
            vec![
                Data::B(params.operator.0.to_vec()),
                Data::B(params.vrf_keyhash.0.to_vec()),
            ],
        ),
        PrimCert::PoolRetirement { pool_hash, epoch } => Data::Constr(
            4,
            vec![Data::B(pool_hash.0.to_vec()), Data::I(BigInt::from(*epoch))],
        ),
        PrimCert::GenesisKeyDelegation { .. } => Data::Constr(5, vec![]),
        PrimCert::MoveInstantaneousRewards { .. } => Data::Constr(6, vec![]),
        // Conway registration / deregistration (with an explicit deposit /
        // refund) translate to the SAME legacy V1/V2 `DCert` as the no-deposit
        // Shelley form — the deposit / refund is silently DROPPED (the
        // PlutusV1/V2 `DCert` has no deposit field). Byte-exact with
        // cardano-ledger `Conway/TxInfo::transTxCertV1V2`:
        //   RegDepositTxCert  cred _deposit -> DCertDelegRegKey   (StakingHash cred)  [Constr 0]
        //   UnRegDepositTxCert cred _refund -> DCertDelegDeRegKey (StakingHash cred)  [Constr 1]
        // A V1/V2 script witnessing one of these is VALID on-chain — rejecting
        // it halted the node at mainnet epoch 511 (slot 135634801, the first
        // Conway stake-registration-with-deposit witnessed by a cert-purpose
        // V1/V2 script). The remaining Conway-only certs below have NO legacy
        // `DCert` form, so they correctly stay `CertificateNotSupported`.
        PrimCert::ConwayStakeRegistration { credential, .. } => {
            Data::Constr(0, vec![staking_hash_data(credential)])
        }
        PrimCert::ConwayStakeDeregistration { credential, .. } => {
            Data::Constr(1, vec![staking_hash_data(credential)])
        }
        PrimCert::RegStakeDeleg { .. } => return conway_only("RegStakeDeleg"),
        PrimCert::VoteDelegation { .. } => return conway_only("VoteDelegation"),
        PrimCert::StakeVoteDelegation { .. } => return conway_only("StakeVoteDelegation"),
        PrimCert::RegStakeVoteDeleg { .. } => return conway_only("RegStakeVoteDeleg"),
        PrimCert::VoteRegDeleg { .. } => return conway_only("VoteRegDeleg"),
        PrimCert::RegDRep { .. } => return conway_only("RegDRep"),
        PrimCert::UpdateDRep { .. } => return conway_only("UpdateDRep"),
        PrimCert::UnregDRep { .. } => return conway_only("UnregDRep"),
        PrimCert::CommitteeHotAuth { .. } => return conway_only("CommitteeHotAuth"),
        PrimCert::CommitteeColdResign { .. } => return conway_only("CommitteeColdResign"),
    };
    Ok(TxCert(data))
}

/// V1/V2 batch translation — mirrors [`certificates_to_plutus`] but emits the
/// `DCert` schema for `txInfoDCert :: [DCert]`.
pub fn certificates_to_plutus_v1v2(certs: &[PrimCert]) -> Result<Vec<TxCert>, PhaseTwoError> {
    certs.iter().map(certificate_to_plutus_v1v2).collect()
}

/// Encode `Option<u64>` as `Constr 1 []` (None) / `Constr 0 [I n]`
/// (Some n). Matches Plutus' canonical Option encoding.
fn option_int(v: Option<u64>) -> Data {
    match v {
        None => Data::Constr(1, vec![]),
        Some(n) => Data::Constr(0, vec![Data::I(BigInt::from(n))]),
    }
}

/// Encode a Plutus `Delegatee::DelegStake(PubKeyHash)` — `Constr 0
/// [B pool_hash]`. Used by the stake-delegation cert variants.
fn delegatee_to_pool(pool_hash: &[u8; 28]) -> Data {
    Data::Constr(0, vec![Data::B(pool_hash.to_vec())])
}

/// Encode a [`PrimDRep`] as the Plutus V3 `DRep` Data shape (#815).
///
/// Haskell reference: `PlutusLedgerApi.V3.Contexts`:
/// ```text
/// data DRep
///   = DRep DRepCredential      -- Constr 0 [Credential]
///   | DRepAlwaysAbstain        -- Constr 1 []
///   | DRepAlwaysNoConfidence   -- Constr 2 []
/// ```
/// `DRepCredential` is `newtype ... deriving newtype ToData` over the
/// bare V2 `Credential` (`Constr 0 [B]` pubkey / `Constr 1 [B]` script) —
/// same shape as [`cred_data`], just wrapped one level deeper in the
/// `DRep` constructor.
///
/// `PrimDRep::KeyHash` stores the 28-byte credential hash zero-padded to
/// 32 bytes (`Hash28::to_hash32_padded` pads bytes `[28..32]`) — strip the
/// trailing 4 pad bytes back to the real 28-byte hash before encoding, or
/// the emitted `Data` would carry 4 extra zero bytes cardano-ledger never
/// produces.
fn drep_to_data(drep: &PrimDRep) -> Data {
    match drep {
        PrimDRep::KeyHash(h32) => Data::Constr(
            0,
            vec![Data::Constr(
                0,
                vec![Data::B(h32.as_bytes()[..28].to_vec())],
            )],
        ),
        PrimDRep::ScriptHash(h28) => {
            Data::Constr(0, vec![Data::Constr(1, vec![Data::B(h28.0.to_vec())])])
        }
        PrimDRep::Abstain => Data::Constr(1, vec![]),
        PrimDRep::NoConfidence => Data::Constr(2, vec![]),
    }
}

/// Encode a Plutus `Delegatee::DelegVote(DRep)` — `Constr 1 [drep]`.
fn delegatee_vote(drep: &PrimDRep) -> Data {
    Data::Constr(1, vec![drep_to_data(drep)])
}

/// Encode a Plutus `Delegatee::DelegStakeVote(PubKeyHash, DRep)` —
/// `Constr 2 [B pool_hash, drep]`.
fn delegatee_stake_vote(pool_hash: &[u8; 28], drep: &PrimDRep) -> Data {
    Data::Constr(2, vec![Data::B(pool_hash.to_vec()), drep_to_data(drep)])
}

/// Translate the tx body's `certificates: Vec<Certificate>` into
/// `Vec<TxCert>` preserving input order.
pub fn certificates_to_plutus(certs: &[PrimCert]) -> Result<Vec<TxCert>, PhaseTwoError> {
    certs.iter().map(certificate_to_plutus).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_context::Credential as PlCredential;
    use dugite_primitives::hash::Hash;
    use dugite_primitives::transaction::{Anchor, GovActionId};
    use dugite_primitives::value::Lovelace;

    /// #770: `cost_models_to_data` (the `ChangedParameters` guardrail `Data` a
    /// governance script sees) must include unknown-language entries (keys ≥ 4)
    /// after the typed langs, in ascending order — matching Haskell
    /// `toPlutusData . flattenCostModels`.
    #[test]
    fn cost_models_to_data_includes_unknown_lang() {
        let cm = CostModels {
            plutus_v3: Some(vec![100]),
            unknown_cost_models: [(4u8, vec![200i64])].into_iter().collect(),
            ..Default::default()
        };
        let data = cost_models_to_data(&cm);
        match data {
            Data::Map(entries) => {
                assert_eq!(entries.len(), 2, "V3 + one unknown lang");
                // Entry 0: (I 2, List [I 100]) — PlutusV3.
                assert_eq!(entries[0].0, Data::I(BigInt::from(2)));
                assert_eq!(entries[0].1, Data::List(vec![Data::I(BigInt::from(100))]));
                // Entry 1: (I 4, List [I 200]) — the unknown language.
                assert_eq!(entries[1].0, Data::I(BigInt::from(4)));
                assert_eq!(entries[1].1, Data::List(vec![Data::I(BigInt::from(200))]));
            }
            other => panic!("expected Data::Map, got {other:?}"),
        }
    }

    fn h28(b: u8) -> dugite_primitives::hash::Hash28 {
        Hash::<28>([b; 28])
    }

    fn h32(b: u8) -> dugite_primitives::hash::Hash<32> {
        Hash::<32>([b; 32])
    }

    fn key_cred(b: u8) -> PrimCred {
        PrimCred::VerificationKey(h28(b))
    }

    fn script_cred(b: u8) -> PrimCred {
        PrimCred::Script(h28(b))
    }

    // V1/V2 DCert encoding ──────────────────────────────────────

    #[test]
    fn v1v2_dcert_stake_delegation_uses_dcert_schema_not_v3_txcert() {
        // DCertDelegDelegate (StakingHash (ScriptCredential h)) poolKey =
        //   Constr 2 [ Constr 0 [Constr 1 [B cred]], B pool ]
        // (StakingHash-wrapped credential; BARE pool PubKeyHash) — NOT the
        // Conway V3 `Constr 2 [Credential, Delegatee]` shape.
        let cert = PrimCert::StakeDelegation {
            credential: script_cred(0xaa),
            pool_hash: h28(0xbb),
        };
        let d = certificate_to_plutus_v1v2(&cert).unwrap().0;
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    Data::Constr(0, vec![Data::Constr(1, vec![Data::B(vec![0xaa; 28])])]),
                    Data::B(vec![0xbb; 28]),
                ]
            )
        );
    }

    #[test]
    fn v1v2_dcert_stake_registration_wraps_in_staking_hash() {
        // DCertDelegRegKey (StakingHash (PubKeyCredential h)) =
        //   Constr 0 [ Constr 0 [Constr 0 [B cred]] ]
        let cert = PrimCert::StakeRegistration(key_cred(0x11));
        let d = certificate_to_plutus_v1v2(&cert).unwrap().0;
        assert_eq!(
            d,
            Data::Constr(
                0,
                vec![Data::Constr(
                    0,
                    vec![Data::Constr(0, vec![Data::B(vec![0x11; 28])])]
                )]
            )
        );
    }

    #[test]
    fn v1v2_dcert_conway_registration_with_deposit_drops_deposit() {
        // Conway RegDepositTxCert (registration WITH a deposit) translates to
        // the SAME legacy V1/V2 DCertDelegRegKey [Constr 0] as the no-deposit
        // Shelley form — the deposit is DROPPED. Byte-exact with cardano-ledger
        // Conway/TxInfo::transTxCertV1V2. Pins the mainnet epoch-511 halt fix
        // (tx 360b9a34…, stake_registration deposit=2_000_000 witnessed by a
        // cert-purpose V1/V2 script).
        let reg = PrimCert::ConwayStakeRegistration {
            credential: key_cred(0x11),
            deposit: Lovelace(2_000_000),
        };
        assert_eq!(
            certificate_to_plutus_v1v2(&reg).unwrap().0,
            Data::Constr(
                0,
                vec![Data::Constr(
                    0,
                    vec![Data::Constr(0, vec![Data::B(vec![0x11; 28])])]
                )]
            ),
        );
        // UnRegDepositTxCert → DCertDelegDeRegKey [Constr 1], refund dropped.
        let dereg = PrimCert::ConwayStakeDeregistration {
            credential: script_cred(0x22),
            refund: Lovelace(2_000_000),
        };
        assert_eq!(
            certificate_to_plutus_v1v2(&dereg).unwrap().0,
            Data::Constr(
                1,
                vec![Data::Constr(
                    0,
                    vec![Data::Constr(1, vec![Data::B(vec![0x22; 28])])]
                )]
            ),
        );
    }

    #[test]
    fn v1v2_certifying_purpose_has_no_index_and_one_field() {
        // V1/V2 `Certifying DCert` = Constr 3 [dcert] — exactly one field,
        // NO integer cert index (that is a V3-only addition).
        let cert = PrimCert::StakeDeregistration(script_cred(0x22));
        let tx_cert = certificate_to_plutus_v1v2(&cert).unwrap();
        let purpose = crate::script_context::ScriptPurpose::Certifying(0, tx_cert);
        let d = purpose.to_data();
        match d {
            Data::Constr(3, fields) => {
                assert_eq!(
                    fields.len(),
                    1,
                    "V1/V2 Certifying must have exactly 1 field"
                );
                // The single field is the DCertDelegDeRegKey shape.
                assert_eq!(
                    fields[0],
                    Data::Constr(
                        1,
                        vec![Data::Constr(
                            0,
                            vec![Data::Constr(1, vec![Data::B(vec![0x22; 28])])]
                        )]
                    )
                );
            }
            other => panic!("expected Constr 3, got {other:?}"),
        }
    }

    // Voter ─────────────────────────────────────────────────────

    #[test]
    fn voter_committee_round_trips() {
        let v = PrimVoter::ConstitutionalCommittee(key_cred(1));
        let pl = voter_to_plutus(&v);
        assert!(matches!(
            pl,
            PlVoter::CommitteeVoter(PlCredential::PubKey(h)) if h == [1u8; 28]
        ));
    }

    #[test]
    fn voter_drep_with_script_cred() {
        let v = PrimVoter::DRep(script_cred(2));
        let pl = voter_to_plutus(&v);
        assert!(matches!(
            pl,
            PlVoter::DrepVoter(PlCredential::Script(h)) if h == [2u8; 28]
        ));
    }

    #[test]
    fn voter_pool_unpads_to_28_bytes() {
        let mut bytes = [0u8; 32];
        bytes[..28].copy_from_slice(&[5u8; 28]);
        let v = PrimVoter::StakePool(Hash::<32>(bytes));
        let pl = voter_to_plutus(&v);
        assert!(matches!(pl, PlVoter::StakePoolVoter(h) if h == [5u8; 28]));
    }

    // Vote ──────────────────────────────────────────────────────

    #[test]
    fn vote_translates_directly() {
        assert!(matches!(vote_to_plutus(&PrimVote::No), PlVote::No));
        assert!(matches!(vote_to_plutus(&PrimVote::Yes), PlVote::Yes));
        assert!(matches!(
            vote_to_plutus(&PrimVote::Abstain),
            PlVote::Abstain
        ));
    }

    // GovActionId ───────────────────────────────────────────────

    #[test]
    fn gov_action_id_widens_index_to_u64() {
        let g = PrimGovActionId {
            transaction_id: h32(0xab),
            action_index: 17,
        };
        let pl = gov_action_id_to_plutus(&g);
        assert_eq!(pl.tx_id, [0xab; 32]);
        assert_eq!(pl.idx, 17u64);
    }

    // Voting procedures map ─────────────────────────────────────

    #[test]
    fn voting_procedures_collect_inner_vote_tuples() {
        let mut inner = BTreeMap::new();
        inner.insert(
            GovActionId {
                transaction_id: h32(0x10),
                action_index: 0,
            },
            PrimVotingProcedure {
                vote: PrimVote::Yes,
                anchor: None,
            },
        );
        inner.insert(
            GovActionId {
                transaction_id: h32(0x20),
                action_index: 1,
            },
            PrimVotingProcedure {
                vote: PrimVote::Abstain,
                anchor: None,
            },
        );
        let mut vp: BTreeMap<PrimVoter, BTreeMap<GovActionId, PrimVotingProcedure>> =
            BTreeMap::new();
        vp.insert(PrimVoter::DRep(key_cred(9)), inner);
        let pl = voting_procedures_to_plutus(&vp);
        assert_eq!(pl.len(), 1);
        let (voter, votes) = &pl[0];
        assert!(matches!(voter, PlVoter::DrepVoter(_)));
        assert_eq!(votes.len(), 2);
        // BTreeMap is sorted on (tx_id, idx) → 0x10 < 0x20.
        assert!(matches!(votes[0].1, PlVote::Yes));
        assert!(matches!(votes[1].1, PlVote::Abstain));
    }

    // Proposal procedure ─────────────────────────────────────────

    fn reward_addr_blob(hash: [u8; 28]) -> Vec<u8> {
        let mut v = vec![0xe0u8]; // mainnet reward, key-stake
        v.extend_from_slice(&hash);
        v
    }

    fn anchor() -> Anchor {
        Anchor {
            url: String::new(),
            data_hash: h32(0),
        }
    }

    #[test]
    fn proposal_to_plutus_encodes_deposit_and_return_addr() {
        use dugite_primitives::transaction::GovAction;
        let p = PrimProposal {
            deposit: Lovelace(100_000),
            return_addr: reward_addr_blob([0x42; 28]),
            gov_action: GovAction::InfoAction,
            anchor: anchor(),
        };
        let pl = proposal_to_plutus(&p).unwrap();
        let Data::Constr(0, fields) = pl.0 else {
            panic!("expected Constr 0 wrapper");
        };
        assert_eq!(fields.len(), 3);
        // deposit
        assert_eq!(fields[0], Data::I(BigInt::from(100_000)));
        // return_addr (Plutus Credential::PubKey([0x42; 28]) → Constr 0 [B ...])
        assert!(
            matches!(&fields[1], Data::Constr(0, inner) if inner == &vec![Data::B(vec![0x42; 28])])
        );
        // gov_action: InfoAction = Constr 6 []
        // Haskell: makeIsDataSchemaIndexed ''GovernanceAction [('InfoAction, 6)]
        assert_eq!(fields[2], Data::Constr(6, vec![]));
    }

    #[test]
    fn proposal_to_plutus_rejects_non_reward_return_addr() {
        use dugite_primitives::transaction::GovAction;
        let mut bad = vec![0x60u8]; // enterprise, not reward
        bad.extend([1u8; 28]);
        let p = PrimProposal {
            deposit: Lovelace(1),
            return_addr: bad,
            gov_action: GovAction::InfoAction,
            anchor: anchor(),
        };
        let err = proposal_to_plutus(&p).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // Certificates ──────────────────────────────────────────────

    #[test]
    fn cert_stake_registration_uses_constr_0_with_none_deposit() {
        let c = PrimCert::StakeRegistration(key_cred(1));
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 0);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1], Data::Constr(1, vec![])); // None
    }

    #[test]
    fn cert_conway_stake_registration_carries_deposit() {
        let c = PrimCert::ConwayStakeRegistration {
            credential: key_cred(1),
            deposit: Lovelace(2_000_000),
        };
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 0);
        assert_eq!(
            fields[1],
            Data::Constr(0, vec![Data::I(BigInt::from(2_000_000))])
        );
    }

    #[test]
    fn cert_pool_registration_emits_operator_and_vrf_bytes() {
        use dugite_primitives::transaction::PoolParams;
        let params = PoolParams {
            operator: h28(0xaa),
            vrf_keyhash: h32(0xbb),
            pledge: Lovelace(0),
            cost: Lovelace(0),
            margin: dugite_primitives::transaction::Rational {
                numerator: 0,
                denominator: 1,
            },
            reward_account: vec![],
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
        };
        let c = PrimCert::PoolRegistration(params);
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 7);
        assert_eq!(fields[0], Data::B(vec![0xaa; 28]));
        assert_eq!(fields[1], Data::B(vec![0xbb; 32]));
    }

    #[test]
    fn cert_pool_retirement_carries_epoch() {
        let c = PrimCert::PoolRetirement {
            pool_hash: h28(0xcc),
            epoch: 500,
        };
        let TxCert(Data::Constr(tag, fields)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 8);
        assert_eq!(fields[1], Data::I(BigInt::from(500)));
    }

    #[test]
    fn cert_committee_hot_auth_carries_both_credentials() {
        let c = PrimCert::CommitteeHotAuth {
            cold_credential: key_cred(1),
            hot_credential: script_cred(2),
        };
        let TxCert(Data::Constr(tag, _)) = certificate_to_plutus(&c).unwrap() else {
            panic!("expected Constr");
        };
        assert_eq!(tag, 9);
    }

    #[test]
    fn cert_genesis_key_delegation_errors() {
        let c = PrimCert::GenesisKeyDelegation {
            genesis_hash: h32(1),
            genesis_delegate_hash: h32(2),
            vrf_keyhash: h32(3),
        };
        let err = certificate_to_plutus(&c).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn cert_mir_errors() {
        let c = PrimCert::MoveInstantaneousRewards {
            source: dugite_primitives::transaction::MIRSource::Reserves,
            target: dugite_primitives::transaction::MIRTarget::OtherAccountingPot(0),
        };
        let err = certificate_to_plutus(&c).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    #[test]
    fn certificates_to_plutus_preserves_order_and_surfaces_first_error() {
        let ok = PrimCert::StakeRegistration(key_cred(1));
        let bad = PrimCert::GenesisKeyDelegation {
            genesis_hash: h32(0),
            genesis_delegate_hash: h32(0),
            vrf_keyhash: h32(0),
        };
        let err = certificates_to_plutus(&[ok, bad]).unwrap_err();
        assert!(matches!(err, PhaseTwoError::Internal(_)));
    }

    // Vote-delegation Delegatee encoding (#815) ──────────────────────────
    //
    // Haskell reference: `PlutusLedgerApi.V3.Contexts`:
    //   data Delegatee = DelegStake PubKeyHash | DelegVote DRep
    //                  | DelegStakeVote PubKeyHash DRep
    //   data DRep = DRep DRepCredential | DRepAlwaysAbstain | DRepAlwaysNoConfidence
    // `DRepCredential` newtype-derives ToData from the bare V2 `Credential`
    // (`Constr 0 [B]` pubkey / `Constr 1 [B]` script), so `DRep (KeyHash h)`
    // encodes as `Constr 0 [Constr 0 [B h]]`.

    fn drep_key(b: u8) -> dugite_primitives::transaction::DRep {
        dugite_primitives::transaction::DRep::KeyHash(h28(b).to_hash32_padded())
    }

    fn drep_script(b: u8) -> dugite_primitives::transaction::DRep {
        dugite_primitives::transaction::DRep::ScriptHash(h28(b))
    }

    #[test]
    fn drep_to_data_key_hash_strips_padding() {
        // The 4 trailing zero-pad bytes added by `to_hash32_padded` must NOT
        // leak into the emitted `B` — only the real 28-byte hash.
        let d = drep_to_data(&drep_key(0x11));
        assert_eq!(
            d,
            Data::Constr(0, vec![Data::Constr(0, vec![Data::B(vec![0x11; 28])])])
        );
    }

    #[test]
    fn drep_to_data_script_hash() {
        let d = drep_to_data(&drep_script(0x22));
        assert_eq!(
            d,
            Data::Constr(0, vec![Data::Constr(1, vec![Data::B(vec![0x22; 28])])])
        );
    }

    #[test]
    fn drep_to_data_abstain_and_no_confidence() {
        assert_eq!(
            drep_to_data(&dugite_primitives::transaction::DRep::Abstain),
            Data::Constr(1, vec![])
        );
        assert_eq!(
            drep_to_data(&dugite_primitives::transaction::DRep::NoConfidence),
            Data::Constr(2, vec![])
        );
    }

    #[test]
    fn cert_vote_delegation_encodes_delegvote_with_key_cred_drep() {
        // TxCertDelegStaking cred (DelegVote drep) = Constr 2 [cred, Constr 1 [drep]]
        let c = PrimCert::VoteDelegation {
            credential: key_cred(1),
            drep: drep_key(0x33),
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    cred_data(&key_cred(1)),
                    Data::Constr(
                        1,
                        vec![Data::Constr(
                            0,
                            vec![Data::Constr(0, vec![Data::B(vec![0x33; 28])])]
                        )]
                    ),
                ]
            )
        );
    }

    #[test]
    fn cert_vote_delegation_encodes_delegvote_with_script_cred_drep() {
        let c = PrimCert::VoteDelegation {
            credential: script_cred(1),
            drep: drep_script(0x44),
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    cred_data(&script_cred(1)),
                    Data::Constr(
                        1,
                        vec![Data::Constr(
                            0,
                            vec![Data::Constr(1, vec![Data::B(vec![0x44; 28])])]
                        )]
                    ),
                ]
            )
        );
    }

    #[test]
    fn cert_vote_delegation_encodes_delegvote_with_abstain() {
        let c = PrimCert::VoteDelegation {
            credential: key_cred(1),
            drep: dugite_primitives::transaction::DRep::Abstain,
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    cred_data(&key_cred(1)),
                    Data::Constr(1, vec![Data::Constr(1, vec![])]),
                ]
            )
        );
    }

    #[test]
    fn cert_vote_delegation_encodes_delegvote_with_no_confidence() {
        let c = PrimCert::VoteDelegation {
            credential: key_cred(1),
            drep: dugite_primitives::transaction::DRep::NoConfidence,
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    cred_data(&key_cred(1)),
                    Data::Constr(1, vec![Data::Constr(2, vec![])]),
                ]
            )
        );
    }

    #[test]
    fn cert_stake_vote_delegation_encodes_delegstakevote() {
        // TxCertDelegStaking cred (DelegStakeVote pool drep) =
        //   Constr 2 [cred, Constr 2 [B pool, drep]]
        let c = PrimCert::StakeVoteDelegation {
            credential: key_cred(1),
            pool_hash: h28(0x55),
            drep: drep_key(0x66),
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                2,
                vec![
                    cred_data(&key_cred(1)),
                    Data::Constr(
                        2,
                        vec![
                            Data::B(vec![0x55; 28]),
                            Data::Constr(0, vec![Data::Constr(0, vec![Data::B(vec![0x66; 28])])]),
                        ]
                    ),
                ]
            )
        );
    }

    #[test]
    fn cert_reg_stake_vote_deleg_encodes_delegstakevote_with_deposit() {
        // TxCertRegDeleg cred (DelegStakeVote pool drep) deposit =
        //   Constr 3 [cred, Constr 2 [B pool, drep], I deposit]
        let c = PrimCert::RegStakeVoteDeleg {
            credential: script_cred(1),
            pool_hash: h28(0x77),
            drep: drep_script(0x88),
            deposit: Lovelace(2_000_000),
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                3,
                vec![
                    cred_data(&script_cred(1)),
                    Data::Constr(
                        2,
                        vec![
                            Data::B(vec![0x77; 28]),
                            Data::Constr(0, vec![Data::Constr(1, vec![Data::B(vec![0x88; 28])])]),
                        ]
                    ),
                    Data::I(BigInt::from(2_000_000)),
                ]
            )
        );
    }

    #[test]
    fn cert_vote_reg_deleg_encodes_delegvote_with_deposit() {
        // TxCertRegDeleg cred (DelegVote drep) deposit =
        //   Constr 3 [cred, Constr 1 [drep], I deposit]
        let c = PrimCert::VoteRegDeleg {
            credential: key_cred(1),
            drep: dugite_primitives::transaction::DRep::Abstain,
            deposit: Lovelace(2_000_000),
        };
        let TxCert(d) = certificate_to_plutus(&c).unwrap();
        assert_eq!(
            d,
            Data::Constr(
                3,
                vec![
                    cred_data(&key_cred(1)),
                    Data::Constr(1, vec![Data::Constr(1, vec![])]),
                    Data::I(BigInt::from(2_000_000)),
                ]
            )
        );
    }

    // GovernanceAction encoding ─────────────────────────────────────────
    //
    // Cross-validated against Haskell:
    //   PlutusLedgerApi.V3.Contexts — `makeIsDataSchemaIndexed ''GovernanceAction`
    //   CardanoLedger Conway TxInfo — `transGovAction`
    //
    // Constr tags (confirmed):
    //   ParameterChange=0, HardForkInitiation=1, TreasuryWithdrawals=2,
    //   NoConfidence=3, UpdateCommittee=4, NewConstitution=5, InfoAction=6.

    #[test]
    fn gov_action_info_encodes_as_constr_6_empty() {
        // InfoAction = Constr 6 []
        // Haskell: makeIsDataSchemaIndexed ''GovernanceAction [('InfoAction, 6)]
        use dugite_primitives::transaction::GovAction;
        let d = gov_action_to_data(&GovAction::InfoAction).unwrap();
        assert_eq!(d, Data::Constr(6, vec![]));
    }

    #[test]
    fn gov_action_no_confidence_encodes_as_constr_3() {
        // NoConfidence (Nothing) = Constr 3 [Constr 1 []]
        // Haskell: makeIsDataSchemaIndexed ''GovernanceAction [('NoConfidence, 3)]
        // field: Maybe GovernanceActionId — Nothing = Constr 1 []
        use dugite_primitives::transaction::GovAction;
        let d = gov_action_to_data(&GovAction::NoConfidence {
            prev_action_id: None,
        })
        .unwrap();
        assert_eq!(d, Data::Constr(3, vec![Data::Constr(1, vec![])]));
    }

    #[test]
    fn gov_action_no_confidence_with_prev_id_encodes_correctly() {
        // NoConfidence (Just gaid) = Constr 3 [Constr 0 [Constr 0 [B txid32, I idx]]]
        // GovernanceActionId = Constr 0 [B txid32, I idx] (V3 bare-txid)
        // Maybe Just = Constr 0 [inner]
        use dugite_primitives::transaction::{GovAction, GovActionId};
        let gaid = GovActionId {
            transaction_id: h32(0xab),
            action_index: 3,
        };
        let d = gov_action_to_data(&GovAction::NoConfidence {
            prev_action_id: Some(gaid),
        })
        .unwrap();
        // outer: Constr 3 [Maybe]
        let Data::Constr(3, ref fields) = d else {
            panic!("NoConfidence must be Constr 3; got {d:?}");
        };
        assert_eq!(fields.len(), 1);
        // Maybe = Just → Constr 0 [gaid_data]
        let Data::Constr(0, ref just_fields) = fields[0] else {
            panic!("Just must be Constr 0; got {:?}", fields[0]);
        };
        assert_eq!(just_fields.len(), 1);
        // GovernanceActionId = Constr 0 [B txid32, I 3]
        let Data::Constr(0, ref gaid_fields) = just_fields[0] else {
            panic!("GovActionId must be Constr 0; got {:?}", just_fields[0]);
        };
        assert_eq!(gaid_fields.len(), 2);
        assert!(
            matches!(&gaid_fields[0], Data::B(b) if b.len() == 32 && b.iter().all(|&x| x == 0xab)),
            "txid must be bare B(32); got {:?}",
            gaid_fields[0]
        );
        assert_eq!(gaid_fields[1], Data::I(BigInt::from(3u64)));
    }

    #[test]
    fn gov_action_hard_fork_encodes_as_constr_1_with_protocol_version() {
        // HardForkInitiation = Constr 1 [Maybe GovActionId, ProtocolVersion]
        // ProtocolVersion = Constr 0 [I major, I minor]
        // Haskell: makeIsDataSchemaIndexed ''ProtocolVersion [('ProtocolVersion, 0)]
        use dugite_primitives::transaction::GovAction;
        let d = gov_action_to_data(&GovAction::HardForkInitiation {
            prev_action_id: None,
            protocol_version: (10, 0),
        })
        .unwrap();
        let Data::Constr(1, ref fields) = d else {
            panic!("HardForkInitiation must be Constr 1; got {d:?}");
        };
        assert_eq!(fields.len(), 2);
        // fields[0]: Maybe GovActionId = Nothing = Constr 1 []
        assert_eq!(fields[0], Data::Constr(1, vec![]));
        // fields[1]: ProtocolVersion = Constr 0 [I 10, I 0]
        assert_eq!(
            fields[1],
            Data::Constr(
                0,
                vec![Data::I(BigInt::from(10u64)), Data::I(BigInt::from(0u64))]
            )
        );
    }

    #[test]
    fn parameter_change_changed_parameters_is_cost_models_map() {
        // #761: mainnet tx 51f495aa proposes a PlutusV3 cost-model
        // ParameterChange. ChangedParameters MUST be
        // Data::Map [(I 18, Map[(I 2, List[I cost..])])] — NOT the old
        // `Constr 0 []` placeholder, which broke the guardrails script's
        // `unMapData` ("unMapData on non-Map Data").
        use dugite_primitives::transaction::{CostModels, GovAction, ProtocolParamUpdate};
        let v3 = vec![100788i64, 420, 1, 1, -900]; // includes a negative cost
        let ppu = ProtocolParamUpdate {
            cost_models: Some(CostModels {
                plutus_v1: None,
                plutus_v2: None,
                plutus_v3: Some(v3.clone()),
                plutus_v4: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let d = gov_action_to_data(&GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ppu),
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(0, ref fields) = d else {
            panic!("ParameterChange must be Constr 0; got {d:?}");
        };
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], Data::Constr(1, vec![]), "Nothing prev_action_id");
        let expected_v3 = Data::List(v3.iter().map(|x| Data::I(BigInt::from(*x))).collect());
        let expected_cm = Data::Map(vec![(Data::I(BigInt::from(2u64)), expected_v3)]);
        let expected_changed = Data::Map(vec![(Data::I(BigInt::from(18u64)), expected_cm)]);
        assert_eq!(
            fields[1], expected_changed,
            "ChangedParameters must be a Data::Map keyed by ppuTag (18 = costModels, lang 2 = V3)"
        );
        assert_eq!(fields[2], Data::Constr(1, vec![]), "Nothing policy_hash");
    }

    #[test]
    fn changed_parameters_int_rational_encoding_and_pputag_order() {
        // #761: int fields → I n; rational fields (a0 = ppuTag 9) → List[I num, I den]
        // (NOT Constr); entries sorted ascending by ppuTag.
        use dugite_primitives::transaction::{GovAction, ProtocolParamUpdate, Rational};
        use dugite_primitives::value::Lovelace;
        let ppu = ProtocolParamUpdate {
            min_fee_a: Some(44), // ppuTag 0
            a0: Some(Rational {
                numerator: 3,
                denominator: 10,
            }), // ppuTag 9
            gov_action_deposit: Some(Lovelace(100_000_000_000)), // ppuTag 30
            ..Default::default()
        };
        let d = gov_action_to_data(&GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ppu),
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(0, fields) = d else {
            panic!("ParameterChange must be Constr 0");
        };
        let Data::Map(entries) = &fields[1] else {
            panic!("ChangedParameters must be Map; got {:?}", fields[1]);
        };
        assert_eq!(entries.len(), 3, "only the 3 set fields appear");
        // Ascending ppuTag order: 0, 9, 30.
        assert_eq!(entries[0].0, Data::I(BigInt::from(0u64)));
        assert_eq!(entries[0].1, Data::I(BigInt::from(44u64)));
        assert_eq!(entries[1].0, Data::I(BigInt::from(9u64)));
        assert_eq!(
            entries[1].1,
            Data::List(vec![
                Data::I(BigInt::from(3u64)),
                Data::I(BigInt::from(10u64))
            ]),
            "a0 rational must be List[num,den], not Constr"
        );
        assert_eq!(entries[2].0, Data::I(BigInt::from(30u64)));
        assert_eq!(entries[2].1, Data::I(BigInt::from(100_000_000_000u64)));
    }

    #[test]
    fn changed_parameters_min_fee_ref_script_is_rational_list_at_tag_33() {
        // #766: minFeeRefScriptCostPerByte (ppuTag 33) is a NonNegativeInterval,
        // so ChangedParameters must emit List[I num, I den] (Haskell
        // `ToPlutusData Rational`), NOT a bare integer or Constr.
        use dugite_primitives::transaction::{GovAction, ProtocolParamUpdate, Rational};
        let ppu = ProtocolParamUpdate {
            min_fee_ref_script_cost_per_byte: Some(Rational {
                numerator: 44,
                denominator: 3,
            }),
            ..Default::default()
        };
        let d = gov_action_to_data(&GovAction::ParameterChange {
            prev_action_id: None,
            protocol_param_update: Box::new(ppu),
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(0, fields) = d else {
            panic!("ParameterChange must be Constr 0");
        };
        let Data::Map(entries) = &fields[1] else {
            panic!("ChangedParameters must be Map");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Data::I(BigInt::from(33u64)), "ppuTag 33");
        assert_eq!(
            entries[0].1,
            Data::List(vec![
                Data::I(BigInt::from(44u64)),
                Data::I(BigInt::from(3u64))
            ]),
            "min_fee_ref rational must be List[num,den] (full 44/3, not truncated)"
        );
    }

    #[test]
    fn gov_action_new_constitution_encodes_as_constr_5() {
        // NewConstitution = Constr 5 [Maybe GovActionId, Constitution]
        // Constitution = Constr 0 [Maybe ScriptHash]
        // Haskell: makeIsDataSchemaIndexed ''Constitution [('Constitution, 0)]
        use dugite_primitives::transaction::{Constitution, GovAction};
        let d = gov_action_to_data(&GovAction::NewConstitution {
            prev_action_id: None,
            constitution: Constitution {
                anchor: Anchor {
                    url: String::new(),
                    data_hash: h32(0),
                },
                script_hash: Some(h28(0xcc)),
            },
        })
        .unwrap();
        let Data::Constr(5, ref fields) = d else {
            panic!("NewConstitution must be Constr 5; got {d:?}");
        };
        assert_eq!(fields.len(), 2);
        // Constitution = Constr 0 [Just(B28)]
        assert_eq!(
            fields[1],
            Data::Constr(0, vec![Data::Constr(0, vec![Data::B(vec![0xcc; 28])])])
        );
    }

    #[test]
    fn gov_action_treasury_withdrawals_encodes_as_constr_2() {
        // TreasuryWithdrawals = Constr 2 [Map Credential Lovelace, Maybe ScriptHash]
        // Map key = stake Credential extracted from reward address
        use dugite_primitives::transaction::GovAction;
        use dugite_primitives::value::Lovelace;
        let mut withdrawals = std::collections::BTreeMap::new();
        // reward address: 0xe0 (mainnet key-stake) || [0x77; 28]
        let mut addr = vec![0xe0u8];
        addr.extend_from_slice(&[0x77u8; 28]);
        withdrawals.insert(addr, Lovelace(500_000));
        let d = gov_action_to_data(&GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(2, ref fields) = d else {
            panic!("TreasuryWithdrawals must be Constr 2; got {d:?}");
        };
        assert_eq!(fields.len(), 2);
        // Map: [(Credential::PubKey([0x77;28]), I 500_000)]
        let Data::Map(ref entries) = fields[0] else {
            panic!("field[0] must be Map; got {:?}", fields[0]);
        };
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0].0, Data::Constr(0, inner) if inner.len() == 1),
            "map key must be PubKeyCredential (Constr 0 [B28]); got {:?}",
            entries[0].0
        );
        assert_eq!(entries[0].1, Data::I(BigInt::from(500_000u64)));
        // Maybe ScriptHash = Nothing
        assert_eq!(fields[1], Data::Constr(1, vec![]));
    }

    #[test]
    fn gov_action_update_committee_encodes_as_constr_4() {
        // UpdateCommittee = Constr 4 [Maybe GovActionId, [ColdCred], Map ColdCred Epoch, Rational]
        // Rational = Constr 0 [I num, I den]
        // ColdCommitteeCredential = newtype deriving ToData from Credential
        use dugite_primitives::transaction::{GovAction, Rational};
        let d = gov_action_to_data(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![key_cred(0xaa)],
            members_to_add: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(script_cred(0xbb), 500u64);
                m
            },
            threshold: Rational {
                numerator: 2,
                denominator: 3,
            },
        })
        .unwrap();
        let Data::Constr(4, ref fields) = d else {
            panic!("UpdateCommittee must be Constr 4; got {d:?}");
        };
        assert_eq!(fields.len(), 4);
        // fields[1]: [ColdCredential] — 1 item (key_cred 0xaa) = Constr 0 [B28]
        let Data::List(ref remove_list) = fields[1] else {
            panic!("field[1] must be List; got {:?}", fields[1]);
        };
        assert_eq!(remove_list.len(), 1);
        assert!(
            matches!(&remove_list[0], Data::Constr(0, _)),
            "ColdCommitteeCredential must be PubKeyCredential (Constr 0); got {:?}",
            remove_list[0]
        );
        // fields[2]: Map — 1 entry: script_cred 0xbb → epoch 500
        let Data::Map(ref add_map) = fields[2] else {
            panic!("field[2] must be Map; got {:?}", fields[2]);
        };
        assert_eq!(add_map.len(), 1);
        assert!(
            matches!(&add_map[0].0, Data::Constr(1, _)),
            "ScriptCredential must be Constr 1; got {:?}",
            add_map[0].0
        );
        assert_eq!(add_map[0].1, Data::I(BigInt::from(500u64)));
        // fields[3]: Rational = Constr 0 [I 2, I 3]
        // Haskell: makeIsDataSchemaIndexed ''Rational [('Rational, 0)] in plutus-tx
        assert_eq!(
            fields[3],
            Data::Constr(
                0,
                vec![Data::I(BigInt::from(2u64)), Data::I(BigInt::from(3u64))]
            ),
            "Rational must be Constr 0 [I num, I den]"
        );
    }

    // GovernanceAction ledger-order (Script < Key) ─────────────────────
    //
    // The 3 V3 GovernanceAction map/list fields (TreasuryWithdrawals map,
    // UpdateCommittee members_to_add map, members_to_remove list) are built
    // in LEDGER `Credential` Ord (Script < Key) in Haskell
    // (`Conway.TxInfo.transGovAction`: `transMap = unsafeFromList . map f .
    // Map.toList` preserves the ledger Map's Credential Ord ScriptHashObj <
    // KeyHashObj; `members_to_remove = Set.toList`, Script < Key, deduped).
    // dugite's derived `Credential`/`BTreeMap` order is the OPPOSITE
    // (Key < Script), so each field is re-sorted by `cmp_ledger`. These are
    // V3-only, so use the LEDGER comparator (NOT the V1/V2 Plutus Key < Script).

    /// Build a reward-account blob: header high-nibble `0xE_` (key-stake) /
    /// `0xF_` (script-stake), low-nibble = network (mainnet = 1), then a
    /// 28-byte credential hash. Mirrors
    /// `tx_info_populate::tests::encode_reward_addr_blob`.
    fn reward_addr_blob_typed(is_script: bool, hash: [u8; 28]) -> Vec<u8> {
        let header = if is_script { 0xf1u8 } else { 0xe1u8 };
        let mut v = Vec::with_capacity(29);
        v.push(header);
        v.extend_from_slice(&hash);
        v
    }

    #[test]
    fn gov_action_treasury_withdrawals_orders_script_before_key() {
        // Two reward blobs: a key-stake (0xE1..) and a script-stake (0xF1..).
        // The key hash is LOWER (0x01 < 0x02) so blob order AND hash order
        // both place the key entry first — only the ledger Script < Key rule
        // flips it. `BTreeMap` iterates the key blob first; the Plutus
        // `Map Credential Lovelace` must list the SCRIPT credential first.
        use dugite_primitives::transaction::GovAction;
        use dugite_primitives::value::Lovelace;
        let key_blob = reward_addr_blob_typed(false, [0x01u8; 28]);
        let script_blob = reward_addr_blob_typed(true, [0x02u8; 28]);
        let mut withdrawals = std::collections::BTreeMap::new();
        withdrawals.insert(key_blob.clone(), Lovelace(10));
        withdrawals.insert(script_blob, Lovelace(20));

        // Sanity: BTreeMap blob order iterates the KEY entry first.
        assert_eq!(
            withdrawals.keys().next().unwrap(),
            &key_blob,
            "blob order must be Key < Script"
        );

        let d = gov_action_to_data(&GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(2, ref fields) = d else {
            panic!("TreasuryWithdrawals must be Constr 2; got {d:?}");
        };
        let Data::Map(ref entries) = fields[0] else {
            panic!("field[0] must be Map; got {:?}", fields[0]);
        };
        assert_eq!(entries.len(), 2);
        // entries[0] = Script credential (Constr 1 [B28]) + amount 20.
        assert_eq!(
            entries[0].0,
            Data::Constr(1, vec![Data::B(vec![0x02u8; 28])]),
            "entries[0] must be the SCRIPT credential (ledger Script < Key); got {:?}",
            entries[0].0
        );
        assert_eq!(entries[0].1, Data::I(BigInt::from(20u64)));
        // entries[1] = Key credential (Constr 0 [B28]) + amount 10.
        assert_eq!(
            entries[1].0,
            Data::Constr(0, vec![Data::B(vec![0x01u8; 28])]),
            "entries[1] must be the KEY credential; got {:?}",
            entries[1].0
        );
        assert_eq!(entries[1].1, Data::I(BigInt::from(10u64)));
    }

    #[test]
    fn gov_action_treasury_withdrawals_single_entry_identity() {
        // Single entry — no over-sort regression; the sole credential stays put.
        use dugite_primitives::transaction::GovAction;
        use dugite_primitives::value::Lovelace;
        let mut withdrawals = std::collections::BTreeMap::new();
        withdrawals.insert(reward_addr_blob_typed(true, [0x55u8; 28]), Lovelace(7));
        let d = gov_action_to_data(&GovAction::TreasuryWithdrawals {
            withdrawals,
            policy_hash: None,
        })
        .unwrap();
        let Data::Constr(2, ref fields) = d else {
            panic!("TreasuryWithdrawals must be Constr 2; got {d:?}");
        };
        let Data::Map(ref entries) = fields[0] else {
            panic!("field[0] must be Map; got {:?}", fields[0]);
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].0,
            Data::Constr(1, vec![Data::B(vec![0x55u8; 28])])
        );
        assert_eq!(entries[0].1, Data::I(BigInt::from(7u64)));
    }

    #[test]
    fn gov_action_update_committee_add_orders_script_before_key() {
        // members_to_add {key 0x01, script 0x02}: dugite's derived
        // `BTreeMap<Credential, _>` iterates Key < Script; the Plutus
        // `Map ColdCredential Epoch` must list the SCRIPT credential first.
        use dugite_primitives::transaction::{GovAction, Rational};
        let mut members_to_add = std::collections::BTreeMap::new();
        members_to_add.insert(key_cred(0x01), 100u64);
        members_to_add.insert(script_cred(0x02), 200u64);

        // Sanity: BTreeMap iterates the KEY credential first (derived Key < Script).
        assert!(matches!(
            members_to_add.keys().next().unwrap(),
            PrimCred::VerificationKey(_)
        ));

        let d = gov_action_to_data(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add,
            threshold: Rational {
                numerator: 1,
                denominator: 2,
            },
        })
        .unwrap();
        let Data::Constr(4, ref fields) = d else {
            panic!("UpdateCommittee must be Constr 4; got {d:?}");
        };
        let Data::Map(ref add_map) = fields[2] else {
            panic!("field[2] must be Map; got {:?}", fields[2]);
        };
        assert_eq!(add_map.len(), 2);
        // add[0] = Script credential (Constr 1) + epoch 200.
        assert_eq!(
            add_map[0].0,
            Data::Constr(1, vec![Data::B(vec![0x02u8; 28])]),
            "add[0] must be the SCRIPT credential (ledger Script < Key); got {:?}",
            add_map[0].0
        );
        assert_eq!(add_map[0].1, Data::I(BigInt::from(200u64)));
        // add[1] = Key credential (Constr 0) + epoch 100.
        assert_eq!(
            add_map[1].0,
            Data::Constr(0, vec![Data::B(vec![0x01u8; 28])]),
            "add[1] must be the KEY credential; got {:?}",
            add_map[1].0
        );
        assert_eq!(add_map[1].1, Data::I(BigInt::from(100u64)));
    }

    #[test]
    fn gov_action_update_committee_add_single_entry_identity() {
        // Single add entry — no over-sort regression.
        use dugite_primitives::transaction::{GovAction, Rational};
        let mut members_to_add = std::collections::BTreeMap::new();
        members_to_add.insert(script_cred(0x66), 300u64);
        let d = gov_action_to_data(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![],
            members_to_add,
            threshold: Rational {
                numerator: 1,
                denominator: 2,
            },
        })
        .unwrap();
        let Data::Constr(4, ref fields) = d else {
            panic!("UpdateCommittee must be Constr 4; got {d:?}");
        };
        let Data::Map(ref add_map) = fields[2] else {
            panic!("field[2] must be Map; got {:?}", fields[2]);
        };
        assert_eq!(add_map.len(), 1);
        assert_eq!(
            add_map[0].0,
            Data::Constr(1, vec![Data::B(vec![0x66u8; 28])])
        );
        assert_eq!(add_map[0].1, Data::I(BigInt::from(300u64)));
    }

    #[test]
    fn gov_action_update_committee_remove_orders_script_before_key() {
        // members_to_remove vec![key, script] in KEY-first input order; the
        // Plutus `[ColdCredential]` list must be re-sorted to ledger order
        // (Script < Key) ⇒ [Script (Constr 1), Key (Constr 0)].
        use dugite_primitives::transaction::{GovAction, Rational};
        let d = gov_action_to_data(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![key_cred(0x01), script_cred(0x02)],
            members_to_add: std::collections::BTreeMap::new(),
            threshold: Rational {
                numerator: 1,
                denominator: 2,
            },
        })
        .unwrap();
        let Data::Constr(4, ref fields) = d else {
            panic!("UpdateCommittee must be Constr 4; got {d:?}");
        };
        let Data::List(ref remove_list) = fields[1] else {
            panic!("field[1] must be List; got {:?}", fields[1]);
        };
        assert_eq!(remove_list.len(), 2);
        // remove[0] = Script credential (Constr 1).
        assert_eq!(
            remove_list[0],
            Data::Constr(1, vec![Data::B(vec![0x02u8; 28])]),
            "remove[0] must be the SCRIPT credential (ledger Script < Key); got {:?}",
            remove_list[0]
        );
        // remove[1] = Key credential (Constr 0).
        assert_eq!(
            remove_list[1],
            Data::Constr(0, vec![Data::B(vec![0x01u8; 28])]),
            "remove[1] must be the KEY credential; got {:?}",
            remove_list[1]
        );
    }

    #[test]
    fn gov_action_update_committee_remove_single_entry_identity() {
        // Single remove entry — no over-sort regression.
        use dugite_primitives::transaction::{GovAction, Rational};
        let d = gov_action_to_data(&GovAction::UpdateCommittee {
            prev_action_id: None,
            members_to_remove: vec![key_cred(0x77)],
            members_to_add: std::collections::BTreeMap::new(),
            threshold: Rational {
                numerator: 1,
                denominator: 2,
            },
        })
        .unwrap();
        let Data::Constr(4, ref fields) = d else {
            panic!("UpdateCommittee must be Constr 4; got {d:?}");
        };
        let Data::List(ref remove_list) = fields[1] else {
            panic!("field[1] must be List; got {:?}", fields[1]);
        };
        assert_eq!(remove_list.len(), 1);
        assert_eq!(
            remove_list[0],
            Data::Constr(0, vec![Data::B(vec![0x77u8; 28])])
        );
    }

    // txInfoVotes ordering ──────────────────────────────────────

    /// `txInfoVotes` (`Map Voter …`) must list voters in the LEDGER `Map Voter`
    /// order. For two same-variant (DRep) voters, the ledger orders
    /// Script < Key, whereas dugite's derived `Voter`/`Credential` Ord orders
    /// Key < Script. `voting_procedures_to_plutus` re-sorts to ledger order, so
    /// the SCRIPT DRep must appear FIRST even though the input `BTreeMap` lists
    /// the key DRep first.
    #[test]
    fn voting_procedures_to_plutus_orders_script_voter_before_key_voter() {
        use crate::script_context::Voter as PlVoter;
        use dugite_primitives::transaction::{Vote as PrimVote, Voter as PrimVoter};

        let mk_inner = |b: u8| {
            let mut inner: BTreeMap<GovActionId, PrimVotingProcedure> = BTreeMap::new();
            inner.insert(
                GovActionId {
                    transaction_id: h32(b),
                    action_index: 0,
                },
                PrimVotingProcedure {
                    vote: PrimVote::Yes,
                    anchor: None,
                },
            );
            inner
        };

        let mut vp: BTreeMap<PrimVoter, BTreeMap<GovActionId, PrimVotingProcedure>> =
            BTreeMap::new();
        // Key DRep (derived order would place this first).
        vp.insert(PrimVoter::DRep(key_cred(0x01)), mk_inner(1));
        // Script DRep (ledger order places this first).
        vp.insert(PrimVoter::DRep(script_cred(0x99)), mk_inner(2));

        // Sanity: the input BTreeMap iterates the KEY voter first (derived Ord).
        assert!(matches!(
            vp.keys().next().unwrap(),
            PrimVoter::DRep(PrimCred::VerificationKey(_))
        ));

        let out = voting_procedures_to_plutus(&vp);
        assert_eq!(out.len(), 2);
        assert!(
            matches!(out[0].0, PlVoter::DrepVoter(PlCredential::Script(h)) if h == [0x99u8; 28]),
            "txInfoVotes[0] must be the SCRIPT DRep (ledger Script<Key); got {:?}",
            out[0].0
        );
        assert!(
            matches!(out[1].0, PlVoter::DrepVoter(PlCredential::PubKey(h)) if h == [0x01u8; 28]),
            "txInfoVotes[1] must be the KEY DRep; got {:?}",
            out[1].0
        );
    }
}
