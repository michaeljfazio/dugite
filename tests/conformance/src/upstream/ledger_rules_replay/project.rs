//! Field projections: the ENUMERATED field list that the replay compares.
//!
//! Both sides are projected into the same shape — `group -> path -> canonical
//! value string` — by two independent readers:
//!
//! * [`from_haskell`] reads the production decoder's `HaskellNewEpochState`
//!   (what Haskell computed: `st_out`, or `st` for the precondition check);
//! * [`from_dugite`] reads dugite's `LedgerState` fields after the production
//!   apply path ran.
//!
//! A field is on the list only when BOTH readers have a real source for it. Where
//! one side has none the field is in [`NOT_COMPARED`] with the reason, and is
//! never reported as a match.
//!
//! ## Haskell source of the field list
//!
//! cardano-ledger `a88b60bdcf3248dfe5a2f9372c188c399233f479` (the revision in
//! `tests/conformance/upstream/sources.toml` the corpus was generated from):
//!
//! * `NewEpochState` — `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs`
//!   `data NewEpochState`: `nesEL`, `nesBprev`, `nesBcur`, `nesEs`, `nesRu`,
//!   `nesPd`, `stashedAVVMAddresses`.
//! * `EpochState` — same file: `esChainAccountState`, `esLState`,
//!   `esSnapshots`, `esNonMyopic`.
//! * `LedgerState` — same file: `lsUTxOState`, `lsCertState`.
//! * `UTxOState` — same file: `utxosUtxo`, `utxosDeposited`, `utxosFees`,
//!   `utxosGovState`, `utxosInstantStake`, `utxosDonation`.
//! * `CertState` — `eras/conway/impl/src/Cardano/Ledger/Conway/State/CertState.hs`
//!   `data ConwayCertState`: `conwayCertVState`, `conwayCertPState`, `conwayCertDState`.
//! * `ConwayGovState` — `eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs`:
//!   `cgsProposals`, `cgsCommittee`, `cgsConstitution`, `cgsCurPParams`,
//!   `cgsPrevPParams`, `cgsFuturePParams`, `cgsDRepPulsingState`.
//! * `SnapShots` — `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs`.

use std::collections::BTreeMap;

use dugite_ledger::state::{FuturePParams, StakeSnapshot};
use dugite_ledger::LedgerState;
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::transaction::{
    Anchor, DRep, OutputDatum, Relay, TransactionOutput, Vote, Voter,
};
use dugite_serialization::haskell_snapshot::{
    decode_proposals_with_roots, govstate, HaskellCommitteeAuth, HaskellDRep, HaskellRelay,
    HaskellSnapShot, HaskellStakePoolState, HaskellVote,
};
use sha2::{Digest, Sha256};

use crate::upstream::ledger_rules_replay::imp_state::{
    typed_hash32, ImpState, RuleKind, IMP_ACTIVE_SLOTS_COEFF,
};

/// `path -> canonical value`.
pub type Group = BTreeMap<String, String>;

/// Every compared field group, in report order.
pub const GROUPS: &[&str] = &[
    "epoch",
    "utxo",
    "pots",
    "dstate",
    "pstate",
    "vstate",
    "gov",
    "pparams",
    "instant_stake",
    "snapshots",
    "blocks_made",
    "non_myopic",
    "inputs",
];

/// A projected state: the groups it carries, and which of [`GROUPS`] it does
/// not carry at all.
#[derive(Debug, Clone, Default)]
pub struct Projection {
    pub groups: BTreeMap<&'static str, Group>,
}

/// Which comparison a projection is for.
///
/// * `Pre`: dugite right after the import against Haskell's `st`. It carries the
///   `inputs` group (what the transition READS that the import must supply) and
///   leaves out the UTxO, whose conversion is shared by both sides.
/// * `Post`: dugite after the apply against Haskell's `st_out`. It carries the
///   UTxO and not `inputs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Pre,
    Post,
}

impl Projection {
    /// A projection with every group the rule's dump carries pre-declared
    /// (empty), so "carried but empty" and "not carried" stay distinguishable.
    ///
    /// For a LEDGER dump the epoch and the treasury/reserves pots come from
    /// `env`, which both sides copy, so they are not groups/paths there.
    pub fn new(nes_level: bool, scope: Scope) -> Self {
        let mut p = Projection::default();
        for g in GROUPS {
            let nes_only = matches!(
                *g,
                "snapshots" | "blocks_made" | "non_myopic" | "epoch" | "inputs"
            );
            let carried = (!nes_only || nes_level)
                && !(*g == "inputs" && scope == Scope::Post)
                && !(*g == "utxo" && scope == Scope::Pre);
            if carried {
                p.groups.insert(g, Group::new());
            }
        }
        p
    }
}

