//! ScriptContext data shapes for Plutus V1 / V2 / V3.
//!
//! The script context is the value of type `Data` that the Cardano
//! ledger passes as the last argument to every Plutus validator. Its
//! shape changes between Plutus versions:
//!
//!   - **V1** (`PlutusV1.Contexts.ScriptContext`):
//!     `ScriptContext { scriptContextTxInfo, scriptContextPurpose }`
//!     where `TxInfo` carries Alonzo-era tx fields (no reference
//!     inputs, no inline datums, no Conway-era features).
//!
//!   - **V2** (`PlutusV2.Contexts.ScriptContext`):
//!     Same shape, expanded `TxInfo` with reference inputs, inline
//!     datums, and reference scripts (CIP-31 / CIP-32 / CIP-33).
//!
//!   - **V3** (`PlutusV3.Contexts.ScriptContext`):
//!     `ScriptContext { scriptContextTxInfo, scriptContextRedeemer,
//!     scriptContextScriptInfo }`. `TxInfo` further expanded with
//!     governance (votes, proposal procedures, treasury fields).
//!     `ScriptInfo` replaces the old `ScriptPurpose` with richer
//!     per-purpose payloads.
//!
//! This module currently defines the *type-level* representation
//! (Rust structs / enums) for V3 — the most recent shape and the one
//! mainnet validators now most commonly target. V1 / V2 use the
//! exact same `TxInfo` field set with reduced shapes (missing the
//! Conway-only entries set to defaults) — they will land in a
//! follow-on commit alongside the per-version `to_data()` encoder.
//!
//! The `to_data()` encoder (this module's main external contract)
//! converts the Rust shape into a `crate::data::Data` value that
//! the CEK machine can pass to the validator term as its argument.

use crate::data::Data;
use num_bigint::BigInt;

/// Plutus version being targeted by the script context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    PlutusV1,
    PlutusV2,
    PlutusV3,
}

/// 28-byte hash used pervasively as a script credential / pool key
/// hash / DRep hash / etc. (`PlutusV3.Common.ScriptHash`).
pub type ScriptHash = [u8; 28];

/// 28-byte hash for stake/payment key credentials
/// (`PlutusV3.V1.PubKeyHash`).
pub type PubKeyHash = [u8; 28];

/// 32-byte transaction id (`PlutusV3.V1.TxId`).
pub type TxId = [u8; 32];

/// Pointer to a transaction output: `(tx_id, index)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOutRef {
    pub tx_id: TxId,
    pub idx: u64,
}

/// A payment / stake key credential — either a public-key hash or
/// a script hash (`PlutusV3.V1.Credential`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    PubKey(PubKeyHash),
    Script(ScriptHash),
}

/// Address = payment credential + optional staking credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub payment: Credential,
    pub staking: Option<StakingCredential>,
}

/// Staking credential — either an inline `Credential` or a pointer
/// to one (Byron pointer addresses still exist on mainnet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StakingCredential {
    Hash(Credential),
    Pointer { slot: u64, tx: u64, cert: u64 },
}

/// Multi-asset value: ada + native tokens. Stored as a sorted
/// `[(policy, [(asset_name, amount)])]` list — matches the on-chain
/// One `(asset_name, amount)` entry under a single policy.
pub type AssetEntry = (Vec<u8>, BigInt);

/// `Value` representation byte-for-byte once serialised to Data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlutusValue {
    pub policies: Vec<(ScriptHash, Vec<AssetEntry>)>,
}

/// Datum carried by a transaction output (V2+). Either an inline
/// `Data` value, a datum hash, or absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputDatum {
    None,
    Hash([u8; 32]),
    Inline(Data),
}

/// Transaction output (`PlutusV3.V1.TxOut`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOut {
    pub address: Address,
    pub value: PlutusValue,
    pub datum: OutputDatum,
    /// Optional reference-script hash (CIP-33).
    pub reference_script: Option<ScriptHash>,
}

/// Transaction input — `TxOutRef` paired with the resolved `TxOut`
/// from the UTxO set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInInfo {
    pub out_ref: TxOutRef,
    pub resolved: TxOut,
}

/// Bound on a tx validity interval. Conway/V3 uses closed bounds
/// on the lower end and open on the upper, matching the
/// `Interval POSIXTime` formulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosixTimeRange {
    pub lower: Option<i64>,
    pub upper: Option<i64>,
}

