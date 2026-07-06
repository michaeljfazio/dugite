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
use std::rc::Rc;

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
///
/// `tx_info` is `Rc`-shared (#838): the same `TxInfoV1` is reused across
/// every V1 redeemer in a transaction (it does not depend on which
/// redeemer is being evaluated), so sharing it here turns what would
/// otherwise be a per-redeemer deep clone into an O(1) refcount bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptContextV1 {
    pub tx_info: Rc<TxInfoV1>,
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
///
/// `tx_info` is `Rc`-shared — see `ScriptContextV1::tx_info`'s doc
/// comment (#838).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptContextV2 {
    pub tx_info: Rc<TxInfoV2>,
    pub purpose: ScriptPurpose,
}

/// V3 TxInfo. Fields mirror
/// `PlutusV3.Contexts.TxInfo` in the Haskell reference.
///
/// **Exact 16-field layout** (`Constr 0 [...]`):
///  0  inputs            List[TxInInfo]
///  1  reference_inputs  List[TxInInfo]
///  2  outputs           List[TxOut]
///  3  fee               I(lovelace)   ← bare Integer (V3 Lovelace newtype)
///  4  mint              Map[(B28, Map[(B name, I qty)])]  — no ada entry
///  5  certs             List[TxCert]
///  6  wdrl              Map[(Credential, I lovelace)]  ← V3: Credential directly, NOT StakingHash
///  7  valid_range       POSIXTimeRange
///  8  signatories       List[B28]
///  9  redeemers         Map[(ScriptPurpose, Redeemer)]
/// 10  datums            Map[(B32, Datum)]
/// 11  txid              B(32)  ← bare bytes (V3 TxId `deriving newtype ToData`)
/// 12  votes             Map[(Voter, Map[(GovActionId, Vote)])]
/// 13  proposal_procedures List[ProposalProcedure]
/// 14  current_treasury  Maybe Lovelace
/// 15  treasury_donation Maybe Lovelace
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxInfoV3 {
    pub inputs: Vec<TxInInfo>,
    pub reference_inputs: Vec<TxInInfo>,
    pub outputs: Vec<TxOut>,
    pub fee: BigInt,
    pub mint: PlutusValue,
    /// V3 `txInfoTxCerts` — field 5 in the 16-field layout.
    pub certs: Vec<TxCert>,
    /// V3 `txInfoWdrl` — Map keyed by `Credential` DIRECTLY, no StakingHash wrapper.
    /// Keys are in BTreeMap / canonical CBOR order.
    pub wdrl: Vec<(Credential, BigInt)>,
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
///
/// `tx_info` is `Rc`-shared — see `ScriptContextV1::tx_info`'s doc
/// comment (#838).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptContextV3 {
    pub tx_info: Rc<TxInfoV3>,
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

    /// V1/V2 `txInfoMint :: Value` encoding.
    ///
    /// Identical to [`to_data`](Self::to_data) but with the mandatory
    /// ada(0) entry `(b"", Map [(b"", I 0)])` prepended. cardano-ledger
    /// builds the mint Value as
    /// `transMintValue m = transCoinToValue zero <> transMultiAsset m`
    /// (`eras/alonzo/impl/.../Plutus/TxInfo.hs`) — the ada symbol is ALWAYS
    /// present with quantity 0 even though minting ada is impossible
    /// ("hysterical raisins" backward-compat: scripts that inspected the
    /// pre-Mary mint Value must keep seeing the ada key). The native-token
    /// policies follow in ascending policy-id order (matching
    /// `transMultiAsset`'s `Map.foldrWithKey'` over the sorted `Data.Map`).
    ///
    /// PlutusV3 `txInfoMint :: MintValue` OMITS this entry — use
    /// [`to_data`](Self::to_data) there.
    pub fn to_mint_data_v1v2(&self) -> Data {
        let ada_entry = (
            Data::B(Vec::new()),
            data_map(vec![(Data::B(Vec::new()), data_i(BigInt::from(0)))]),
        );
        let mut entries: Vec<(Data, Data)> = Vec::with_capacity(self.policies.len() + 1);
        entries.push(ada_entry);
        for (policy, assets) in &self.policies {
            let token_entries: Vec<(Data, Data)> = assets
                .iter()
                .map(|(name, amt)| (Data::B(name.clone()), data_i(amt.clone())))
                .collect();
            let key = if policy == &[0u8; 28] {
                Data::B(Vec::new())
            } else {
                data_bs28(policy)
            };
            entries.push((key, data_map(token_entries)));
        }
        data_map(entries)
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

    /// V3 encoding: `TxOutRef = Constr 0 [B bytes32, Integer]`.
    ///
    /// V3 changed `TxId` to `newtype TxId BuiltinByteString deriving newtype
    /// ToData`, so the txid is a BARE bytestring here, NOT the V1/V2
    /// `Constr 0 [B bytes32]` wrapper. Used by every V3 input/reference-input,
    /// the `Spending` redeemer purpose, and the `SpendingScript` script-info.
    pub fn to_data_v3(&self) -> Data {
        data_constr(0, vec![data_bs32(&self.tx_id), data_i(self.idx.into())])
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

    /// V2 encoding: `Constr 0 [TxOutRef, TxOut_v2]` (wrapped TxId).
    pub fn to_data(&self) -> Data {
        data_constr(0, vec![self.out_ref.to_data(), self.resolved.to_data()])
    }

    /// V3 encoding: `Constr 0 [TxOutRef_v3, TxOut_v2]`.
    ///
    /// Identical to the V2 shape except the embedded `TxOutRef` uses the V3
    /// bare-txid form (`Constr 0 [B32, I idx]`); the resolved `TxOut` is the
    /// same 4-field V2/V3 layout.
    pub fn to_data_v3(&self) -> Data {
        data_constr(0, vec![self.out_ref.to_data_v3(), self.resolved.to_data()])
    }
}

impl PosixTimeRange {
    pub fn to_data(&self, conway_or_later: bool) -> Data {
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
        // cardano-ledger `transValidityInterval` (verbatim — Alonzo
        // `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/TxInfo.hs`,
        // Conway `eras/conway/impl/src/Cardano/Ledger/Conway/TxInfo.hs`):
        //
        //   SNothing SNothing      => always
        //   (SJust i) SNothing     => PV1.from t  = LowerBound (Finite t) True
        //                                           UpperBound PosInf      True
        //   SNothing (SJust i)     => -- THE ERA-DEPENDENT CASE --
        //       Alonzo/Babbage (V1/V2): PV1.to t  = UpperBound (Finite t) True   (CLOSED)
        //       Conway (V3):  strictUpperBound t  = UpperBound (Finite t) False  (OPEN)
        //   (SJust i) (SJust j)    => lowerBound t1      = LowerBound (Finite t1) True
        //                             strictUpperBound t2 = UpperBound (Finite t2) False (OPEN, all eras)
        //
        // So the LOWER bound closure is ALWAYS True (NegInf, Finite, or PosInf).
        // The UPPER bound closure for a *finite* upper is True only in the
        // Alonzo/Babbage upper-only case; every other finite-upper case
        // (V3 upper-only, or both-bounds in any era) is False. PosInf upper
        // is always True. `to` (V1/V2) calls `upperBound s = UpperBound
        // (Finite s) True`; Conway/both-bounds calls `strictUpperBound s =
        // UpperBound (Finite s) False`.

        let data_false = data_constr(0, vec![]); // Bool False = Constr 0 []
        let data_true = data_constr(1, vec![]); // Bool True  = Constr 1 []

        // lower bound: None => NegInf, closed (True); Some => Finite ms, closed (True)
        let lower_ext = match self.lower {
            None => data_constr(0, vec![]), // NegInf = Constr 0 []
            Some(t) => data_constr(1, vec![data_i(t.into())]), // Finite t = Constr 1 [I(t)]
        };
        let lower_bound = data_constr(0, vec![lower_ext, data_true.clone()]);

        // upper bound: None => PosInf (closed/True); Some => Finite ms (closure
        // is era-dependent — see table above).
        let upper_ext = match self.upper {
            None => data_constr(2, vec![]), // PosInf = Constr 2 []
            Some(t) => data_constr(1, vec![data_i(t.into())]), // Finite t = Constr 1 [I(t)]
        };
        // Finite upper-bound closure is ERA-GATED (NOT language-gated):
        //
        //   * Conway+ (`conway_or_later`): a finite upper bound is ALWAYS
        //     exclusive (closure False). Conway's `transValidityInterval`
        //     (`eras/conway/impl/Cardano.Ledger.Conway.TxInfo`) builds BOTH the
        //     upper-only case and the both-bounds case with `strictUpperBound`.
        //   * Alonzo/Babbage (pre-Conway): the upper-ONLY case uses `PV1.to`
        //     (`eras/alonzo/impl/Cardano.Ledger.Alonzo.Plutus.TxInfo`), and
        //     `PlutusLedgerApi.V1.Interval.to s = Interval (LowerBound NegInf True)
        //     (upperBound s)` with `upperBound s = UpperBound (Finite s) True` —
        //     i.e. INCLUSIVE. The both-bounds case still uses `strictUpperBound`
        //     (exclusive). Conway deliberately unified these to exclusive.
        //
        // PosInf upper bound is always closed (True).
        //
        // #772: dugite previously gated this on LANGUAGE (V1/V2 ⇒ inclusive),
        // emitting an inclusive upper bound for a V1/V2 script in the Conway era.
        // That over-charged a validity-range-reading Reward redeemer by +1453 cpu
        // vs cardano-node (empirically localised by a leaf-diff of dugite's vs
        // cardano-ledger's constructed ScriptContext; the closure was the only
        // differing leaf). The gate is the ERA, not the script language version.
        let upper_closed = match self.upper {
            None => true,
            Some(_) => {
                if conway_or_later {
                    false
                } else {
                    self.lower.is_none()
                }
            }
        };
        let upper_bound = data_constr(
            0,
            vec![upper_ext, if upper_closed { data_true } else { data_false }],
        );

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
        // V3 schema: `GovActionId = Constr 0 [TxId, Integer]`.
        // V3 `TxId` is `newtype … deriving newtype ToData` → BARE `B bytes32`,
        // NOT the V1/V2 `Constr 0 [B bytes32]` wrapper.
        data_constr(0, vec![data_bs32(&self.tx_id), data_i(self.idx.into())])
    }
}

impl ScriptPurpose {
    /// Encode this `ScriptPurpose` as Plutus V1/V2 `Data`.
    ///
    /// Used for:
    /// - `scriptContextPurpose` in V1/V2 `ScriptContext`
    /// - `txInfoRedeemers` map keys in V2 `TxInfo`
    ///
    /// `Spending` uses the **wrapped** `TxId` form:
    /// `Constr 1 [Constr 0 [Constr 0 [B txid32], I idx]]`.
    ///
    /// Haskell: `makeIsDataSchemaIndexed ''ScriptPurpose`
    /// in `PlutusLedgerApi.V{1,2}.Contexts`:
    /// `[('Minting,0), ('Spending,1), ('Rewarding,2), ('Certifying,3)]`
    pub fn to_data(&self) -> Data {
        match self {
            ScriptPurpose::Minting(h) => data_constr(0, vec![data_bs28(h)]),
            ScriptPurpose::Spending(r) => data_constr(1, vec![r.to_data()]),
            // V1/V2 `Rewarding StakingCredential`: the credential must be wrapped
            // in `StakingHash` (Constr 0) — `Rewarding (StakingHash cred)` =
            // `Constr 2 [Constr 0 [Constr {0|1} [B28]]]`. Omitting the StakingHash
            // wrapper makes a deserializer read the inner `Credential`'s Constr-1
            // (ScriptCredential) tag as `StakingPtr` and `unIData` the 28-byte hash
            // → "unIData on non-I". (#22; PlutusLedgerApi.V{1,2}.Credential:
            // StakingHash=0/StakingPtr=1, PubKeyCredential=0/ScriptCredential=1.)
            ScriptPurpose::Rewarding(c) => data_constr(2, vec![data_constr(0, vec![c.to_data()])]),
            // V1/V2 `Certifying DCert` takes EXACTLY ONE field (the DCert) —
            // `Constr 3 [dcert]`. The integer cert-index is a V3-only addition
            // (`to_data_v3`). `c.0` here must already be the V1/V2 `DCert`-schema
            // Data (built via `certificate_to_plutus_v1v2`).
            ScriptPurpose::Certifying(_i, c) => data_constr(3, vec![c.0.clone()]),
            ScriptPurpose::Voting(v) => data_constr(4, vec![v.to_data()]),
            ScriptPurpose::Proposing(i, p) => {
                data_constr(5, vec![data_i((*i).into()), p.0.clone()])
            }
            // Dijkstra `DijkstraGuarding(ScriptHash)` — Sum 6.
            // Issue #475 Phase 3.5.
            ScriptPurpose::Guarding(h) => data_constr(6, vec![data_bs28(h)]),
        }
    }

    /// Encode this `ScriptPurpose` as Plutus **V3** `Data`.
    ///
    /// Used for `txInfoRedeemers` map keys in V3 `TxInfo`.
    ///
    /// The **only** difference from [`Self::to_data`] is that `Spending`
    /// uses the **bare** txid form introduced in V3:
    /// `Constr 1 [Constr 0 [B txid32, I idx]]`
    /// (NOT the double-wrapped V1/V2 `Constr 1 [Constr 0 [Constr 0 [B32], I]]`).
    ///
    /// All other variants are identical to the V1/V2 encoding — their
    /// Constr tags and field shapes are unchanged across versions.
    ///
    /// Haskell: `PlutusLedgerApi.V3.Contexts`:
    /// ```haskell
    /// makeIsDataSchemaIndexed ''ScriptPurpose
    ///   [('Minting,0), ('Spending,1), ('Rewarding,2), ('Certifying,3),
    ///    ('Voting,4), ('Proposing,5)]
    /// ```
    /// `Spending V3.TxOutRef` where `TxId` is `newtype … deriving newtype ToData`
    /// from `BuiltinByteString` → bare B(32) in the `TxOutRef` payload.
    pub fn to_data_v3(&self) -> Data {
        match self {
            // Spending: bare-txid TxOutRef (V3 form).
            // Constr 1 [Constr 0 [B txid32, I idx]]
            ScriptPurpose::Spending(r) => data_constr(1, vec![r.to_data_v3()]),
            // V3 `Rewarding Credential` takes the credential DIRECTLY (no
            // StakingHash wrapper — V3 dropped StakingCredential), so override the
            // V1/V2 `to_data` arm which now adds the wrapper. Constr 2 [Credential].
            ScriptPurpose::Rewarding(c) => data_constr(2, vec![c.to_data()]),
            // V3 `Certifying Integer TxCert` KEEPS the cert index (V1/V2 dropped
            // it). `c.0` is the V3 `TxCert`-schema Data here. Constr 3 [I i, TxCert].
            ScriptPurpose::Certifying(i, c) => {
                data_constr(3, vec![data_i((*i).into()), c.0.clone()])
            }
            // All other variants: same encoding in V1/V2/V3.
            other => other.to_data(),
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
                data_constr(1, vec![out_ref.to_data_v3(), dat])
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
        // 16-field layout per `PlutusV3.Contexts.TxInfo` (plutus 5cb073dca6 /
        // cardano-ledger d0e208885b). Fields are in EXACT Haskell order.
        //
        //  0  inputs
        //  1  reference_inputs
        //  2  outputs
        //  3  fee               (bare I, NOT ada-Value map — V3 fee :: Lovelace newtype)
        //  4  mint
        //  5  txInfoTxCerts     (List[TxCert])
        //  6  txInfoWdrl        (Map[(Credential, I)] — NO StakingHash wrapper)
        //  7  txInfoValidRange
        //  8  signatories
        //  9  redeemers
        // 10  datums
        // 11  txid              (bare B(32) — V3 TxId uses `deriving newtype ToData`
        //                        from BuiltinByteString, not makeIsDataIndexed)
        // 12  votes
        // 13  proposal_procedures
        // 14  current_treasury
        // 15  treasury_donation
        data_constr(
            0,
            vec![
                // 0 — inputs (V3 TxInInfo: bare-txid TxOutRef)
                data_list(self.inputs.iter().map(TxInInfo::to_data_v3).collect()),
                // 1 — reference_inputs
                data_list(
                    self.reference_inputs
                        .iter()
                        .map(TxInInfo::to_data_v3)
                        .collect(),
                ),
                // 2 — outputs
                data_list(self.outputs.iter().map(TxOut::to_data).collect()),
                // 3 — fee :: Lovelace = bare I(lovelace), NOT a Value map.
                // V3 diverges from V1/V2 which used `fee :: Value`.
                data_i(self.fee.clone()),
                // 4 — mint (no ada entry — V3 MintValue, not V1/V2 Value)
                self.mint.to_data(),
                // 5 — txInfoTxCerts :: [TxCert]
                data_list(self.certs.iter().map(|c| c.0.clone()).collect()),
                // 6 — txInfoWdrl :: Map Credential Lovelace
                // V3 key type is `Credential` directly (Constr 0/1 [B28]),
                // NOT wrapped in `StakingHash` as V1/V2 used.
                data_map(
                    self.wdrl
                        .iter()
                        .map(|(cred, amt)| (cred.to_data(), data_i(amt.clone())))
                        .collect(),
                ),
                // 7 — txInfoValidRange :: POSIXTimeRange (V3/Conway semantics:
                // finite upper bound is always strict/open)
                self.valid_range.to_data(true),
                // 8 — signatories :: [PubKeyHash]
                data_list(self.signatories.iter().map(data_bs28).collect()),
                // 9 — redeemers :: Map ScriptPurpose Redeemer
                // TODO(task-13f): redeemers map not yet populated — wired as
                // 9 — redeemers :: Map ScriptPurpose Redeemer
                // V3 map keys use `ScriptPurpose::to_data_v3()` so that the
                // `Spending` variant encodes with the bare-txid `TxOutRef`
                // form introduced in V3 (V1/V2 use `Constr 0 [B bytes]`
                // inside TxOutRef; V3 uses bare `B bytes`).
                // Reference: PlutusLedgerApi.V3.Contexts — `TxId` changed to
                // `newtype TxId … deriving newtype ToData` from `BuiltinByteString`.
                data_map(
                    self.redeemers
                        .iter()
                        .map(|(p, d)| (p.to_data_v3(), d.clone()))
                        .collect(),
                ),
                // 10 — datums :: Map DatumHash Datum
                data_map(
                    self.datums
                        .iter()
                        .map(|(h, d)| (data_bs32(h), d.clone()))
                        .collect(),
                ),
                // 11 — txid :: TxId = bare B(32).
                // In V3, `TxId = newtype TxId = TxId BuiltinByteString deriving newtype ToData`.
                // `deriving newtype ToData` delegates to `BuiltinByteString`'s `ToData` instance
                // which emits a BARE `B(bytes)` — NOT `Constr 0 [B bytes]`.
                // Contrast V1/V2 which used `makeIsDataIndexed ''TxId [('TxId, 0)]`
                // producing `Constr 0 [B bytes]`. V3 changed this deliberately.
                data_bs32(&self.txid),
                // 12 — votes :: Map Voter (Map GovActionId Vote)
                data_map(
                    self.votes
                        .iter()
                        .map(|(voter, votes)| {
                            (
                                voter.to_data(),
                                data_map(
                                    votes
                                        .iter()
                                        .map(|(gid, v)| (gid.to_data(), v.to_data()))
                                        .collect(),
                                ),
                            )
                        })
                        .collect(),
                ),
                // 13 — proposal_procedures :: [ProposalProcedure]
                data_list(
                    self.proposal_procedures
                        .iter()
                        .map(|p| p.0.clone())
                        .collect(),
                ),
                // 14 — current_treasury :: Maybe Lovelace (Just=0, Nothing=1)
                match &self.current_treasury {
                    None => data_constr(1, vec![]),
                    Some(t) => data_constr(0, vec![data_i(t.clone())]),
                },
                // 15 — treasury_donation :: Maybe Lovelace (Just=0, Nothing=1)
                match &self.treasury_donation {
                    None => data_constr(1, vec![]),
                    Some(t) => data_constr(0, vec![data_i(t.clone())]),
                },
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
    pub fn to_data(&self, conway_or_later: bool) -> Data {
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
                // mint :: Value — INCLUDES the ada(0) entry (transMintValue).
                self.mint.to_mint_data_v1v2(),
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
                // txInfoValidRange — closure is ERA-gated (#772): pre-Conway
                // ttl-only finite upper is CLOSED via `PV1.to`; Conway+ is OPEN
                // via `strictUpperBound`. See `PosixTimeRange::to_data`.
                self.valid_range.to_data(conway_or_later),
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
    pub fn to_data(&self, conway_or_later: bool) -> Data {
        data_constr(
            0,
            vec![
                self.tx_info.to_data(conway_or_later),
                self.purpose.to_data(),
            ],
        )
    }
}

impl TxInfoV2 {
    pub fn to_data(&self, conway_or_later: bool) -> Data {
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
                // mint :: Value — INCLUDES the ada(0) entry (transMintValue).
                self.mint.to_mint_data_v1v2(),
                data_list(self.dcert.iter().map(|c| c.0.clone()).collect()),
                // V2 wdrl :: Map StakingCredential Integer — Data::Map.
                // Reference: PlutusLedgerApi/V2/Contexts.hs TxInfo.txInfoWdrl.
                data_map(
                    self.wdrl
                        .iter()
                        .map(|(cred, amt)| (cred.to_data(), data_i(amt.clone())))
                        .collect(),
                ),
                // txInfoValidRange — closure is ERA-gated (#772): pre-Conway
                // ttl-only finite upper is CLOSED via `PV1.to`; Conway+ is OPEN
                // via `strictUpperBound`. See `PosixTimeRange::to_data`.
                self.valid_range.to_data(conway_or_later),
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
    pub fn to_data(&self, conway_or_later: bool) -> Data {
        data_constr(
            0,
            vec![
                self.tx_info.to_data(conway_or_later),
                self.purpose.to_data(),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(b: u8) -> Credential {
        Credential::PubKey([b; 28])
    }

    /// Extract the `(lower_closure, upper_closure)` Bool constructor tags
    /// from a `PosixTimeRange::to_data()` result: `Constr 0 [LowerBound,
    /// UpperBound]` where each bound is `Constr 0 [Extended, Bool]` and a
    /// `Bool` is `Constr 0 []` (False) / `Constr 1 []` (True).
    fn closures(d: &Data) -> (u64, u64) {
        let Data::Constr(0, bounds) = d else {
            panic!("interval is not Constr 0")
        };
        let tag = |b: &Data| -> u64 {
            let Data::Constr(0, parts) = b else {
                panic!("bound is not Constr 0")
            };
            let Data::Constr(t, _) = &parts[1] else {
                panic!("closure is not a Constr")
            };
            *t
        };
        (tag(&bounds[0]), tag(&bounds[1]))
    }

    #[test]
    fn valid_range_upper_closure_matches_cardano_ledger_per_era() {
        // cardano-ledger `transValidityInterval`:
        //   SNothing (SJust i)  [ttl only]:
        //     Alonzo/Babbage (V1/V2): PV1.to t        => UpperBound (Finite t) True  (CLOSED)
        //     Conway (V3):  strictUpperBound t          => UpperBound (Finite t) False (OPEN)
        //   (SJust i) (SJust j) [both bounds]:
        //     all eras:     strictUpperBound t2         => UpperBound (Finite t2) False (OPEN)
        // The lower-bound closure is ALWAYS True.

        // ttl-only, V1/V2 (v3_semantics = false): upper CLOSED (True = tag 1).
        let ttl_only = PosixTimeRange {
            lower: None,
            upper: Some(1_000),
        };
        assert_eq!(closures(&ttl_only.to_data(false)), (1, 1));

        // ttl-only, V3 (v3_semantics = true): upper OPEN (False = tag 0).
        assert_eq!(closures(&ttl_only.to_data(true)), (1, 0));

        // both bounds: upper OPEN in every era (strictUpperBound).
        let both = PosixTimeRange {
            lower: Some(500),
            upper: Some(1_000),
        };
        assert_eq!(closures(&both.to_data(false)), (1, 0));
        assert_eq!(closures(&both.to_data(true)), (1, 0));

        // PosInf upper (no ttl): always CLOSED (True), both eras.
        let lower_only = PosixTimeRange {
            lower: Some(500),
            upper: None,
        };
        assert_eq!(closures(&lower_only.to_data(false)), (1, 1));
        assert_eq!(closures(&lower_only.to_data(true)), (1, 1));
    }

    #[test]
    fn v1v2_mint_value_prepends_ada_zero_entry() {
        // A mint with one native policy. V1/V2 `txInfoMint :: Value` must
        // include the ada(0) entry (b"", Map[(b"", 0)]) FIRST, per
        // cardano-ledger `transMintValue m = transCoinToValue zero <>
        // transMultiAsset m`. V3 `MintValue` omits it.
        let v = PlutusValue {
            policies: vec![([0x5d; 28], vec![(b"tok".to_vec(), BigInt::from(-1))])],
        };
        // V1/V2: ada entry present and first.
        let m = v.to_mint_data_v1v2();
        let Ok(entries) = m.into_map() else {
            panic!("expected Map");
        };
        assert_eq!(entries.len(), 2, "ada + 1 native policy");
        // entry[0] = (b"", Map[(b"", I 0)])
        assert!(matches!(&entries[0].0, Data::B(b) if b.is_empty()));
        let Data::Map(ada_tokens) = &entries[0].1 else {
            panic!("ada inner must be a Map");
        };
        assert_eq!(ada_tokens.len(), 1);
        assert!(matches!(&ada_tokens[0].0, Data::B(b) if b.is_empty()));
        assert!(matches!(&ada_tokens[0].1, Data::I(n) if *n == BigInt::from(0)));
        // entry[1] = the native policy (28-byte key).
        assert!(matches!(&entries[1].0, Data::B(b) if b.len() == 28));

        // V3 (to_data): NO ada entry — just the native policy.
        let v3 = v.to_data();
        let Ok(v3_entries) = v3.into_map() else {
            panic!("expected Map");
        };
        assert_eq!(v3_entries.len(), 1, "V3 MintValue omits ada");
        assert!(matches!(&v3_entries[0].0, Data::B(b) if b.len() == 28));
    }

    #[test]
    fn credential_pubkey_encodes_as_constr_0() {
        let c = pk(0xaa);
        let d = c.to_data();
        if let Ok((tag, fields)) = d.into_constr() {
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
        if let Ok((0, fields)) = d.into_constr() {
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

    fn empty_tx_info_v3() -> TxInfoV3 {
        TxInfoV3 {
            inputs: vec![],
            reference_inputs: vec![],
            outputs: vec![],
            fee: BigInt::from(0),
            mint: PlutusValue::default(),
            certs: vec![],
            wdrl: vec![],
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
        }
    }

    #[test]
    fn empty_tx_info_v3_round_trips_through_cbor() {
        let info = empty_tx_info_v3();
        let d = info.to_data();
        // CBOR round-trip
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2);
    }

    // ────────────────────────────────────────────────────────────────────
    // V3 TxInfo structural conformance tests (task #13)
    // ────────────────────────────────────────────────────────────────────
    //
    // These tests verify the 16-field layout against expected values
    // derived INDEPENDENTLY from the schema in the task specification
    // (plutus 5cb073dca6 / cardano-ledger d0e208885b). They are NOT
    // derived from the implementation — each expected value is computed
    // by hand from the spec before writing the assertion.

    /// Core 16-field layout test. Builds a representative V3 tx with
    /// non-trivial data in every Conway-era field, then asserts each
    /// element of the outer Constr 0 list matches the expected type.
    #[test]
    fn tx_info_v3_to_data_emits_exactly_16_fields_in_correct_order() {
        // Build a representative V3 TxInfo with data in the key fields.
        // Input: tx_id=[0xaa;32] @ index 0, resolved to an enterprise output.
        let txout_ref_data = {
            // TxOutRef = Constr 0 [TxId, Integer]
            // TxId (inside TxOutRef) = Constr 0 [B bytes32]  — the V1-style inner txid
            let inner_txid = data_constr(0, vec![data_bs32(&[0xaa; 32])]);
            data_constr(0, vec![inner_txid, data_i(BigInt::from(0u64))])
        };
        let addr_data = data_constr(
            0,
            vec![
                data_constr(0, vec![data_bs28(&[0x11; 28])]), // PubKeyCredential
                data_constr(1, vec![]),                       // Nothing (no staking)
            ],
        );
        // ADA-only value for the resolved output
        let val_data = data_map(vec![(
            Data::B(vec![]),
            data_map(vec![(Data::B(vec![]), data_i(BigInt::from(2_000_000u64)))]),
        )]);
        let txout_data = data_constr(
            0,
            vec![
                addr_data.clone(),
                val_data,
                data_constr(0, vec![]), // OutputDatum::None
                data_constr(1, vec![]), // reference_script = Nothing
            ],
        );
        let _input_data = data_constr(0, vec![txout_ref_data, txout_data]);

        // Cert: TxCertUnRegStaking(PubKey([0x22;28]), None) = Constr 1 [Constr 0 [B28], Constr 1 []]
        let cert_data = Data::Constr(
            1,
            vec![
                data_constr(0, vec![data_bs28(&[0x22; 28])]),
                data_constr(1, vec![]),
            ],
        );

        // Withdrawal: PubKeyCredential([0x33;28]) -> 500_000 lovelace
        let wdrl_key = data_constr(0, vec![data_bs28(&[0x33; 28])]); // Credential::PubKey
        let wdrl_val = data_i(BigInt::from(500_000u64));

        // Mint: policy [0x44;28] -> token name "A" -> qty 10
        let mint_data = data_map(vec![(
            data_bs28(&[0x44; 28]),
            data_map(vec![(Data::B(b"A".to_vec()), data_i(BigInt::from(10i64)))]),
        )]);

        // ValidRange: Finite(1_000_000) closed lower, PosInf open upper
        // = Constr 0 [Constr 0 [Constr 1 [I 1000000], Constr 1 []], Constr 0 [Constr 2 [], Constr 1 []]]
        let valid_range_data = data_constr(
            0,
            vec![
                data_constr(
                    0,
                    vec![
                        data_constr(1, vec![data_i(BigInt::from(1_000_000i64))]),
                        data_constr(1, vec![]),
                    ],
                ),
                data_constr(0, vec![data_constr(2, vec![]), data_constr(1, vec![])]),
            ],
        );

        // txInfoId: bare B(32) for V3 (deriving newtype from BuiltinByteString)
        let txid_data = data_bs32(&[0xab; 32]);

        // current_treasury = Just 1_000_000 = Constr 0 [I 1_000_000]
        let treasury_data = data_constr(0, vec![data_i(BigInt::from(1_000_000u64))]);
        // treasury_donation = Nothing = Constr 1 []
        let donation_data = data_constr(1, vec![]);

        let info = TxInfoV3 {
            inputs: vec![TxInInfo {
                out_ref: TxOutRef {
                    tx_id: [0xaa; 32],
                    idx: 0,
                },
                resolved: TxOut {
                    address: Address {
                        payment: Credential::PubKey([0x11; 28]),
                        staking: None,
                    },
                    value: PlutusValue {
                        policies: vec![([0u8; 28], vec![(vec![], BigInt::from(2_000_000u64))])],
                    },
                    datum: OutputDatum::None,
                    reference_script: None,
                },
            }],
            reference_inputs: vec![],
            outputs: vec![],
            fee: BigInt::from(200_000u64),
            mint: PlutusValue {
                policies: vec![([0x44; 28], vec![(b"A".to_vec(), BigInt::from(10i64))])],
            },
            certs: vec![TxCert(Data::Constr(
                1,
                vec![
                    data_constr(0, vec![data_bs28(&[0x22; 28])]),
                    data_constr(1, vec![]),
                ],
            ))],
            wdrl: vec![(Credential::PubKey([0x33; 28]), BigInt::from(500_000u64))],
            valid_range: PosixTimeRange {
                lower: Some(1_000_000),
                upper: None,
            },
            signatories: vec![[0x55; 28]],
            redeemers: vec![],
            datums: vec![],
            txid: [0xab; 32],
            votes: vec![],
            proposal_procedures: vec![],
            current_treasury: Some(BigInt::from(1_000_000u64)),
            treasury_donation: None,
        };

        let d = info.to_data();
        let Data::Constr(tag, ref fields) = d else {
            panic!("TxInfoV3::to_data must be Constr; got {d:?}");
        };
        assert_eq!(tag, 0, "outer tag must be 0");
        assert_eq!(
            fields.len(),
            16,
            "TxInfoV3 must emit exactly 16 fields; got {}. Full data:\n{d:?}",
            fields.len()
        );

        // Field 0: inputs — List[TxInInfo]
        assert!(
            matches!(&fields[0], Data::List(v) if v.len() == 1),
            "field[0] (inputs) must be List of 1; got {:?}",
            fields[0]
        );
        // Field 1: reference_inputs — List[] (empty)
        assert!(
            matches!(&fields[1], Data::List(v) if v.is_empty()),
            "field[1] (reference_inputs) must be empty List; got {:?}",
            fields[1]
        );
        // Field 2: outputs — List[] (empty)
        assert!(
            matches!(&fields[2], Data::List(v) if v.is_empty()),
            "field[2] (outputs) must be empty List; got {:?}",
            fields[2]
        );
        // Field 3: fee — bare I
        assert_eq!(
            fields[3],
            data_i(BigInt::from(200_000u64)),
            "field[3] (fee) must be bare I(200_000)"
        );
        // Field 4: mint — Map
        assert_eq!(
            fields[4], mint_data,
            "field[4] (mint) must be Map with policy 0x44"
        );
        // Field 5: certs — List[TxCert]
        // Expected: List[Constr 1 [Constr 0 [B28], Constr 1 []]]
        assert!(
            matches!(&fields[5], Data::List(v) if v.len() == 1),
            "field[5] (certs) must be List of 1; got {:?}",
            fields[5]
        );
        assert_eq!(
            fields[5],
            data_list(vec![cert_data.clone()]),
            "field[5] (certs) must match expected TxCertUnRegStaking encoding"
        );
        // Field 6: wdrl — Map[(Credential, I)]
        // Expected: Map[(Constr 0 [B28 0x33], I 500_000)]
        assert_eq!(
            fields[6],
            data_map(vec![(wdrl_key.clone(), wdrl_val.clone())]),
            "field[6] (wdrl) must be Map with Credential key (NOT StakingHash)"
        );
        // Field 7: valid_range — POSIXTimeRange
        assert_eq!(
            fields[7], valid_range_data,
            "field[7] (valid_range) must be POSIXTimeRange"
        );
        // Field 8: signatories — List[B28]
        assert_eq!(
            fields[8],
            data_list(vec![data_bs28(&[0x55; 28])]),
            "field[8] (signatories) must be List[B28]"
        );
        // Field 9: redeemers — Map (empty)
        assert_eq!(
            fields[9],
            data_map(vec![]),
            "field[9] (redeemers) must be Map (empty)"
        );
        // Field 10: datums — Map (empty)
        assert_eq!(
            fields[10],
            data_map(vec![]),
            "field[10] (datums) must be Map (empty)"
        );
        // Field 11: txid — BARE B(32) (V3 TxId deriving newtype from BuiltinByteString)
        assert_eq!(
            fields[11], txid_data,
            "field[11] (txid) must be bare B(32), NOT Constr 0 [B32]"
        );
        // Also confirm it is NOT the Constr-wrapped form
        assert!(
            !matches!(&fields[11], Data::Constr(0, _)),
            "field[11] (txid) must NOT be Constr 0 [...]; V3 uses bare bytes"
        );
        // Field 12: votes — Map (empty)
        assert!(
            matches!(&fields[12], Data::Map(v) if v.is_empty()),
            "field[12] (votes) must be empty Map; got {:?}",
            fields[12]
        );
        // Field 13: proposal_procedures — List (empty)
        assert!(
            matches!(&fields[13], Data::List(v) if v.is_empty()),
            "field[13] (proposal_procedures) must be empty List; got {:?}",
            fields[13]
        );
        // Field 14: current_treasury — Just(1_000_000) = Constr 0 [I 1_000_000]
        assert_eq!(
            fields[14], treasury_data,
            "field[14] (current_treasury) must be Just(1_000_000)"
        );
        // Field 15: treasury_donation — Nothing = Constr 1 []
        assert_eq!(
            fields[15], donation_data,
            "field[15] (treasury_donation) must be Nothing"
        );

        // Verify the whole structure round-trips through CBOR
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2, "TxInfoV3 must round-trip through CBOR");
    }

    /// Assert wdrl Map key is Credential DIRECTLY — `Constr 0/1 [B28]` —
    /// NOT wrapped in StakingHash (`Constr 0 [Constr 0 [B28]]`).
    #[test]
    fn tx_info_v3_wdrl_key_is_bare_credential_not_staking_hash() {
        let mut info = empty_tx_info_v3();
        // Pubkey credential
        info.wdrl = vec![(Credential::PubKey([0xaa; 28]), BigInt::from(100u64))];
        let d = info.to_data();
        let Data::Constr(0, ref fields) = d else {
            panic!("expected Constr 0")
        };

        // Field 6 is wdrl
        let Data::Map(ref entries) = fields[6] else {
            panic!("wdrl (field[6]) must be Map; got {:?}", fields[6]);
        };
        assert_eq!(entries.len(), 1, "must have one wdrl entry");
        let (key, _val) = &entries[0];

        // Key must be Credential directly: Constr 0 [B28] for PubKey
        match key {
            Data::Constr(0, inner) => {
                assert_eq!(inner.len(), 1, "PubKeyCredential has 1 field");
                assert!(
                    matches!(&inner[0], Data::B(b) if b.len() == 28),
                    "PubKeyCredential field must be B(28); got {:?}",
                    inner[0]
                );
            }
            other => panic!(
                "wdrl key must be Credential (Constr 0 [B28]); got {other:?}. \
                 NOT Constr 0 [Constr 0 [B28]] (that would be StakingHash-wrapped)"
            ),
        }

        // Explicitly check it is NOT double-wrapped (StakingHash form would be
        // Constr 0 [Constr 0/1 [...]])
        if let Data::Constr(0, ref outer_fields) = key {
            if let Some(Data::Constr(_, _)) = outer_fields.first() {
                panic!(
                    "wdrl key is double-wrapped as StakingHash; V3 must use bare Credential. \
                     Got: {key:?}"
                );
            }
        }

        // Script credential variant
        let mut info2 = empty_tx_info_v3();
        info2.wdrl = vec![(Credential::Script([0xbb; 28]), BigInt::from(200u64))];
        let d2 = info2.to_data();
        let Data::Constr(0, ref fields2) = d2 else {
            panic!()
        };
        let Data::Map(ref entries2) = fields2[6] else {
            panic!()
        };
        // Script credential = Constr 1 [B28]
        assert!(
            matches!(&entries2[0].0, Data::Constr(1, inner) if inner.len() == 1),
            "ScriptCredential must be Constr 1; got {:?}",
            entries2[0].0
        );
    }

    /// Assert TxCertUnRegStaking encodes as Constr 1 [Constr 0/1[B28], Maybe].
    #[test]
    fn tx_cert_unreg_staking_encodes_as_constr_1_with_cred_and_maybe() {
        // TxCertUnRegStaking(PubKey([0xcc;28]), None) at V3 (PV<10 semantics = Nothing)
        let cert_data = Data::Constr(
            1,
            vec![
                data_constr(0, vec![data_bs28(&[0xcc; 28])]), // Credential::PubKey
                data_constr(1, vec![]),                       // None
            ],
        );

        let mut info = empty_tx_info_v3();
        info.certs = vec![TxCert(cert_data.clone())];
        let d = info.to_data();
        let Data::Constr(0, ref fields) = d else {
            panic!()
        };

        // Field 5 is certs
        let Data::List(ref cert_list) = fields[5] else {
            panic!("certs (field[5]) must be List; got {:?}", fields[5]);
        };
        assert_eq!(cert_list.len(), 1);
        // Check it matches the expected encoding exactly
        assert_eq!(
            cert_list[0], cert_data,
            "TxCertUnRegStaking must be Constr 1 [Credential, Maybe Lovelace]"
        );
        // Outer tag is 1
        let Data::Constr(tag, ref cert_fields) = cert_list[0] else {
            panic!()
        };
        assert_eq!(tag, 1u64, "TxCertUnRegStaking must use Constr tag 1");
        assert_eq!(cert_fields.len(), 2, "must have exactly 2 fields");
        // credential is Constr 0 [B28]
        assert!(
            matches!(&cert_fields[0], Data::Constr(0, inner) if inner.len() == 1),
            "cert credential must be Constr 0 [B28]; got {:?}",
            cert_fields[0]
        );
        // Maybe is Nothing = Constr 1 []
        assert_eq!(
            cert_fields[1],
            data_constr(1, vec![]),
            "cert Maybe (None) must be Constr 1 []"
        );
    }

    /// Assert txInfoId (field 11) is BARE B(32) in V3, NOT Constr 0 [B32].
    ///
    /// Reasoning: In V1/V2, `TxId` used `makeIsDataIndexed ''TxId [('TxId, 0)]`
    /// which gives `Constr 0 [B bytes32]`. In V3, `TxId` changed to
    /// `newtype TxId = TxId BuiltinByteString deriving newtype ToData`.
    /// The `deriving newtype` strategy delegates to the inner type's instance:
    /// `BuiltinByteString` has `instance ToData BuiltinByteString where
    ///   toBuiltinData = mkB . fromBuiltin` — i.e., BARE `B(bytes)`.
    /// Therefore V3 txInfoId MUST be bare `B(32)`.
    #[test]
    fn tx_info_v3_txid_is_bare_bytes_not_constr_wrapped() {
        let mut info = empty_tx_info_v3();
        info.txid = [0xde; 32];
        let d = info.to_data();
        let Data::Constr(0, ref fields) = d else {
            panic!()
        };

        // Field 11 is txid
        match &fields[11] {
            Data::B(b) => {
                assert_eq!(b.len(), 32, "txid must be 32 bytes");
                assert!(b.iter().all(|&x| x == 0xde), "txid bytes must match");
            }
            other => panic!(
                "V3 txInfoId (field[11]) must be bare B(32); got {other:?}. \
                 V1/V2 used Constr 0 [B32] but V3 uses `deriving newtype ToData` \
                 from BuiltinByteString which gives bare bytes."
            ),
        }
    }

    /// Pretty-print the full to_data CBOR hex for visual inspection.
    /// This test always passes — it is for eyeball verification.
    #[test]
    fn tx_info_v3_cbor_hex_for_inspection() {
        // Build a compact but non-trivial V3 TxInfo with one of each key field.
        let info = TxInfoV3 {
            inputs: vec![],
            reference_inputs: vec![],
            outputs: vec![],
            fee: BigInt::from(170_000u64),
            mint: PlutusValue::default(),
            certs: vec![TxCert(Data::Constr(
                1,
                vec![
                    data_constr(0, vec![data_bs28(&[0x22; 28])]),
                    data_constr(1, vec![]),
                ],
            ))],
            wdrl: vec![(Credential::PubKey([0x33; 28]), BigInt::from(500_000u64))],
            valid_range: PosixTimeRange {
                lower: Some(1_666_656_000_000i64),
                upper: None,
            },
            signatories: vec![[0x55; 28]],
            redeemers: vec![],
            datums: vec![],
            txid: [0xab; 32],
            votes: vec![],
            proposal_procedures: vec![],
            current_treasury: Some(BigInt::from(999_999u64)),
            treasury_donation: None,
        };
        let d = info.to_data();
        let cbor = d.to_cbor().unwrap();
        let hex = hex::encode(&cbor);

        // Annotated layout (for human review):
        // Constr 0 [
        //   [0] inputs:              List []
        //   [1] reference_inputs:    List []
        //   [2] outputs:             List []
        //   [3] fee:                 I(170_000)
        //   [4] mint:                Map []  (empty - no native assets)
        //   [5] certs:               List [Constr 1 [Constr 0 [B28 0x22*28], Constr 1 []]]
        //   [6] wdrl:                Map [(Constr 0 [B28 0x33*28], I 500_000)]
        //   [7] valid_range:         Constr 0 [LowerBound, UpperBound]
        //   [8] signatories:         List [B28 0x55*28]
        //   [9] redeemers:           Map []
        //   [10] datums:             Map []
        //   [11] txid:               B(32) = 0xab*32  (bare bytes, NOT Constr-wrapped)
        //   [12] votes:              Map []
        //   [13] proposal_procedures:List []
        //   [14] current_treasury:   Constr 0 [I 999_999]
        //   [15] treasury_donation:  Constr 1 []
        // ]
        println!("TxInfoV3 CBOR hex:\n{hex}");

        // Structural sanity: 16 fields
        let Data::Constr(0, ref fields) = d else {
            panic!()
        };
        assert_eq!(fields.len(), 16, "eyeball test: must still have 16 fields");

        // Print field-by-field summary
        for (i, f) in fields.iter().enumerate() {
            let desc = match i {
                0 => "inputs",
                1 => "reference_inputs",
                2 => "outputs",
                3 => "fee",
                4 => "mint",
                5 => "certs",
                6 => "wdrl",
                7 => "valid_range",
                8 => "signatories",
                9 => "redeemers",
                10 => "datums",
                11 => "txid",
                12 => "votes",
                13 => "proposal_procedures",
                14 => "current_treasury",
                15 => "treasury_donation",
                _ => "unknown",
            };
            println!("  field[{i:2}] {desc:25}: {f:?}");
        }
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
        let d = info.to_data(false);
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
        let d = info.to_data(false);
        let cbor = d.to_cbor().unwrap();
        let d2 = Data::from_cbor(&cbor).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn script_context_v1_v2_v3_all_top_constr_zero() {
        let p = ScriptPurpose::Minting([0u8; 28]);
        let v1 = ScriptContextV1 {
            tx_info: Rc::new(TxInfoV1 {
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
            }),
            purpose: p.clone(),
        };
        let v2 = ScriptContextV2 {
            tx_info: Rc::new(TxInfoV2 {
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
            }),
            purpose: p,
        };
        assert!(matches!(v1.to_data(false), Data::Constr(0, _)));
        assert!(matches!(v2.to_data(false), Data::Constr(0, _)));
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
        if let Ok((1, fields)) = si.to_data().into_constr() {
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