/// Fields that are deliberately NOT compared, and why. Reported with every run.
///
/// The `rules` column limits the entry to the rules it applies to
/// (`"both"`, `"ledger"`, `"newepoch"`).
pub const NOT_COMPARED: &[(&str, &str, &str)] = &[
    (
        "NEWEPOCH ratification (RATIFY / ENACT / proposal expiry)",
        "newepoch",
        "NOT EXERCISED: the import never maps `cgsDRepPulsingState`, so every NEWEPOCH vector \
         runs with nothing to ratify (#1132; also #1133 futurePParams, #1134 committee SNothing). \
         The `pre=inputs:*` kinds record it on every vector; there is no positive coverage of \
         ratification, enactment or expiry until #1132 is fixed",
    ),
    (
        "spsDelegators, dsFutureGenDelegs",
        "both",
        "the production decoder skips both; dugite has no field for either on import",
    ),
    (
        "spssStakeRatio, spssSelfDelegatedOwners, spssSelfDelegatedOwnersStake, spssNumDelegators",
        "newepoch",
        "StakePoolSnapShot fields the production decoder skips; dugite stores no equivalent",
    ),
    (
        "LEDGER epoch, treasury, reserves",
        "ledger",
        "copied from `env` into both sides, so a comparison would test the copy, not the rule",
    ),
    (
        "dsIRewards / dsDeltas (MIR) contents",
        "both",
        "dugite's import hard-codes them empty (Conway has no MIR); a count comparison would \
         compare a constant",
    ),
    (
        "utxosDeposited",
        "both",
        "dugite stores no such field; its components are compared individually (stake-key \
         deposits in total and per account, pool deposits, DRep deposits, proposal deposits)",
    ),
    (
        "nesRu (PulsingRewUpdate)",
        "newepoch",
        "the production decoder skips the field and dugite carries no field to decode it into: \
         dugite recomputes the reward update from the snapshots, so there is no Haskell value \
         and no dugite value to put side by side. Its EFFECT (treasury, reserves, reward \
         balances) IS compared",
    ),
    (
        "nesPd (PoolDistr)",
        "newepoch",
        "dugite has no field that holds it (it is re-derived from the mark snapshot on demand), \
         so there is nothing to read on the dugite side",
    ),
    (
        "stashedAVVMAddresses",
        "newepoch",
        "always empty in Conway; the production decoder skips it",
    ),
    (
        "GovState.cgsDRepPulsingState",
        "both",
        "the production decoder keeps it as opaque bytes (`drep_pulsing_raw`) and dugite's \
         `DRepPulsingState` is a different, not isomorphic, structure with no encoder in the \
         ledger crate: both values exist but no field-by-field mapping does",
    ),
    (
        "DRepState delegators (drepDelegs)",
        "both",
        "the production decoder skips the field; dugite rebuilds the set from the forward map \
         on import, so a comparison would test the rebuild against itself",
    ),
    (
        "GovActionState vote anchors",
        "both",
        "Haskell's per-voter vote maps hold only the `Vote`; the anchor is discarded when the \
         vote is recorded, so there is no Haskell value to compare. Vote VALUES are compared",
    ),
    (
        "snapshot pool owners / relays / metadata",
        "newepoch",
        "the cardano-node 11.x `StakePoolSnapShot` wire layout omits them (see \
         `from_haskell_snapshot`'s snapshot backfill), so the Haskell side has no value. vrf, \
         pledge, cost, margin and reward account are compared",
    ),
    (
        "GovState.cgsFuturePParams",
        "ledger",
        "dugite applies a transaction through `apply_block`, which runs the TICK steps \
         (`solidify_next_epoch_pparams` / `predict_future_pparams`) before the body; Haskell's \
         LEDGER rule runs no tick, so the two are not the same transition for this field",
    ),
    (
        "snapshots, blocks-made, non-myopic, pool distribution",
        "ledger",
        "a LEDGER dump holds a bare `LedgerState`, which does not contain them",
    ),
    (
        "consensus state (nonces, opcert counters), tip, era history",
        "both",
        "not part of `NewEpochState`; no ImpSpec dump carries them",
    ),
];

fn cred_key(tag: u8, h: &Hash28) -> String {
    format!("{tag}:{}", h.to_hex())
}

/// `typed Hash32` -> `tag:hash28`.
fn typed_key(h: &Hash32) -> String {
    let tag = h.as_bytes()[28];
    let mut b = [0u8; 28];
    b.copy_from_slice(&h.as_bytes()[..28]);
    format!("{tag}:{}", Hash28::from_bytes(b).to_hex())
}

fn anchor_str(a: &Option<(String, Hash32)>) -> String {
    match a {
        Some((u, h)) => format!("{u}@{}", h.to_hex()),
        None => "-".into(),
    }
}

fn dugite_anchor_str(a: &Option<Anchor>) -> String {
    anchor_str(&a.as_ref().map(|a| (a.url.clone(), a.data_hash)))
}

/// Canonical JSON (object keys sorted) of any serde value.
fn canon<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .map(|v| v.to_string())
        .unwrap_or_else(|e| format!("<unserialisable: {e}>"))
}