/// V1 TxInfo. Mirrors `PlutusV1.Contexts.TxInfo` — the original
/// Alonzo-era shape. No reference inputs, no inline datums, no
/// governance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInfoV1 {
    pub inputs: Vec<TxInInfo>,
    pub outputs: Vec<TxOut>,
    pub fee: BigInt,
    pub mint: PlutusValue,
    pub dcert: Vec<TxCert>,
    pub wdrl: Vec<(StakingCredential, BigInt)>,
    pub valid_range: PosixTimeRange,
    pub signatories: Vec<PubKeyHash>,
    pub data: Vec<([u8; 32], Data)>,
    pub txid: TxId,
}

/// V1 ScriptContext = `(TxInfo, ScriptPurpose)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptContextV1 {
    pub tx_info: TxInfoV1,
    pub purpose: ScriptPurpose,
}

/// V2 TxInfo. Mirrors `PlutusV2.Contexts.TxInfo`. Adds reference
/// inputs, inline datums and reference scripts on outputs, and
/// redeemers map (per CIP-31/32/33). No governance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInfoV2 {
    pub inputs: Vec<TxInInfo>,
    pub reference_inputs: Vec<TxInInfo>,
    pub outputs: Vec<TxOut>,
    pub fee: BigInt,
    pub mint: PlutusValue,
    pub dcert: Vec<TxCert>,
    pub wdrl: Vec<(StakingCredential, BigInt)>,
    pub valid_range: PosixTimeRange,
    pub signatories: Vec<PubKeyHash>,
    pub redeemers: Vec<(ScriptPurpose, Data)>,
    pub data: Vec<([u8; 32], Data)>,
    pub txid: TxId,
}

/// V2 ScriptContext = `(TxInfo, ScriptPurpose)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptContextV2 {
    pub tx_info: TxInfoV2,
    pub purpose: ScriptPurpose,
}

/// V3 TxInfo. Fields mirror
/// `PlutusV3.Contexts.TxInfo` in the Haskell reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInfoV3 {
    pub inputs: Vec<TxInInfo>,
    pub reference_inputs: Vec<TxInInfo>,
    pub outputs: Vec<TxOut>,
    pub fee: BigInt,
    pub mint: PlutusValue,
    pub valid_range: PosixTimeRange,
    pub signatories: Vec<PubKeyHash>,
    pub redeemers: Vec<(ScriptPurpose, Data)>,
    pub datums: Vec<([u8; 32], Data)>,
    pub txid: TxId,
    // Conway-era fields — fully present in V3.
    pub votes: Vec<(Voter, Vec<(GovActionId, Vote)>)>,
    pub proposal_procedures: Vec<ProposalProcedure>,
    pub current_treasury: Option<BigInt>,
    pub treasury_donation: Option<BigInt>,
}

/// `ScriptPurpose` used in TxInfo.redeemers map (a flat, Data-encoded
/// shape; the rich `ScriptInfo` is used at the outer ScriptContext
/// level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptPurpose {
    Minting(ScriptHash),
    Spending(TxOutRef),
    Rewarding(Credential),
    Certifying(u64, TxCert),
    Voting(Voter),
    Proposing(u64, ProposalProcedure),
    /// Dijkstra (PV12+) — `DijkstraGuarding(ScriptHash)`. The payload is
    /// the hash of the script credential being guarded (matches
    /// `Cardano.Ledger.Dijkstra.Scripts.DijkstraGuarding`, `Sum 6`).
    /// Issue #475 Phase 3.5.
    Guarding(ScriptHash),
}

/// V3 `ScriptInfo` — the per-purpose payload at the top of the
/// `ScriptContext`. `Spending` carries the optional inline datum
/// directly (V1/V2 used a separate `Datum` argument).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptInfo {
    Minting(ScriptHash),
    Spending {
        out_ref: TxOutRef,
        datum: Option<Data>,
    },
    Rewarding(Credential),
    Certifying(u64, TxCert),
    Voting(Voter),
    Proposing(u64, ProposalProcedure),
    /// Dijkstra (PV12+) — `DijkstraGuarding(ScriptHash)`. Issue #475
    /// Phase 3.5.
    Guarding(ScriptHash),
}

/// Certificates — opaque to the validator beyond their Data encoding.
/// Refined in a follow-on commit; for now we wrap raw Data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxCert(pub Data);

/// Governance voter (DRep / SPO / CC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Voter {
    CommitteeVoter(Credential),
    DrepVoter(Credential),
    StakePoolVoter(PubKeyHash),
}

/// Vote: yes / no / abstain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    No,
    Yes,
    Abstain,
}

/// Governance action id: `(tx_id, gov_action_idx)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovActionId {
    pub tx_id: TxId,
    pub idx: u64,
}

/// Proposal procedure — opaque to the validator beyond Data encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalProcedure(pub Data);

/// Top-level V3 ScriptContext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptContextV3 {
    pub tx_info: TxInfoV3,
    pub redeemer: Data,
    pub script_info: ScriptInfo,
}

// ───────────────────────────────────────────────────────────────────────────
// Data encoding
// ───────────────────────────────────────────────────────────────────────────

/// Encode a 28-byte hash as a `Data::B`.
fn data_bs28(b: &[u8; 28]) -> Data {
    Data::B(b.to_vec())
}

/// Encode a 32-byte hash as a `Data::B`.
fn data_bs32(b: &[u8; 32]) -> Data {
    Data::B(b.to_vec())
}

/// Encode a BigInt as `Data::I`.
fn data_i(n: BigInt) -> Data {
    Data::I(n)
}

fn data_list(items: Vec<Data>) -> Data {
    Data::List(items)
}

fn data_map(entries: Vec<(Data, Data)>) -> Data {
    Data::Map(entries)
}

fn data_constr(tag: u64, fields: Vec<Data>) -> Data {
    Data::Constr(tag, fields)
}

/// Encode a 32-byte hash as `TxId = Constr 0 [B bytes32]`.
///
/// The Haskell `TxId` newtype is serialised with
/// `makeIsDataSchemaIndexed ''TxId [('TxId, 0)]`, so scripts
/// navigate it with `unConstrData` then read the inner bytes.
fn data_txid(b: &[u8; 32]) -> Data {
    data_constr(0, vec![data_bs32(b)])
}

/// Encode an ADA-only fee as a Plutus `Value`.
///
/// `txInfoFee :: Value` — schema: `Map [(B[], Map [(B[], I lovelace)])]`
/// Empty bytestring = adaSymbol; empty bytestring = adaToken.
fn data_ada_value(lovelace: BigInt) -> Data {
    data_map(vec![(
        Data::B(Vec::new()),
        data_map(vec![(Data::B(Vec::new()), data_i(lovelace))]),
    )])
}

impl Credential {
    pub fn to_data(&self) -> Data {
        match self {
            Credential::PubKey(h) => data_constr(0, vec![data_bs28(h)]),
            Credential::Script(h) => data_constr(1, vec![data_bs28(h)]),
        }
    }
}

impl StakingCredential {
    pub fn to_data(&self) -> Data {
        match self {
            StakingCredential::Hash(c) => data_constr(0, vec![c.to_data()]),
            StakingCredential::Pointer { slot, tx, cert } => data_constr(
                1,
                vec![
                    data_i((*slot).into()),
                    data_i((*tx).into()),
                    data_i((*cert).into()),
                ],
            ),
        }
    }
}

impl Address {
    pub fn to_data(&self) -> Data {
        let staking = match &self.staking {
            None => data_constr(1, vec![]),               // Nothing
            Some(s) => data_constr(0, vec![s.to_data()]), // Just
        };
        data_constr(0, vec![self.payment.to_data(), staking])
    }
}

impl PlutusValue {
    pub fn to_data(&self) -> Data {
        // Plutus Value = Map CurrencySymbol (Map TokenName Integer).
        //
        // Critical: the CurrencySymbol (PolicyId) for Ada is the EMPTY
        // bytestring b"", NOT 28 zero bytes.  Haskell defines:
        //   adaSymbol = CurrencySymbol mempty  -- CurrencySymbol ""
        // Scripts that inspect value by policy look up b"" for Ada; if
        // we emit the 28-zero-byte key they silently find nothing.
        //
        // For native-token policies, the CurrencySymbol IS the 28-byte
        // policy-id script hash (so `data_bs28` is correct for those).
        data_map(
            self.policies
                .iter()
                .map(|(policy, assets)| {
                    let entries: Vec<(Data, Data)> = assets
                        .iter()
                        .map(|(name, amt)| (Data::B(name.clone()), data_i(amt.clone())))
                        .collect();
                    // ADA policy ([0u8;28]) is a sentinel value in `value_to_plutus`.
                    // Emit it as the EMPTY bytestring (b"") to match the Haskell
                    // `adaSymbol = CurrencySymbol mempty` convention.
                    let key = if policy == &[0u8; 28] {
                        Data::B(Vec::new())
                    } else {
                        data_bs28(policy)
                    };
                    (key, data_map(entries))
                })
                .collect(),
        )
    }
}