fn digest(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Two independent 64-bit FNV-1a lanes over a stream of length-prefixed fields.
struct Fp(u64, u64);

impl Fp {
    fn new() -> Self {
        Fp(0xcbf2_9ce4_8422_2325, 0x8422_2325_cbf2_9ce4)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in (b.len() as u64).to_le_bytes().iter().chain(b) {
            self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3);
            self.1 = (self.1 ^ u64::from(x.rotate_left(3) ^ 0x5a))
                .wrapping_mul(0x0000_0100_0000_01b3)
                .rotate_left(1);
        }
    }
    fn hex(&self) -> String {
        format!("{:016x}{:016x}", self.0, self.1)
    }
}

/// Canonical form of an output, identical for both sides: its lovelace for
/// reading, and a 128-bit fingerprint of EVERY semantic field (address bytes,
/// lovelace, each asset, datum, reference script) for equality. A fingerprint
/// rather than the full text because the UTxO is the one group with thousands of
/// entries per vector and the replay runs in a debug build.
fn out_str(o: &TransactionOutput) -> String {
    let mut f = Fp::new();
    f.bytes(&o.address.to_bytes());
    f.bytes(&o.value.coin.0.to_le_bytes());
    for (p, m) in &o.value.multi_asset {
        for (n, q) in m {
            f.bytes(p.as_bytes());
            f.bytes(&n.0);
            f.bytes(&q.to_le_bytes());
        }
    }
    match &o.datum {
        OutputDatum::None => f.bytes(b"d0"),
        OutputDatum::DatumHash(h) => {
            f.bytes(b"d1");
            f.bytes(h.as_bytes());
        }
        OutputDatum::InlineDatum { data, raw_cbor } => {
            f.bytes(b"d2");
            match raw_cbor {
                Some(r) => f.bytes(r),
                None => f.bytes(canon(data).as_bytes()),
            }
        }
    }
    match &o.script_ref {
        None => f.bytes(b"s0"),
        Some(s) => {
            f.bytes(b"s1");
            f.bytes(canon(s).as_bytes());
        }
    }
    format!(
        "coin={}|assets={}|datum={}|ref={}|fp={}",
        o.value.coin.0,
        o.value.multi_asset.values().map(|m| m.len()).sum::<usize>(),
        match &o.datum {
            OutputDatum::None => "-",
            OutputDatum::DatumHash(_) => "hash",
            OutputDatum::InlineDatum { .. } => "inline",
        },
        o.script_ref.is_some(),
        f.hex()
    )
}

fn hrelay(r: &HaskellRelay) -> Relay {
    match r {
        HaskellRelay::SingleHostAddr(port, v4, v6) => Relay::SingleHostAddr {
            port: *port,
            ipv4: *v4,
            ipv6: *v6,
        },
        HaskellRelay::SingleHostName(port, dns) => Relay::SingleHostName {
            port: *port,
            dns_name: dns.clone(),
        },
        HaskellRelay::MultiHostName(dns) => Relay::MultiHostName {
            dns_name: dns.clone(),
        },
    }
}

fn pool_str(s: &HaskellStakePoolState, with_deposit: bool) -> String {
    let mut owners: Vec<String> = s.owners.iter().map(|o| o.to_hex()).collect();
    owners.sort();
    let relays: Vec<String> = s.relays.iter().map(|r| canon(&hrelay(r))).collect();
    let meta = anchor_str(&s.metadata);
    let dep = if with_deposit {
        format!("|dep={}", s.deposit)
    } else {
        String::new()
    };
    format!(
        "vrf={}|pledge={}|cost={}|margin={}/{}|rwd={}|owners=[{}]|relays=[{}]|meta={meta}{dep}",
        s.vrf_hash.to_hex(),
        s.pledge,
        s.cost,
        s.margin_num,
        s.margin_den,
        hex::encode(&s.reward_account),
        owners.join(","),
        relays.join(",")
    )
}

fn drep_str_h(d: &HaskellDRep) -> String {
    match d {
        HaskellDRep::KeyHash(h) => format!("key:{}", h.to_hex()),
        HaskellDRep::ScriptHash(h) => format!("script:{}", h.to_hex()),
        HaskellDRep::AlwaysAbstain => "abstain".into(),
        HaskellDRep::AlwaysNoConfidence => "noconf".into(),
    }
}

fn drep_str_d(d: &DRep) -> String {
    let hex28 = |h: &[u8]| hex::encode(&h[..28]);
    match d {
        DRep::KeyHash(h) => format!("key:{}", hex28(h.as_bytes())),
        DRep::ScriptHash(h) => format!("script:{}", hex28(h.as_bytes())),
        DRep::Abstain => "abstain".into(),
        DRep::NoConfidence => "noconf".into(),
    }
}

fn vote_str_h(v: HaskellVote) -> &'static str {
    match v {
        HaskellVote::No => "No",
        HaskellVote::Yes => "Yes",
        HaskellVote::Abstain => "Abstain",
    }
}