impl TxOutRef {
    pub fn to_data(&self) -> Data {
        // Schema: `TxOutRef = Constr 0 [TxId, Integer]`
        //         `TxId     = Constr 0 [B bytes32]`      (newtype wrapper)
        //
        // The TxId is NOT bare bytes — it is itself a Constr 0 [B bytes32].
        // Haskell: `makeIsDataSchemaIndexed ''TxId [('TxId, 0)]`
        // Scripts navigate it via `unConstrData` before reading the inner bytes.
        let tx_id_data = data_constr(0, vec![data_bs32(&self.tx_id)]);
        data_constr(0, vec![tx_id_data, data_i(self.idx.into())])
    }
}

impl OutputDatum {
    pub fn to_data(&self) -> Data {
        match self {
            OutputDatum::None => data_constr(0, vec![]),
            OutputDatum::Hash(h) => data_constr(1, vec![data_bs32(h)]),
            OutputDatum::Inline(d) => data_constr(2, vec![d.clone()]),
        }
    }
}

impl TxOut {
    /// V1 TxOut schema: `Constr 0 [Address, Value, Maybe DatumHash]`
    ///
    /// V1 does not have inline datums or reference scripts.
    /// `datum` field for V1: `OutputDatum::None` → `Nothing = Constr 1 []`,
    ///                       `OutputDatum::Hash(h)` → `Just h = Constr 0 [B32]`.
    /// `OutputDatum::Inline` should not appear in V1 — treated as None.
    pub fn to_data_v1(&self) -> Data {
        let datum_hash = match &self.datum {
            OutputDatum::None => data_constr(1, vec![]), // Nothing
            OutputDatum::Hash(h) => data_constr(0, vec![data_bs32(h)]), // Just (DatumHash h)
            OutputDatum::Inline(_) => data_constr(1, vec![]), // V1 sees no inline datums
        };
        data_constr(
            0,
            vec![self.address.to_data(), self.value.to_data(), datum_hash],
        )
    }

    /// V2/V3 TxOut schema: `Constr 0 [Address, Value, OutputDatum, Maybe ScriptHash]`
    ///
    /// `OutputDatum`: None=Constr0[], Hash=Constr1[B32], Inline=Constr2[datum].
    /// Reference-script: Nothing=Constr1[], Just h=Constr0[B28].
    pub fn to_data(&self) -> Data {
        let ref_script = match &self.reference_script {
            None => data_constr(1, vec![]),
            Some(h) => data_constr(0, vec![data_bs28(h)]),
        };
        data_constr(
            0,
            vec![
                self.address.to_data(),
                self.value.to_data(),
                self.datum.to_data(),
                ref_script,
            ],
        )
    }
}

impl TxInInfo {
    /// V1 encoding: `Constr 0 [TxOutRef, TxOut_v1]`.
    pub fn to_data_v1(&self) -> Data {
        data_constr(0, vec![self.out_ref.to_data(), self.resolved.to_data_v1()])
    }

    /// V2/V3 encoding: `Constr 0 [TxOutRef, TxOut_v2]`.
    pub fn to_data(&self) -> Data {
        data_constr(0, vec![self.out_ref.to_data(), self.resolved.to_data()])
    }
}

impl PosixTimeRange {
    pub fn to_data(&self) -> Data {
        // Plutus `Interval POSIXTime` is encoded as:
        //   Interval (LowerBound (Extended POSIXTime) Bool)
        //            (UpperBound (Extended POSIXTime) Bool)
        //
        // PlutusTx Data encoding of each constructor:
        //
        //   Extended:
        //     NegInf      = Constr 0 []
        //     Finite t    = Constr 1 [I(t)]       -- ONE field, just the value
        //     PosInf      = Constr 2 []
        //
        //   Bool:
        //     False       = Constr 0 []
        //     True        = Constr 1 []
        //
        //   LowerBound ext closed = Constr 0 [ext.to_data(), bool.to_data()]
        //   UpperBound ext closed = Constr 0 [ext.to_data(), bool.to_data()]
        //   Interval   lb  ub    = Constr 0 [lb.to_data(), ub.to_data()]
        //
        // Translating cardano-ledger's `transValidityInterval`:
        //   validity_start = None     => LowerBound NegInf True
        //   validity_start = Some(ms) => LowerBound (Finite ms) True  -- closed lower
        //   ttl = None                => UpperBound PosInf True
        //   ttl = Some(ms)            => UpperBound (Finite ms) False -- open upper
        //
        // Reference:
        //   IntersectMBO/cardano-ledger: eras/alonzo/impl/.../TxInfo.hs
        //   `transValidityInterval` / `transVITime`

        let data_false = data_constr(0, vec![]); // Bool False = Constr 0 []
        let data_true = data_constr(1, vec![]); // Bool True  = Constr 1 []

        // lower bound: None => NegInf, closed (True); Some => Finite ms, closed (True)
        let lower_ext = match self.lower {
            None => data_constr(0, vec![]), // NegInf = Constr 0 []
            Some(t) => data_constr(1, vec![data_i(t.into())]), // Finite t = Constr 1 [I(t)]
        };
        let lower_bound = data_constr(0, vec![lower_ext, data_true.clone()]);

        // upper bound: None => PosInf, closed (True); Some => Finite ms, open (False)
        let upper_ext = match self.upper {
            None => data_constr(2, vec![]), // PosInf = Constr 2 []
            Some(t) => data_constr(1, vec![data_i(t.into())]), // Finite t = Constr 1 [I(t)]
        };
        let upper_closed = if self.upper.is_none() {
            data_true // PosInf bound is closed (True)
        } else {
            data_false // Finite upper bound is open (False) — half-open interval [a, b)
        };
        let upper_bound = data_constr(0, vec![upper_ext, upper_closed]);

        data_constr(0, vec![lower_bound, upper_bound])
    }
}

impl Vote {
    pub fn to_data(&self) -> Data {
        match self {
            Vote::No => data_constr(0, vec![]),
            Vote::Yes => data_constr(1, vec![]),
            Vote::Abstain => data_constr(2, vec![]),
        }
    }
}

impl Voter {
    pub fn to_data(&self) -> Data {
        match self {
            Voter::CommitteeVoter(c) => data_constr(0, vec![c.to_data()]),
            Voter::DrepVoter(c) => data_constr(1, vec![c.to_data()]),
            Voter::StakePoolVoter(h) => data_constr(2, vec![data_bs28(h)]),
        }
    }
}

impl GovActionId {
    pub fn to_data(&self) -> Data {
        // V3 schema: `GovActionId = Constr 0 [TxId, Integer]`
        // `TxId = Constr 0 [B bytes32]` — same wrapping as TxOutRef.
        let tx_id_data = data_constr(0, vec![data_bs32(&self.tx_id)]);
        data_constr(0, vec![tx_id_data, data_i(self.idx.into())])
    }
}

impl ScriptPurpose {
    pub fn to_data(&self) -> Data {
        match self {
            ScriptPurpose::Minting(h) => data_constr(0, vec![data_bs28(h)]),
            ScriptPurpose::Spending(r) => data_constr(1, vec![r.to_data()]),
            ScriptPurpose::Rewarding(c) => data_constr(2, vec![c.to_data()]),
            ScriptPurpose::Certifying(i, c) => {
                data_constr(3, vec![data_i((*i).into()), c.0.clone()])
            }
            ScriptPurpose::Voting(v) => data_constr(4, vec![v.to_data()]),
            ScriptPurpose::Proposing(i, p) => {
                data_constr(5, vec![data_i((*i).into()), p.0.clone()])
            }
            // Dijkstra `DijkstraGuarding(ScriptHash)` — Sum 6.
            // Issue #475 Phase 3.5.
            ScriptPurpose::Guarding(h) => data_constr(6, vec![data_bs28(h)]),
        }
    }
}

impl ScriptInfo {
    pub fn to_data(&self) -> Data {
        match self {
            ScriptInfo::Minting(h) => data_constr(0, vec![data_bs28(h)]),
            ScriptInfo::Spending { out_ref, datum } => {
                let dat = match datum {
                    None => data_constr(1, vec![]),
                    Some(d) => data_constr(0, vec![d.clone()]),
                };
                data_constr(1, vec![out_ref.to_data(), dat])
            }
            ScriptInfo::Rewarding(c) => data_constr(2, vec![c.to_data()]),
            ScriptInfo::Certifying(i, c) => data_constr(3, vec![data_i((*i).into()), c.0.clone()]),
            ScriptInfo::Voting(v) => data_constr(4, vec![v.to_data()]),
            ScriptInfo::Proposing(i, p) => data_constr(5, vec![data_i((*i).into()), p.0.clone()]),
            // Dijkstra `DijkstraGuarding(ScriptHash)` — Sum 6. The
            // populator emits this for any Plutus V3 / V4 script invoked
            // through a credential-based guard at TxBody key 14.
            // Issue #475 Phase 3.5.
            ScriptInfo::Guarding(h) => data_constr(6, vec![data_bs28(h)]),
        }
    }
}