fn vote_str_d(v: &Vote) -> &'static str {
    match v {
        Vote::No => "No",
        Vote::Yes => "Yes",
        Vote::Abstain => "Abstain",
    }
}

fn voter_key(v: &Voter) -> String {
    match v {
        Voter::ConstitutionalCommittee(c) => {
            format!("cc/{}:{}", u8::from(c.is_script()), c.to_hash().to_hex())
        }
        Voter::DRep(c) => format!("drep/{}:{}", u8::from(c.is_script()), c.to_hash().to_hex()),
        Voter::StakePool(h) => format!("pool/{}", hex::encode(&h.as_bytes()[..28])),
    }
}

/// `ProtocolParameters` -> one entry per top-level field.
fn pparams_entries(
    prefix: &str,
    pp: &dugite_primitives::protocol_params::ProtocolParameters,
    g: &mut Group,
) {
    // `active_slots_coeff` is genesis, not on the wire: the production importer
    // overwrites it from genesis, so both sides carry the same value by
    // construction and it is not a ledger-rule output.
    let mut pp = pp.clone();
    pp.active_slots_coeff = IMP_ACTIVE_SLOTS_COEFF;
    if let Ok(serde_json::Value::Object(m)) = serde_json::to_value(&pp) {
        for (k, v) in m {
            g.insert(format!("{prefix}/{k}"), v.to_string());
        }
    }
}

fn snap_pool_str(vrf: &Hash32, pledge: u64, cost: u64, mn: u64, md: u64, rwd: &[u8]) -> String {
    format!(
        "vrf={}|pledge={pledge}|cost={cost}|margin={mn}/{md}|rwd={}",
        vrf.to_hex(),
        hex::encode(rwd)
    )
}

fn haskell_snap(name: &str, s: &HaskellSnapShot, g: &mut Group) {
    for ((tag, h), c) in &s.stake {
        g.insert(format!("{name}/stake/{}", cred_key(*tag, h)), c.to_string());
    }
    for ((tag, h), p) in &s.delegations {
        g.insert(format!("{name}/deleg/{}", cred_key(*tag, h)), p.to_hex());
    }
    for (id, p) in &s.pool_params {
        g.insert(
            format!("{name}/pool/{}", id.to_hex()),
            snap_pool_str(
                &p.vrf_hash,
                p.pledge,
                p.cost,
                p.margin_num,
                p.margin_den,
                &p.reward_account,
            ),
        );
    }
}

fn dugite_snap(name: &str, s: &Option<StakeSnapshot>, g: &mut Group) {
    let Some(s) = s else { return };
    for (c, v) in s.stake_distribution.iter() {
        g.insert(format!("{name}/stake/{}", typed_key(c)), v.0.to_string());
    }
    for (c, p) in s.delegations.iter() {
        g.insert(format!("{name}/deleg/{}", typed_key(c)), p.to_hex());
    }
    for (id, p) in s.pool_params.iter() {
        g.insert(
            format!("{name}/pool/{}", id.to_hex()),
            snap_pool_str(
                &p.vrf_keyhash,
                p.pledge.0,
                p.cost.0,
                p.margin_numerator,
                p.margin_denominator,
                &p.reward_account,
            ),
        );
    }
}

fn likelihood_str(w: &[f32]) -> String {
    let bits: String = w.iter().map(|f| format!("{:08x}", f.to_bits())).collect();
    // The first values are shown for the reader; equality is on all of them.
    let head: Vec<String> = w.iter().take(3).map(|f| format!("{f:e}")).collect();
    format!(
        "n={}|first=[{}]|sha={}",
        w.len(),
        head.join(","),
        digest(&bits)
    )
}