impl TxInfoV3 {
    pub fn to_data(&self) -> Data {
        // Field order matches `PlutusV3.Contexts.TxInfo`.
        data_constr(
            0,
            vec![
                data_list(self.inputs.iter().map(TxInInfo::to_data).collect()),
                data_list(
                    self.reference_inputs
                        .iter()
                        .map(TxInInfo::to_data)
                        .collect(),
                ),
                data_list(self.outputs.iter().map(TxOut::to_data).collect()),
                // V3 `txInfoFee :: Lovelace` is a newtype over Integer with a
                // newtype-derived ToData — it serialises as a BARE `I lovelace`,
                // NOT a Value map (unlike V1/V2 `txInfoFee :: Value`). Adversarial
                // review caught the previous attempt wrongly applying the
                // V1/V2 ada-Value encoding here.
                data_i(self.fee.clone()),
                self.mint.to_data(),
                data_list(self.signatories.iter().map(data_bs28).collect()),
                data_map(
                    self.redeemers
                        .iter()
                        .map(|(p, d)| (p.to_data(), d.clone()))
                        .collect(),
                ),
                data_map(
                    self.datums
                        .iter()
                        .map(|(h, d)| (data_bs32(h), d.clone()))
                        .collect(),
                ),
                // txInfoId :: TxId = Constr 0 [B bytes32]
                data_constr(0, vec![data_bs32(&self.txid)]),
                data_list(
                    self.votes
                        .iter()
                        .map(|(voter, votes)| {
                            data_constr(
                                0,
                                vec![
                                    voter.to_data(),
                                    data_map(
                                        votes
                                            .iter()
                                            .map(|(gid, v)| (gid.to_data(), v.to_data()))
                                            .collect(),
                                    ),
                                ],
                            )
                        })
                        .collect(),
                ),
                data_list(
                    self.proposal_procedures
                        .iter()
                        .map(|p| p.0.clone())
                        .collect(),
                ),
                match &self.current_treasury {
                    None => data_constr(1, vec![]),
                    Some(t) => data_constr(0, vec![data_i(t.clone())]),
                },
                match &self.treasury_donation {
                    None => data_constr(1, vec![]),
                    Some(t) => data_constr(0, vec![data_i(t.clone())]),
                },
                self.valid_range.to_data(),
            ],
        )
    }
}

impl ScriptContextV3 {
    pub fn to_data(&self) -> Data {
        data_constr(
            0,
            vec![
                self.tx_info.to_data(),
                self.redeemer.clone(),
                self.script_info.to_data(),
            ],
        )
    }
}

impl TxInfoV1 {
    pub fn to_data(&self) -> Data {
        // V1 TxInfo (Alonzo-era) field order — 10 fields, Constr 0:
        //   [inputs, outputs, fee, mint, dcert, wdrl, validRange,
        //    signatories, data, id]
        //
        // Key V1 vs V2 differences:
        //   - inputs/outputs use V1 TxOut shape (3 fields, no ref_script)
        //   - fee :: Value  (NOT Integer) = Map[(b"", Map[(b"", I lovelace)])]
        //   - wdrl :: [(StakingCredential, Integer)]  = List[Constr 0 [cred, amt]]
        //     (V1 uses AssocList not AssocMap; schema: `[(StakingCredential, Integer)]`)
        //   - data :: [(DatumHash, Datum)] = List[Constr 0 [B32, datum]]
        //   - id :: TxId = Constr 0 [B bytes32]
        data_constr(
            0,
            vec![
                data_list(self.inputs.iter().map(TxInInfo::to_data_v1).collect()),
                data_list(self.outputs.iter().map(TxOut::to_data_v1).collect()),
                // fee :: Value — ADA-only map, NOT bare Integer
                data_ada_value(self.fee.clone()),
                self.mint.to_data(),
                data_list(self.dcert.iter().map(|c| c.0.clone()).collect()),
                // V1 wdrl :: [(StakingCredential, Integer)] — AssocList, not Map.
                // Encoded as List[Constr 0 [cred.to_data(), I(amt)]].
                // Reference: PlutusLedgerApi/V1/Contexts.hs TxInfo definition.
                data_list(
                    self.wdrl
                        .iter()
                        .map(|(cred, amt)| {
                            data_constr(0, vec![cred.to_data(), data_i(amt.clone())])
                        })
                        .collect(),
                ),
                self.valid_range.to_data(),
                data_list(self.signatories.iter().map(data_bs28).collect()),
                // V1 data :: [(DatumHash, Datum)] — AssocList, not Map.
                // Encoded as List[Constr 0 [B32(hash), datum]].
                data_list(
                    self.data
                        .iter()
                        .map(|(h, d)| data_constr(0, vec![data_bs32(h), d.clone()]))
                        .collect(),
                ),
                // id :: TxId = Constr 0 [B bytes32]
                data_txid(&self.txid),
            ],
        )
    }
}

impl ScriptContextV1 {
    pub fn to_data(&self) -> Data {
        data_constr(0, vec![self.tx_info.to_data(), self.purpose.to_data()])
    }
}

impl TxInfoV2 {
    pub fn to_data(&self) -> Data {
        // V2 TxInfo (Babbage-era) field order — 12 fields, Constr 0:
        //   [inputs, refInputs, outputs, fee, mint, dcert, wdrl,
        //    validRange, signatories, redeemers, data, id]
        //
        // Key V2 differences from V1:
        //   - Has reference_inputs
        //   - fee :: Value (same as V1: Map, not bare Integer)
        //   - wdrl :: Map StakingCredential Integer (AssocMap → Data::Map)
        //   - data :: Map DatumHash Datum (AssocMap → Data::Map)
        //   - Has redeemers Map
        //   - id :: TxId = Constr 0 [B bytes32]
        data_constr(
            0,
            vec![
                data_list(self.inputs.iter().map(TxInInfo::to_data).collect()),
                data_list(
                    self.reference_inputs
                        .iter()
                        .map(TxInInfo::to_data)
                        .collect(),
                ),
                data_list(self.outputs.iter().map(TxOut::to_data).collect()),
                // fee :: Value — ADA-only map, NOT bare Integer
                data_ada_value(self.fee.clone()),
                self.mint.to_data(),
                data_list(self.dcert.iter().map(|c| c.0.clone()).collect()),
                // V2 wdrl :: Map StakingCredential Integer — Data::Map.
                // Reference: PlutusLedgerApi/V2/Contexts.hs TxInfo.txInfoWdrl.
                data_map(
                    self.wdrl
                        .iter()
                        .map(|(cred, amt)| (cred.to_data(), data_i(amt.clone())))
                        .collect(),
                ),
                self.valid_range.to_data(),
                data_list(self.signatories.iter().map(data_bs28).collect()),
                // V2 redeemers :: Map ScriptPurpose Redeemer — Data::Map.
                data_map(
                    self.redeemers
                        .iter()
                        .map(|(p, d)| (p.to_data(), d.clone()))
                        .collect(),
                ),
                // V2 data :: Map DatumHash Datum — Data::Map.
                data_map(
                    self.data
                        .iter()
                        .map(|(h, d)| (data_bs32(h), d.clone()))
                        .collect(),
                ),
                // id :: TxId = Constr 0 [B bytes32]
                data_txid(&self.txid),
            ],
        )
    }
}