/// Project what Haskell computed.
pub fn from_haskell(imp: &ImpState, scope: Scope) -> Projection {
    let with_utxo = scope == Scope::Post;
    let nes = &imp.nes;
    let mut p = Projection::new(imp.carried.nes_level, scope);
    let mut put = |grp: &'static str, k: String, v: String| {
        p.groups.entry(grp).or_default().insert(k, v);
    };

    if imp.carried.nes_level {
        put("epoch", "epoch".into(), nes.epoch.0.to_string());
        put("pots", "treasury".into(), nes.treasury.to_string());
        put("pots", "reserves".into(), nes.reserves.to_string());
    }

    if with_utxo {
        for (i, o) in &imp.utxo {
            put(
                "utxo",
                format!("{}#{}", i.transaction_id.to_hex(), i.index),
                out_str(o),
            );
        }
    }

    put("pots", "fees".into(), nes.fees.to_string());
    put("pots", "donation".into(), nes.donation.to_string());

    let cs = &nes.cert_state;
    for ((tag, h), a) in &cs.dstate.accounts {
        put(
            "dstate",
            format!("acct/{}", cred_key(*tag, h)),
            format!(
                "bal={}|dep={}|pool={}|drep={}",
                a.balance,
                a.deposit,
                a.pool_delegation
                    .as_ref()
                    .map(|p| p.to_hex())
                    .unwrap_or_else(|| "-".into()),
                a.drep_delegation
                    .as_ref()
                    .map(drep_str_h)
                    .unwrap_or_else(|| "-".into())
            ),
        );
    }
    put(
        "dstate",
        "total_key_deposits".into(),
        cs.dstate
            .accounts
            .values()
            .map(|a| a.deposit)
            .sum::<u64>()
            .to_string(),
    );
    for (g, (d, v)) in &cs.dstate.genesis_delegates {
        put(
            "dstate",
            format!("gendeleg/{}", g.to_hex()),
            format!("{}/{}", d.to_hex(), v.to_hex()),
        );
    }

    for (id, s) in &cs.pstate.stake_pools {
        put("pstate", format!("pool/{}", id.to_hex()), pool_str(s, true));
    }
    for (id, s) in &cs.pstate.future_pool_params {
        put(
            "pstate",
            format!("fpool/{}", id.to_hex()),
            pool_str(s, false),
        );
    }
    for (id, e) in &cs.pstate.retirements {
        put("pstate", format!("retire/{}", id.to_hex()), e.0.to_string());
    }
    for (h, c) in &cs.pstate.vrf_key_hashes {
        put("pstate", format!("vrf/{}", h.to_hex()), c.to_string());
    }

    for ((tag, h), d) in &cs.vstate.dreps {
        put(
            "vstate",
            format!("drep/{}", cred_key(*tag, h)),
            format!(
                "exp={}|dep={}|anchor={}",
                d.expiry.0,
                d.deposit,
                anchor_str(&d.anchor)
            ),
        );
    }
    for ((tag, h), a) in &cs.vstate.committee_state {
        let v = match a {
            HaskellCommitteeAuth::Hot(t, hh) => format!("hot:{}", cred_key(*t, hh)),
            HaskellCommitteeAuth::Resigned(an) => format!("resigned:{}", anchor_str(an)),
        };
        put("vstate", format!("cc/{}", cred_key(*tag, h)), v);
    }
    put(
        "vstate",
        "dormant_epochs".into(),
        cs.vstate.dormant_epochs.to_string(),
    );

    // ── GovState ─────────────────────────────────────────────────────────
    let gs = &nes.gov_state;
    match &gs.committee_raw {
        Some(raw) => match govstate::decode_committee(raw) {
            Ok((c, _)) => {
                put("gov", "committee/present".into(), "yes".into());
                for ((tag, h), e) in &c.members {
                    put(
                        "gov",
                        format!("committee/member/{}", cred_key(*tag, h)),
                        e.to_string(),
                    );
                }
                put(
                    "gov",
                    "committee/threshold".into(),
                    format!("{}/{}", c.threshold.0, c.threshold.1),
                );
            }
            Err(e) => put(
                "gov",
                "committee/present".into(),
                format!("<decode error: {e}>"),
            ),
        },
        None => put("gov", "committee/present".into(), "no".into()),
    }
    put(
        "gov",
        "constitution".into(),
        match &gs.constitution {
            Some(c) => format!(
                "{}@{}|script={}",
                c.anchor_url,
                c.anchor_hash.to_hex(),
                c.script_hash
                    .as_ref()
                    .map(|h| h.to_hex())
                    .unwrap_or_else(|| "-".into())
            ),
            None => "-".into(),
        },
    );
    match decode_proposals_with_roots(&gs.proposals_raw) {
        Ok(pr) => {
            let root = |o: &Option<dugite_serialization::haskell_snapshot::HaskellGovActionId>| {
                o.as_ref()
                    .map(|i| format!("{}#{}", i.tx_hash.to_hex(), i.index))
                    .unwrap_or_else(|| "-".into())
            };
            put(
                "gov",
                "root/pparam_update".into(),
                root(&pr.roots.pparam_update),
            );
            put("gov", "root/hard_fork".into(), root(&pr.roots.hard_fork));
            put("gov", "root/committee".into(), root(&pr.roots.committee));
            put(
                "gov",
                "root/constitution".into(),
                root(&pr.roots.constitution),
            );
            for (n, a) in pr.actions.iter().enumerate() {
                let id = format!("{}#{}", a.gas_id.tx_hash.to_hex(), a.gas_id.index);
                put("gov", format!("proposal/order/{n:06}"), id.clone());
                put(
                    "gov",
                    format!("proposal/{id}"),
                    format!(
                        "dep={}|ret={}|act={}|anchor={}|proposed={}|expires={}",
                        a.procedure.deposit.0,
                        hex::encode(&a.procedure.return_addr),
                        digest(&canon(&a.procedure.gov_action)),
                        dugite_anchor_str(&Some(a.procedure.anchor.clone())),
                        a.proposed_in.0,
                        a.expires_after.0
                    ),
                );
                for ((tag, h), v) in &a.committee_votes {
                    put(
                        "gov",
                        format!("vote/{id}/cc/{}", cred_key(*tag, h)),
                        vote_str_h(*v).into(),
                    );
                }
                for ((tag, h), v) in &a.drep_votes {
                    put(
                        "gov",
                        format!("vote/{id}/drep/{}", cred_key(*tag, h)),
                        vote_str_h(*v).into(),
                    );
                }
                for (h, v) in &a.pool_votes {
                    put(
                        "gov",
                        format!("vote/{id}/pool/{}", h.to_hex()),
                        vote_str_h(*v).into(),
                    );
                }
            }
        }
        Err(e) => put("gov", "proposals".into(), format!("<decode error: {e}>")),
    }
    if imp.kind == RuleKind::NewEpoch {
        // Compared for NEWEPOCH only; see `NOT_COMPARED` (cgsFuturePParams).
        put(
            "gov",
            "future_pparams/tag".into(),
            gs.future_pparams_tag.to_string(),
        );
        if let Some(f) = &gs.future_pparams {
            let mut g = Group::new();
            pparams_entries("future", f, &mut g);
            for (k, v) in g {
                put("gov", format!("future_pparams/{k}"), v);
            }
        }
    }

    {
        let mut g = Group::new();
        pparams_entries("cur", &nes.cur_pparams, &mut g);
        pparams_entries("prev", &nes.prev_pparams, &mut g);
        for (k, v) in g {
            put("pparams", k, v);
        }
    }

    for ((tag, h), c) in &nes.instant_stake {
        put("instant_stake", cred_key(*tag, h), c.to_string());
    }

    if imp.carried.nes_level {
        haskell_snap(
            "mark",
            &nes.snapshots.mark,
            p.groups.entry("snapshots").or_default(),
        );
        haskell_snap(
            "set",
            &nes.snapshots.set,
            p.groups.entry("snapshots").or_default(),
        );
        haskell_snap(
            "go",
            &nes.snapshots.go,
            p.groups.entry("snapshots").or_default(),
        );
        p.groups
            .entry("snapshots")
            .or_default()
            .insert("fee".into(), nes.snapshots.fee.to_string());
        let bm = p.groups.entry("blocks_made").or_default();
        for (id, n) in &nes.blocks_made_prev {
            bm.insert(format!("prev/{}", id.to_hex()), n.to_string());
        }
        for (id, n) in &nes.blocks_made_cur {
            bm.insert(format!("cur/{}", id.to_hex()), n.to_string());
        }
        let nm = p.groups.entry("non_myopic").or_default();
        nm.insert("reward_pot".into(), nes.non_myopic.reward_pot.to_string());
        for (id, w) in &nes.non_myopic.likelihoods {
            nm.insert(format!("likelihood/{}", id.to_hex()), likelihood_str(w));
        }
    }
    if scope == Scope::Pre && imp.carried.nes_level {
        // What NEWEPOCH reads that the import must supply. dugite's import maps
        // neither (see `from_haskell_snapshot`), so these differ whenever Haskell
        // holds them: the transition then runs without them.
        set_input(
            &mut p,
            "drep_pulsing_state",
            if nes.gov_state.drep_pulsing_raw.is_empty() {
                "absent"
            } else {
                "present"
            }
            .into(),
        );
        set_input(
            &mut p,
            "nes_ru",
            if imp.reward_update_present {
                "SJust"
            } else {
                "SNothing"
            }
            .into(),
        );
    }
    p
}