impl ScriptContextV2 {
    pub fn to_data(&self) -> Data {
        data_constr(0, vec![self.tx_info.to_data(), self.purpose.to_data()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(b: u8) -> Credential {
        Credential::PubKey([b; 28])
    }

    #[test]
    fn credential_pubkey_encodes_as_constr_0() {
        let c = pk(0xaa);
        let d = c.to_data();
        if let Data::Constr(tag, fields) = d {
            assert_eq!(tag, 0);
            assert_eq!(fields.len(), 1);
            assert!(matches!(&fields[0], Data::B(b) if b.len() == 28));
        } else {
            panic!("expected Constr");
        }
    }

    #[test]
    fn credential_script_encodes_as_constr_1() {
        let c = Credential::Script([0xbb; 28]);
        let d = c.to_data();
        assert!(matches!(d, Data::Constr(1, _)));
    }

    #[test]
    fn address_encodes_as_pair_payment_staking() {
        let a = Address {
            payment: pk(1),
            staking: None,
        };
        let d = a.to_data();
        if let Data::Constr(0, fields) = d {
            assert_eq!(fields.len(), 2);
        } else {
            panic!("expected Constr 0");
        }
    }

    #[test]
    fn tx_out_ref_encodes_as_constr_0_with_txid_wrapper_and_idx() {
        // TxOutRef = Constr 0 [TxId, Integer]
        // TxId     = Constr 0 [B bytes32]   (newtype — NOT bare bytes)
        let r = TxOutRef {
            tx_id: [7u8; 32],
            idx: 3,
        };
        let d = r.to_data();
        let Data::Constr(0, ref fields) = d else {
            panic!("TxOutRef must be Constr 0; got {d:?}");
        };
        assert_eq!(
            fields.len(),
            2,
            "TxOutRef must have 2 fields (TxId, Integer)"
        );
        // fields[0] must be TxId = Constr 0 [B bytes32]
        let Data::Constr(0, ref id_fields) = fields[0] else {
            panic!("TxId must be Constr 0; got {:?}", fields[0]);
        };
        assert_eq!(id_fields.len(), 1, "TxId has 1 inner field (the raw bytes)");
        assert!(
            matches!(&id_fields[0], Data::B(b) if b.len() == 32 && b.iter().all(|&x| x == 7)),
            "TxId inner bytes must be the 32 hash bytes; got {:?}",
            id_fields[0]
        );
        // fields[1] must be the index
        assert!(
            matches!(&fields[1], Data::I(i) if i == &BigInt::from(3)),
            "TxOutRef idx field must be I(3); got {:?}",
            fields[1]
        );
    }

    #[test]
    fn empty_tx_info_v3_round_trips_through_cbor() {
        let info = TxInfoV3 {
            inputs: vec![],
            reference_inputs: vec![],
            outputs: vec![],
            fee: BigInt::from(0),
            mint: PlutusValue::default(),
            valid_range: PosixTimeRange {
                lower: None,
                upper: None,
            },
            signatories: vec![],
            redeemers: vec![],
            datums: vec![],
            txid: [0u8; 32],
            votes: vec![],
            proposal_procedures: vec![],
            current_treasury: None,
            treasury_donation: None,
        };
        let d = info.to_data();
        // CBOR round-trip
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn vote_constructors_have_expected_tags() {
        assert!(matches!(Vote::No.to_data(), Data::Constr(0, _)));
        assert!(matches!(Vote::Yes.to_data(), Data::Constr(1, _)));
        assert!(matches!(Vote::Abstain.to_data(), Data::Constr(2, _)));
    }

    #[test]
    fn empty_tx_info_v1_round_trips_through_cbor() {
        let info = TxInfoV1 {
            inputs: vec![],
            outputs: vec![],
            fee: BigInt::from(0),
            mint: PlutusValue::default(),
            dcert: vec![],
            wdrl: vec![],
            valid_range: PosixTimeRange {
                lower: None,
                upper: None,
            },
            signatories: vec![],
            data: vec![],
            txid: [0u8; 32],
        };
        let d = info.to_data();
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn empty_tx_info_v2_round_trips_through_cbor() {
        let info = TxInfoV2 {
            inputs: vec![],
            reference_inputs: vec![],
            outputs: vec![],
            fee: BigInt::from(0),
            mint: PlutusValue::default(),
            dcert: vec![],
            wdrl: vec![],
            valid_range: PosixTimeRange {
                lower: None,
                upper: None,
            },
            signatories: vec![],
            redeemers: vec![],
            data: vec![],
            txid: [0u8; 32],
        };
        let d = info.to_data();
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn script_context_v1_v2_v3_all_top_constr_zero() {
        let p = ScriptPurpose::Minting([0u8; 28]);
        let v1 = ScriptContextV1 {
            tx_info: TxInfoV1 {
                inputs: vec![],
                outputs: vec![],
                fee: BigInt::from(0),
                mint: PlutusValue::default(),
                dcert: vec![],
                wdrl: vec![],
                valid_range: PosixTimeRange {
                    lower: None,
                    upper: None,
                },
                signatories: vec![],
                data: vec![],
                txid: [0u8; 32],
            },
            purpose: p.clone(),
        };
        let v2 = ScriptContextV2 {
            tx_info: TxInfoV2 {
                inputs: vec![],
                reference_inputs: vec![],
                outputs: vec![],
                fee: BigInt::from(0),
                mint: PlutusValue::default(),
                dcert: vec![],
                wdrl: vec![],
                valid_range: PosixTimeRange {
                    lower: None,
                    upper: None,
                },
                signatories: vec![],
                redeemers: vec![],
                data: vec![],
                txid: [0u8; 32],
            },
            purpose: p,
        };
        assert!(matches!(v1.to_data(), Data::Constr(0, _)));
        assert!(matches!(v2.to_data(), Data::Constr(0, _)));
    }

    #[test]
    fn script_info_spending_includes_optional_datum() {
        let si = ScriptInfo::Spending {
            out_ref: TxOutRef {
                tx_id: [0u8; 32],
                idx: 0,
            },
            datum: Some(Data::I(BigInt::from(42))),
        };
        if let Data::Constr(1, fields) = si.to_data() {
            assert_eq!(fields.len(), 2);
            // datum slot: Constr 0 [Data::I(42)]
            if let Data::Constr(0, inner) = &fields[1] {
                assert!(matches!(&inner[0], Data::I(i) if i == &BigInt::from(42)));
            } else {
                panic!("expected Just-wrapped datum");
            }
        } else {
            panic!("expected ScriptInfo::Spending → Constr 1");
        }
    }
}