/// Project what dugite holds.
pub fn from_dugite(st: &LedgerState, kind: RuleKind, scope: Scope) -> Projection {
    let with_utxo = scope == Scope::Post;
    let mut p = Projection::new(kind == RuleKind::NewEpoch, scope);
    let mut put = |grp: &'static str, k: String, v: String| {
        p.groups.entry(grp).or_default().insert(k, v);
    };

    if kind == RuleKind::NewEpoch {
        put("epoch", "epoch".into(), st.epoch.0.to_string());
        put("pots", "treasury".into(), st.epochs.treasury.0.to_string());
        put("pots", "reserves".into(), st.epochs.reserves.0.to_string());
    }

    if with_utxo {
        for (i, o) in st.utxo.utxo_set.iter() {
            put(
                "utxo",
                format!("{}#{}", i.transaction_id.to_hex(), i.index),
                out_str(&o),
            );
        }
    }

    let gov = &*st.gov.governance;
    put("pots", "fees".into(), st.utxo.epoch_fees.0.to_string());
    put(
        "pots",
        "donation".into(),
        st.utxo.pending_donations.0.to_string(),
    );

    for (c, bal) in st.certs.reward_accounts.iter() {
        put(
            "dstate",
            format!("acct/{}", typed_key(c)),
            format!(
                "bal={}|dep={}|pool={}|drep={}",
                bal.0,
                st.certs.stake_key_deposits.get(c).copied().unwrap_or(0),
                st.certs
                    .delegations
                    .get(c)
                    .map(|p| p.to_hex())
                    .unwrap_or_else(|| "-".into()),
                gov.vote_delegations
                    .get(c)
                    .map(drep_str_d)
                    .unwrap_or_else(|| "-".into())
            ),
        );
    }
    put(
        "dstate",
        "total_key_deposits".into(),
        st.certs.total_stake_key_deposits.to_string(),
    );
    for (g, (d, v)) in &st.genesis_delegates {
        put(
            "dstate",
            format!("gendeleg/{}", g.to_hex()),
            format!("{}/{}", d.to_hex(), v.to_hex()),
        );
    }

    let relays = |r: &[Relay]| -> String { r.iter().map(canon).collect::<Vec<_>>().join(",") };
    for (id, r) in st.certs.pool_params.iter() {
        let mut owners: Vec<String> = r.owners.iter().map(|o| o.to_hex()).collect();
        owners.sort();
        put(
            "pstate",
            format!("pool/{}", id.to_hex()),
            format!(
                "vrf={}|pledge={}|cost={}|margin={}/{}|rwd={}|owners=[{}]|relays=[{}]|meta={}|dep={}",
                r.vrf_keyhash.to_hex(),
                r.pledge.0,
                r.cost.0,
                r.margin_numerator,
                r.margin_denominator,
                hex::encode(&r.reward_account),
                owners.join(","),
                relays(&r.relays),
                anchor_str(&match (&r.metadata_url, &r.metadata_hash) {
                    (Some(u), Some(h)) => Some((u.clone(), *h)),
                    _ => None,
                }),
                st.certs.pool_deposits.get(id).copied().unwrap_or(0)
            ),
        );
    }
    for (id, r) in &st.certs.future_pool_params {
        let mut owners: Vec<String> = r.owners.iter().map(|o| o.to_hex()).collect();
        owners.sort();
        put(
            "pstate",
            format!("fpool/{}", id.to_hex()),
            format!(
                "vrf={}|pledge={}|cost={}|margin={}/{}|rwd={}|owners=[{}]|relays=[{}]|meta={}",
                r.vrf_keyhash.to_hex(),
                r.pledge.0,
                r.cost.0,
                r.margin_numerator,
                r.margin_denominator,
                hex::encode(&r.reward_account),
                owners.join(","),
                relays(&r.relays),
                anchor_str(&match (&r.metadata_url, &r.metadata_hash) {
                    (Some(u), Some(h)) => Some((u.clone(), *h)),
                    _ => None,
                }),
            ),
        );
    }
    for (id, e) in &st.certs.pending_retirements {
        put("pstate", format!("retire/{}", id.to_hex()), e.0.to_string());
    }
    for (h, c) in st.certs.vrf_key_hashes.iter() {
        put("pstate", format!("vrf/{}", h.to_hex()), c.to_string());
    }

    for (_k, d) in gov.dreps.iter() {
        put(
            "vstate",
            format!(
                "drep/{}",
                cred_key(u8::from(d.credential.is_script()), d.credential.to_hash())
            ),
            format!(
                "exp={}|dep={}|anchor={}",
                d.drep_expiry.0,
                d.deposit.0,
                dugite_anchor_str(&d.anchor)
            ),
        );
    }
    for (cold, hot) in gov.committee_hot_keys.iter() {
        put(
            "vstate",
            format!("cc/{}", typed_key(cold)),
            format!("hot:{}", typed_key(hot)),
        );
    }
    for (cold, anchor) in gov.committee_resigned.iter() {
        put(
            "vstate",
            format!("cc/{}", typed_key(cold)),
            format!("resigned:{}", dugite_anchor_str(anchor)),
        );
    }
    put(
        "vstate",
        "dormant_epochs".into(),
        gov.num_dormant_epochs.to_string(),
    );

    // GovState.
    put(
        "gov",
        "committee/present".into(),
        if gov.no_confidence { "no" } else { "yes" }.into(),
    );
    for (c, e) in gov.committee_expiration.iter() {
        put(
            "gov",
            format!("committee/member/{}", typed_key(c)),
            e.0.to_string(),
        );
    }
    put(
        "gov",
        "committee/threshold".into(),
        gov.committee_threshold
            .as_ref()
            .map(|t| format!("{}/{}", t.numerator, t.denominator))
            .unwrap_or_else(|| "-".into()),
    );
    put(
        "gov",
        "constitution".into(),
        match &gov.constitution {
            Some(c) => format!(
                "{}@{}|script={}",
                c.anchor.url,
                c.anchor.data_hash.to_hex(),
                c.script_hash
                    .as_ref()
                    .map(|h| h.to_hex())
                    .unwrap_or_else(|| "-".into())
            ),
            None => "-".into(),
        },
    );
    let root = |o: &Option<dugite_primitives::transaction::GovActionId>| {
        o.as_ref()
            .map(|i| format!("{}#{}", i.transaction_id.to_hex(), i.action_index))
            .unwrap_or_else(|| "-".into())
    };
    put(
        "gov",
        "root/pparam_update".into(),
        root(&gov.enacted_pparam_update),
    );
    put("gov", "root/hard_fork".into(), root(&gov.enacted_hard_fork));
    put("gov", "root/committee".into(), root(&gov.enacted_committee));
    put(
        "gov",
        "root/constitution".into(),
        root(&gov.enacted_constitution),
    );
    let mut ordered: Vec<_> = gov.proposals.iter().collect();
    ordered.sort_by_key(|(_, s)| s.submission_index);
    for (n, (id, s)) in ordered.iter().enumerate() {
        let ids = format!("{}#{}", id.transaction_id.to_hex(), id.action_index);
        put("gov", format!("proposal/order/{n:06}"), ids.clone());
        put(
            "gov",
            format!("proposal/{ids}"),
            format!(
                "dep={}|ret={}|act={}|anchor={}|proposed={}|expires={}",
                s.procedure.deposit.0,
                hex::encode(&s.procedure.return_addr),
                digest(&canon(&s.procedure.gov_action)),
                dugite_anchor_str(&Some(s.procedure.anchor.clone())),
                s.proposed_epoch.0,
                s.expires_epoch.0
            ),
        );
    }
    for (id, votes) in gov.votes_by_action.iter() {
        let ids = format!("{}#{}", id.transaction_id.to_hex(), id.action_index);
        for (voter, vp) in votes.iter() {
            put(
                "gov",
                format!("vote/{ids}/{}", voter_key(voter)),
                vote_str_d(&vp.vote).into(),
            );
        }
    }
    if kind == RuleKind::NewEpoch {
        let (tag, with) = match &gov.future_pparams {
            FuturePParams::NoPParamsUpdate => (0u8, None),
            FuturePParams::DefinitePParamsUpdate(pp) => (1, Some(pp.as_ref())),
            FuturePParams::PotentialPParamsUpdate(pp) => (2, pp.as_deref()),
        };
        put("gov", "future_pparams/tag".into(), tag.to_string());
        if let Some(f) = with {
            let mut g = Group::new();
            pparams_entries("future", f, &mut g);
            for (k, v) in g {
                put("gov", format!("future_pparams/{k}"), v);
            }
        }
    }

    {
        let mut g = Group::new();
        pparams_entries("cur", &st.epochs.protocol_params, &mut g);
        pparams_entries("prev", &st.epochs.prev_protocol_params, &mut g);
        for (k, v) in g {
            put("pparams", k, v);
        }
    }

    for (c, v) in &st.certs.stake_distribution.stake_map {
        put("instant_stake", typed_key(c), v.0.to_string());
    }

    if kind == RuleKind::NewEpoch {
        dugite_snap(
            "mark",
            &st.epochs.snapshots.mark,
            p.groups.entry("snapshots").or_default(),
        );
        dugite_snap(
            "set",
            &st.epochs.snapshots.set,
            p.groups.entry("snapshots").or_default(),
        );
        dugite_snap(
            "go",
            &st.epochs.snapshots.go,
            p.groups.entry("snapshots").or_default(),
        );
        p.groups
            .entry("snapshots")
            .or_default()
            .insert("fee".into(), st.epochs.snapshots.ss_fee.0.to_string());
        let bm = p.groups.entry("blocks_made").or_default();
        for (id, n) in st.epochs.snapshots.bprev_blocks_by_pool.iter() {
            bm.insert(format!("prev/{}", id.to_hex()), n.to_string());
        }
        for (id, n) in st.consensus.epoch_blocks_by_pool.iter() {
            bm.insert(format!("cur/{}", id.to_hex()), n.to_string());
        }
        let nm = p.groups.entry("non_myopic").or_default();
        nm.insert(
            "reward_pot".into(),
            st.epochs.non_myopic.reward_pot.0.to_string(),
        );
        for (id, l) in &st.epochs.non_myopic.likelihoods {
            nm.insert(format!("likelihood/{}", id.to_hex()), likelihood_str(&l.0));
        }
    }
    if scope == Scope::Pre && kind == RuleKind::NewEpoch {
        set_input(
            &mut p,
            "drep_pulsing_state",
            if gov.drep_pulsing_state.is_some() {
                "present"
            } else {
                "absent"
            }
            .into(),
        );
        set_input(
            &mut p,
            "nes_ru",
            if st.epochs.rupd_snapshot.is_some() {
                "SJust"
            } else {
                "SNothing"
            }
            .into(),
        );
    }
    p
}

fn set_input(p: &mut Projection, k: &str, v: String) {
    p.groups
        .entry("inputs")
        .or_default()
        .insert(k.to_string(), v);
}

/// Silence an unused-import lint when a helper is only used on one side.
#[allow(dead_code)]
fn _typed(tag: u8, h: &Hash28) -> Hash32 {
    typed_hash32(tag, h)
}
