//! Shared structured generators for the encode-first fuzz targets.
//!
//! ## Why these exist (issue #974)
//!
//! Every one of the harness's original 53 targets took `|data: &[u8]|`. That
//! makes every encoder test **decode-first**: the fuzzer must synthesise bytes
//! `decode_transaction` accepts before `encode_transaction` runs at all. So the
//! reachable encoder surface is bounded by what a byte mutator can invent.
//!
//! In practice the deep, optional, rarely-populated fields are unreachable. To
//! exercise #951 a mutator must build a valid transaction body carrying an
//! `update` field holding a `ProtocolParamUpdate` with key 26 populated with a
//! ten-element `drep_voting_thresholds`. To exercise #948 it must build a valid
//! `vote_delegation` certificate. Starting from a corpus of real blocks — none
//! of which contain either — the probability is not small, it is zero.
//!
//! That is the mechanical reason the whole v2.4.1-v2.4.5 encoder wave went
//! undetected by a nightly job running the entire time.
//!
//! These generators invert the direction: build the structure, encode it, then
//! require the decoder to reproduce it.
//!
//! ## Deliberate bias
//!
//! A uniform `#[derive(Arbitrary)]` would spend its entropy on the wide middle
//! of each field's range and essentially never land on a boundary. Every
//! generator here is biased toward the shapes with a bug history:
//!
//! - collection sizes straddling the `encodeMap` / `variableListLenEncoding`
//!   threshold at 23/24 and the CBOR header-width change at 255/256 (#930,
//!   #938)
//! - integers straddling the Word64 boundary that #952 turned on: `2^64 - 1`,
//!   `2^64`, `2^64 + 1`, `i128::MAX`
//! - every `ProtocolParamUpdate` key 0-37 populated at once (#951 lived in key
//!   26; key 15/17 semantics were #919)
//! - all 19 certificate variants, all 7 `GovAction` variants, all 3 `Voter`
//!   discriminators across both credential types (#948, #932, #940)
//! - populated witness sets including bootstrap witnesses (#939)
//!
//! ## Standing caveat
//!
//! A same-process round-trip is necessary but NOT sufficient: a wrong shape
//! shared by encoder AND decoder round-trips perfectly. #951 was caught only
//! because the two disagreed. This raises reachability; Haskell-derived
//! fixtures remain the oracle.

/// Node modules compiled directly into this crate — see `node::n2c_query`.
pub mod node;

use arbitrary::{Arbitrary, Unstructured};
use dugite_primitives::address::{Address, BaseAddress, EnterpriseAddress, RewardAddress};
use dugite_primitives::credentials::Credential;
use dugite_primitives::era::Era;
use dugite_primitives::hash::{Hash, Hash28, Hash32, PolicyId};
use dugite_primitives::network::NetworkId;
use dugite_primitives::time::SlotNo;
use dugite_primitives::transaction::*;
use dugite_primitives::value::{AssetName, Lovelace, MultiAsset, Value};
use std::collections::BTreeMap;

/// Entropy-stream wrapper with Cardano-shaped, boundary-biased draws.
///
/// Never fails: when the stream runs dry, `Unstructured` returns zeros/defaults
/// and generation continues. A fuzz generator that could return `Err` would
/// turn every short input into a silent skip, which is the failure shape this
/// whole work stream exists to remove.
/// Which `protocol_param_update` wire type to generate for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PpuShape {
    /// Conway and later: keys 0-11, 16-33, plus the Dijkstra additions 34-37.
    Conway,
    /// Shelley..Babbage: keys 0-24, including 12-15.
    PreConway,
}

pub struct Gen<'a> {
    u: Unstructured<'a>,
}

impl<'a> Gen<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Gen {
            u: Unstructured::new(data),
        }
    }

    pub fn byte(&mut self) -> u8 {
        u8::arbitrary(&mut self.u).unwrap_or(0)
    }

    pub fn bool(&mut self) -> bool {
        self.byte() & 1 == 1
    }

    /// True with probability ~`n`/255. Used to decide whether an optional
    /// field is populated; biased high so deep fields are actually reached.
    pub fn chance(&mut self, n: u8) -> bool {
        self.byte() < n
    }

    pub fn u64(&mut self) -> u64 {
        u64::arbitrary(&mut self.u).unwrap_or(0)
    }

    pub fn u32(&mut self) -> u32 {
        u32::arbitrary(&mut self.u).unwrap_or(0)
    }

    /// Pick one of `n` alternatives, uniformly over the byte range.
    pub fn choice(&mut self, n: u8) -> u8 {
        self.byte() % n.max(1)
    }

    /// A collection length biased onto the CBOR framing boundaries.
    ///
    /// `encodeMap` / `variableListLenEncoding` switch from a definite header to
    /// the indefinite `0xbf`/`0x9f` … `0xff` form above 23 entries, and the
    /// definite header itself widens at 256. Both boundaries have a bug
    /// history: #930 (a 1-byte over-count at >=256 entries produced a false
    /// `OutputValueTooLarge` reject) and #938.
    ///
    /// `cap` bounds the cost — 256-element collections of expensive items make
    /// each iteration slow enough to hurt the fuzzer's throughput more than the
    /// extra coverage is worth.
    pub fn collection_len(&mut self, cap: usize) -> usize {
        const BOUNDARIES: [usize; 12] = [0, 1, 2, 22, 23, 24, 25, 30, 254, 255, 256, 257];
        let n = match self.choice(4) {
            // Most draws land exactly on a boundary.
            0..=2 => BOUNDARIES[(self.byte() as usize) % BOUNDARIES.len()],
            // The rest spread out, so the generator is not purely boundary-bound.
            _ => self.byte() as usize,
        };
        n.min(cap)
    }

    pub fn hash32(&mut self) -> Hash32 {
        let mut bytes = [0u8; 32];
        for b in bytes.iter_mut() {
            *b = self.byte();
        }
        Hash::from_bytes(bytes)
    }

    pub fn hash28(&mut self) -> Hash28 {
        let mut bytes = [0u8; 28];
        for b in bytes.iter_mut() {
            *b = self.byte();
        }
        Hash::from_bytes(bytes)
    }

    pub fn bytes(&mut self, cap: usize) -> Vec<u8> {
        let len = self.collection_len(cap);
        (0..len).map(|_| self.byte()).collect()
    }

    /// A coin amount biased toward the values that break naive width handling:
    /// the CBOR header-width steps and the u64 ceiling.
    pub fn coin(&mut self) -> u64 {
        match self.choice(6) {
            0 => 0,
            1 => 1,
            2 => 23,
            3 => 24,
            4 => u64::MAX,
            _ => self.u64(),
        }
    }

    /// The same, as the `Lovelace` newtype.
    pub fn lovelace(&mut self) -> Lovelace {
        Lovelace(self.coin())
    }

    /// A rational in lowest terms, including the degenerate shapes an encoder
    /// can mishandle.
    ///
    /// Reduced deliberately: `Reader::read_rational` divides by the gcd at
    /// DECODE time, matching Haskell's on-chain `Rational`/`BoundedRatio`,
    /// which is always built through GHC's `%` smart constructor
    /// (`cardano-ledger-binary`'s `decodeIntegralRational`). A generated
    /// `18446744073709551615/18446744073709551615` therefore comes back as
    /// `1/1` — value-preserving normalisation, not an encoder defect. Feeding
    /// the generator non-reduced pairs would manufacture a permanent false
    /// positive instead of a finding.
    pub fn rational(&mut self) -> Rational {
        let (numerator, denominator) = match self.choice(5) {
            0 => (0, 1),
            1 => (1, 1),
            2 => (1, u64::MAX),
            3 => (u64::MAX, 1),
            _ => (self.u64(), self.u64().max(1)),
        };
        let divisor = gcd(numerator, denominator).max(1);
        Rational {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }

    /// An integer straddling the Word64 boundary that #952 turned on.
    ///
    /// `encode_plutus_int` gated the bignum path on `to_i128()` and then called
    /// `encode_int(i128)`, whose `value as u64` SILENTLY TRUNCATES. Haskell's
    /// threshold is Word64: the plain int form covers `[-(2^64) .. 2^64-1]`
    /// only. Integers in `(2^64, i128::MAX]` wrapped mod 2^64, producing a
    /// wrong `script_data_hash` and therefore wrong phase-2.
    pub fn boundary_bigint(&mut self) -> num_bigint::BigInt {
        use num_bigint::BigInt;
        let two_64 = BigInt::from(1u128 << 64);
        match self.choice(10) {
            0 => BigInt::from(0),
            1 => BigInt::from(-1),
            2 => &two_64 - 1, // 2^64 - 1: largest plain uint
            3 => two_64.clone(),
            4 => &two_64 + 1, // first value that must take the bignum path
            5 => BigInt::from(i128::MAX),
            6 => BigInt::from(i128::MIN),
            7 => -(&two_64) - 1,
            8 => -(&two_64),
            _ => BigInt::from(self.u64()),
        }
    }

    pub fn credential(&mut self) -> Credential {
        if self.bool() {
            Credential::VerificationKey(self.hash28())
        } else {
            Credential::Script(self.hash28())
        }
    }

    /// All four `DRep` variants.
    ///
    /// #948 lived here: `encode_drep` emitted a 32-byte DRep KeyHash where CDDL
    /// `drep = [0, addr_keyhash]` wants `bstr(28)`, while `read_drep` rejects
    /// any width but 28 — dugite's own output was self-undecodable. Two
    /// existing tests PINNED the bug by asserting the wrong shape.
    pub fn drep(&mut self) -> DRep {
        match self.choice(4) {
            // CDDL `drep = [0, addr_keyhash]` — bstr(28). `read_drep` builds
            // the value as `read_hash28_cert()?.to_hash32_padded()` and rejects
            // any width but 28, so the in-memory Hash32 is always zero-padded.
            // Generating a full-width hash would come back truncated. #948 was
            // the encoder writing 32 here — self-undecodable output.
            0 => DRep::KeyHash(self.hash28().to_hash32_padded()),
            1 => DRep::ScriptHash(self.hash28()),
            2 => DRep::Abstain,
            _ => DRep::NoConfidence,
        }
    }

    /// All three `Voter` discriminators, over both credential types — five
    /// distinct wire tags. #932's `encode_voter` StakePool arm emitted 32 bytes
    /// where `voter = [4, pool_keyhash]` wants `bstr(28)`.
    pub fn voter(&mut self) -> Voter {
        match self.choice(3) {
            0 => Voter::ConstitutionalCommittee(self.credential()),
            1 => Voter::DRep(self.credential()),
            // `voter = [4, pool_keyhash]` — bstr(28), zero-padded in memory.
            // #932's `encode_voter` StakePool arm emitted 32 here.
            _ => Voter::StakePool(self.hash28().to_hash32_padded()),
        }
    }

    pub fn vote(&mut self) -> Vote {
        match self.choice(3) {
            0 => Vote::No,
            1 => Vote::Yes,
            _ => Vote::Abstain,
        }
    }

    pub fn anchor(&mut self) -> Anchor {
        Anchor {
            url: String::from_utf8_lossy(&self.bytes(64)).into_owned(),
            data_hash: self.hash32(),
        }
    }

    pub fn gov_action_id(&mut self) -> GovActionId {
        GovActionId {
            transaction_id: self.hash32(),
            action_index: self.u32(),
        }
    }

    fn maybe_gov_action_id(&mut self) -> Option<GovActionId> {
        self.chance(180).then(|| self.gov_action_id())
    }

    /// A reward account — 29 bytes on the wire (1 header + 28 hash).
    pub fn reward_account(&mut self) -> Vec<u8> {
        let mut acct = vec![0xe0 | (self.byte() & 0x1f)];
        acct.extend(self.hash28().as_bytes());
        acct
    }

    /// Every `ProtocolParamUpdate` key, 0 through 37.
    ///
    /// This is the shape #951 lived in. The PPU key 26 encoder wrote the ten
    /// `drep_voting_thresholds` elements in the WRONG ORDER — it dropped
    /// `constitution` from index 3, shifted six up, and appended it at index 9
    /// where Haskell puts `treasuryWithdrawal`. The DECODER was always right,
    /// so a dugite-built `ParameterChange` installed the wrong governance
    /// thresholds: the very values that decide whether actions pass.
    ///
    /// Every field is populated with high probability, because the defect is
    /// only observable when the fields around it are populated too — a sparse
    /// map with one threshold set cannot reveal a permutation.
    pub fn ppu(&mut self) -> ProtocolParamUpdate {
        self.ppu_for(PpuShape::Conway)
    }

    /// A `ProtocolParamUpdate` restricted to one era's key set.
    ///
    /// The two wire types genuinely differ, and generating a field the target
    /// encoder is not specified to emit would produce a permanent false
    /// positive rather than a finding:
    ///
    /// - keys 12-15 (`d`, `extra_entropy`, `protocol_version`,
    ///   `min_utxo_value`) exist only Shelley..Babbage — Conway drops
    ///   decentralisation and extra entropy, moves protocol_version into the
    ///   HardForkInitiation action, and replaces min_utxo_value with
    ///   coinsPerUTxOByte at key 17
    /// - keys 25-33 (the voting thresholds and governance parameters) and
    ///   34-37 (the Dijkstra ref-script parameters) do not exist pre-Conway
    pub fn ppu_for(&mut self, shape: PpuShape) -> ProtocolParamUpdate {
        // Populate nearly always: the point is a dense map, not a realistic one.
        macro_rules! opt {
            ($self:ident, $e:expr) => {
                if $self.chance(230) {
                    Some($e)
                } else {
                    None
                }
            };
        }

        let pre = shape == PpuShape::PreConway;
        let conway = shape == PpuShape::Conway;

        // Keys 25 and 26 are POSITIONAL ARRAYS — five pool thresholds and ten
        // DRep thresholds — not fifteen independent map keys. A partial group
        // is not representable: the encoder writes the whole array or nothing,
        // so generating three-of-ten would manufacture a false positive rather
        // than a finding.
        //
        // That atomicity is exactly what makes key 26 the #951 shape. All ten
        // values are drawn INDEPENDENTLY so any permutation between encoder
        // and decoder surfaces as two fields swapping values.
        let mut drep_group: [Option<Rational>; 10] = Default::default();
        if conway && self.chance(230) {
            for slot in drep_group.iter_mut() {
                *slot = Some(self.rational());
            }
        }
        let mut pool_group: [Option<Rational>; 5] = Default::default();
        if conway && self.chance(230) {
            for slot in pool_group.iter_mut() {
                *slot = Some(self.rational());
            }
        }
        // Both halves of key 14 together, or neither.
        let pv = if pre && self.chance(230) {
            Some(self.u64())
        } else {
            None
        };

        ProtocolParamUpdate {
            min_fee_a: opt!(self, self.u64()),
            min_fee_b: opt!(self, self.u64()),
            max_block_body_size: opt!(self, self.u64()),
            max_tx_size: opt!(self, self.u64()),
            max_block_header_size: opt!(self, self.u64()),
            key_deposit: opt!(self, self.lovelace()),
            pool_deposit: opt!(self, self.lovelace()),
            e_max: opt!(self, self.u64()),
            n_opt: opt!(self, self.u64()),
            a0: opt!(self, self.rational()),
            rho: opt!(self, self.rational()),
            tau: opt!(self, self.rational()),
            min_pool_cost: opt!(self, self.lovelace()),
            ada_per_utxo_byte: opt!(self, self.lovelace()),
            // Key 15 — decoded then DROPPED before #919. Key 17 is
            // coinsPerUTxOWord pre-Babbage and coinsPerUTxOByte after.
            min_utxo_value: pre.then(|| opt!(self, self.lovelace())).flatten(),
            cost_models: opt!(self, self.cost_models()),
            execution_costs: opt!(
                self,
                ExUnitPrices {
                    mem_price: self.rational(),
                    step_price: self.rational(),
                }
            ),
            max_tx_ex_units: opt!(self, self.ex_units()),
            max_block_ex_units: opt!(self, self.ex_units()),
            max_val_size: opt!(self, self.u64()),
            collateral_percentage: opt!(self, self.u64()),
            max_collateral_inputs: opt!(self, self.u64()),
            min_fee_ref_script_cost_per_byte: conway.then(|| opt!(self, self.rational())).flatten(),
            d: pre.then(|| opt!(self, self.rational())).flatten(),
            extra_entropy: pre.then(|| opt!(self, self.hash32())).flatten(),
            // protocol_version is ONE wire key carrying [major, minor], so the
            // two halves must be present or absent together — a lone major
            // cannot be represented and would be a false positive.
            protocol_version_major: pv,
            protocol_version_minor: pv,
            drep_deposit: conway.then(|| opt!(self, self.lovelace())).flatten(),
            gov_action_deposit: conway.then(|| opt!(self, self.lovelace())).flatten(),
            gov_action_lifetime: conway.then(|| opt!(self, self.u64())).flatten(),
            // Key 26 — the ten DRep voting thresholds, in the order Haskell's
            // `EncCBOR DRepVotingThresholds` writes them. All ten are drawn
            // independently so a permutation is observable.
            dvt_pp_network_group: drep_group[0].clone(),
            dvt_pp_economic_group: drep_group[1].clone(),
            dvt_pp_technical_group: drep_group[2].clone(),
            dvt_pp_gov_group: drep_group[3].clone(),
            dvt_hard_fork: drep_group[4].clone(),
            dvt_no_confidence: drep_group[5].clone(),
            dvt_committee_normal: drep_group[6].clone(),
            dvt_committee_no_confidence: drep_group[7].clone(),
            dvt_constitution: drep_group[8].clone(),
            dvt_treasury_withdrawal: drep_group[9].clone(),
            // Key 25 — the five pool thresholds. Verified CORRECT during the
            // #951 audit; generated anyway so a future regression is caught.
            pvt_motion_no_confidence: pool_group[0].clone(),
            pvt_committee_normal: pool_group[1].clone(),
            pvt_committee_no_confidence: pool_group[2].clone(),
            pvt_hard_fork: pool_group[3].clone(),
            pvt_pp_security_group: pool_group[4].clone(),
            min_committee_size: conway.then(|| opt!(self, self.u64())).flatten(),
            committee_term_limit: conway.then(|| opt!(self, self.u64())).flatten(),
            drep_activity: conway.then(|| opt!(self, self.u64())).flatten(),
            // Dijkstra keys 34-37.
            max_ref_script_size_per_block: conway.then(|| opt!(self, self.u32())).flatten(),
            max_ref_script_size_per_tx: conway.then(|| opt!(self, self.u32())).flatten(),
            // NonZero Word32 upstream.
            ref_script_cost_stride: conway.then(|| opt!(self, self.u32().max(1))).flatten(),
            ref_script_cost_multiplier: conway.then(|| opt!(self, self.rational())).flatten(),
        }
    }

    pub fn ex_units(&mut self) -> ExUnits {
        ExUnits {
            mem: self.coin(),
            steps: self.coin(),
        }
    }

    pub fn cost_models(&mut self) -> CostModels {
        let model = |g: &mut Self| {
            let len = g.collection_len(40);
            (0..len).map(|_| g.u64() as i64).collect::<Vec<i64>>()
        };
        let mut unknown_cost_models = BTreeMap::new();
        // Language keys >= 4 only: 0-3 land in the typed fields, and the
        // decoder guarantees that split. Latent on-chain today (#770), carried
        // for byte-exact completeness — so exactly the sort of field a
        // decode-first target can never reach.
        let unknown_len = self.collection_len(6);
        for _ in 0..unknown_len {
            let key = 4u8.saturating_add(self.byte() % 32);
            let costs = model(self);
            unknown_cost_models.insert(key, costs);
        }
        CostModels {
            plutus_v1: self.chance(200).then(|| model(self)),
            plutus_v2: self.chance(200).then(|| model(self)),
            plutus_v3: self.chance(200).then(|| model(self)),
            plutus_v4: self.chance(128).then(|| model(self)),
            unknown_cost_models,
        }
    }

    /// All three `Relay` variants, each with its optional fields exercised.
    pub fn relay(&mut self) -> Relay {
        let port = self.chance(180).then(|| self.u32() as u16);
        match self.choice(3) {
            0 => Relay::SingleHostAddr {
                port,
                ipv4: self.chance(180).then(|| {
                    let mut a = [0u8; 4];
                    for b in a.iter_mut() {
                        *b = self.byte();
                    }
                    a
                }),
                ipv6: self.chance(128).then(|| {
                    let mut a = [0u8; 16];
                    for b in a.iter_mut() {
                        *b = self.byte();
                    }
                    a
                }),
            },
            1 => Relay::SingleHostName {
                port,
                dns_name: String::from_utf8_lossy(&self.bytes(32)).into_owned(),
            },
            _ => Relay::MultiHostName {
                dns_name: String::from_utf8_lossy(&self.bytes(32)).into_owned(),
            },
        }
    }

    /// All seven `GovAction` variants.
    pub fn gov_action(&mut self) -> GovAction {
        match self.choice(7) {
            0 => GovAction::ParameterChange {
                prev_action_id: self.maybe_gov_action_id(),
                protocol_param_update: Box::new(self.ppu()),
                policy_hash: self.chance(128).then(|| self.hash28()),
            },
            1 => GovAction::HardForkInitiation {
                prev_action_id: self.maybe_gov_action_id(),
                protocol_version: (self.u64(), self.u64()),
            },
            2 => {
                let len = self.collection_len(30);
                let mut withdrawals = BTreeMap::new();
                for _ in 0..len {
                    let account = self.reward_account();
                    let amount = self.lovelace();
                    withdrawals.insert(account, amount);
                }
                GovAction::TreasuryWithdrawals {
                    withdrawals,
                    policy_hash: self.chance(128).then(|| self.hash28()),
                }
            }
            3 => GovAction::NoConfidence {
                prev_action_id: self.maybe_gov_action_id(),
            },
            4 => {
                let remove_len = self.collection_len(24);
                // `Set (Credential ColdCommitteeRole)` — duplicates rejected.
                let members_to_remove = dedup_preserving_order(
                    (0..remove_len)
                        .map(|_| self.credential())
                        .collect::<Vec<_>>(),
                );
                let add_len = self.collection_len(24);
                let mut members_to_add = BTreeMap::new();
                for _ in 0..add_len {
                    let cred = self.credential();
                    let epoch = self.u64();
                    members_to_add.insert(cred, epoch);
                }
                GovAction::UpdateCommittee {
                    prev_action_id: self.maybe_gov_action_id(),
                    members_to_remove,
                    members_to_add,
                    threshold: self.rational(),
                }
            }
            5 => GovAction::NewConstitution {
                prev_action_id: self.maybe_gov_action_id(),
                constitution: Constitution {
                    anchor: self.anchor(),
                    script_hash: self.chance(128).then(|| self.hash28()),
                },
            },
            _ => GovAction::InfoAction,
        }
    }

    pub fn proposal_procedure(&mut self) -> ProposalProcedure {
        ProposalProcedure {
            deposit: self.lovelace(),
            return_addr: self.reward_account(),
            gov_action: self.gov_action(),
            anchor: self.anchor(),
        }
    }

    pub fn voting_procedure(&mut self) -> VotingProcedure {
        VotingProcedure {
            vote: self.vote(),
            anchor: self.chance(160).then(|| self.anchor()),
        }
    }

    pub fn pool_params(&mut self) -> PoolParams {
        let owner_len = self.collection_len(8);
        let relay_len = self.collection_len(4);
        PoolParams {
            operator: self.hash28(),
            vrf_keyhash: self.hash32(),
            pledge: self.lovelace(),
            cost: self.lovelace(),
            margin: self.rational(),
            reward_account: self.reward_account(),
            // `pool_owners` is `Set (KeyHash Staking)` — duplicates are
            // rejected at decode, so dedup keeps a low-entropy input from
            // reading as an encoder defect.
            pool_owners: dedup_preserving_order((0..owner_len).map(|_| self.hash28()).collect()),
            relays: (0..relay_len).map(|_| self.relay()).collect(),
            pool_metadata: self.chance(160).then(|| PoolMetadata {
                url: String::from_utf8_lossy(&self.bytes(64)).into_owned(),
                hash: self.hash32(),
            }),
        }
    }
}

impl Gen<'_> {
    /// All 19 `Certificate` variants, round-robin over the discriminant.
    ///
    /// The Conway governance certificates are where #948 lived: `encode_drep`
    /// emitted a 32-byte DRep KeyHash that `read_drep` (which demands 28)
    /// rejects, making dugite's output self-undecodable. It reached every DRep
    /// delegation certificate — `VoteDelegation`, `StakeVoteDelegation`,
    /// `RegStakeVoteDeleg`, `VoteRegDeleg` — and no on-chain corpus in this
    /// repo contains one.
    ///
    /// `choice` covers the full range so no variant is unreachable; a byte
    /// mutator flipping one byte moves between adjacent variants.
    pub fn certificate(&mut self) -> Certificate {
        self.certificate_for(Era::Conway)
    }

    /// A certificate valid for `era`.
    ///
    /// Pre-Conway decoders accept wire tags 0-6 only; Conway adds 7-18 (the
    /// CIP-1694 governance certificates). Generating a Conway certificate in a
    /// Babbage body is rejected at decode — a false positive, not a finding.
    ///
    /// The variant indices below are this generator's own; the mapping to wire
    /// tags is in `encode_certificate`.
    pub fn certificate_for(&mut self, era: Era) -> Certificate {
        // Indices whose wire tag is <= 6, i.e. representable before Conway.
        const PRE_CONWAY_VARIANTS: [u8; 7] = [0, 1, 4, 5, 6, 17, 18];
        let index = if era >= Era::Conway {
            self.choice(19)
        } else {
            PRE_CONWAY_VARIANTS[(self.byte() as usize) % PRE_CONWAY_VARIANTS.len()]
        };
        match index {
            0 => Certificate::StakeRegistration(self.credential()),
            1 => Certificate::StakeDeregistration(self.credential()),
            2 => Certificate::ConwayStakeRegistration {
                credential: self.credential(),
                deposit: self.lovelace(),
            },
            3 => Certificate::ConwayStakeDeregistration {
                credential: self.credential(),
                refund: self.lovelace(),
            },
            4 => Certificate::StakeDelegation {
                credential: self.credential(),
                pool_hash: self.hash28(),
            },
            5 => Certificate::PoolRegistration(self.pool_params()),
            6 => Certificate::PoolRetirement {
                pool_hash: self.hash28(),
                epoch: self.u64(),
            },
            7 => Certificate::RegDRep {
                credential: self.credential(),
                deposit: self.lovelace(),
                anchor: self.chance(160).then(|| self.anchor()),
            },
            8 => Certificate::UnregDRep {
                credential: self.credential(),
                refund: self.lovelace(),
            },
            9 => Certificate::UpdateDRep {
                credential: self.credential(),
                anchor: self.chance(160).then(|| self.anchor()),
            },
            10 => Certificate::VoteDelegation {
                credential: self.credential(),
                drep: self.drep(),
            },
            11 => Certificate::StakeVoteDelegation {
                credential: self.credential(),
                pool_hash: self.hash28(),
                drep: self.drep(),
            },
            12 => Certificate::RegStakeDeleg {
                credential: self.credential(),
                pool_hash: self.hash28(),
                deposit: self.lovelace(),
            },
            13 => Certificate::CommitteeHotAuth {
                cold_credential: self.credential(),
                hot_credential: self.credential(),
            },
            14 => Certificate::CommitteeColdResign {
                cold_credential: self.credential(),
                anchor: self.chance(160).then(|| self.anchor()),
            },
            15 => Certificate::RegStakeVoteDeleg {
                credential: self.credential(),
                pool_hash: self.hash28(),
                drep: self.drep(),
                deposit: self.lovelace(),
            },
            16 => Certificate::VoteRegDeleg {
                credential: self.credential(),
                drep: self.drep(),
                deposit: self.lovelace(),
            },
            17 => Certificate::GenesisKeyDelegation {
                // `genesis_key_delegation = (5, genesishash, genesis_delegate_hash,
                // vrf_keyhash)`. The first two are blake2b_224 (28 bytes,
                // zero-padded in memory); only vrf_keyhash is a genuine 32.
                genesis_hash: self.hash28().to_hash32_padded(),
                genesis_delegate_hash: self.hash28().to_hash32_padded(),
                vrf_keyhash: self.hash32(),
            },
            _ => {
                let source = if self.bool() {
                    MIRSource::Reserves
                } else {
                    MIRSource::Treasury
                };
                let target = if self.bool() {
                    let len = self.collection_len(24);
                    MIRTarget::StakeCredentials(
                        (0..len)
                            .map(|_| (self.credential(), self.u64() as i64))
                            .collect(),
                    )
                } else {
                    MIRTarget::OtherAccountingPot(self.coin())
                };
                Certificate::MoveInstantaneousRewards { source, target }
            }
        }
    }
}

/// Binary GCD, matching the reduction `Reader::read_rational` applies.
fn gcd(a: u64, b: u64) -> u64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

impl Gen<'_> {
    pub fn address(&mut self) -> Address {
        let network = if self.bool() {
            NetworkId::Mainnet
        } else {
            NetworkId::Testnet
        };
        // Byron addresses are deliberately excluded: `encode_transaction` has
        // no Byron encoder (see FIRST_ENCODABLE_ERA in encode_roundtrip.rs), so
        // generating one would be a permanent false positive, not a finding.
        match self.choice(3) {
            0 => Address::Base(BaseAddress {
                network,
                payment: self.credential(),
                stake: self.credential(),
            }),
            1 => Address::Enterprise(EnterpriseAddress {
                network,
                payment: self.credential(),
            }),
            _ => Address::Reward(RewardAddress {
                network,
                stake: self.credential(),
            }),
        }
    }

    /// A `Value`, with the multi-asset map straddling the `encodeMap`
    /// boundaries at BOTH levels.
    ///
    /// #930 lived exactly here: `encode_multi_asset` emitted a definite header
    /// above 23 entries where Haskell's `encodeMap` switches to the indefinite
    /// form, over-counting by one byte at >= 256 entries. On preprod tx
    /// `96ae78f7...` (a 324-entry asset map) that measured 5001 against
    /// `maxValSize=5000` — a false Phase-1 reject.
    pub fn value(&mut self, asset_cap: usize) -> Value {
        let mut multi_asset: MultiAsset = BTreeMap::new();
        let policies = self.collection_len(asset_cap.min(8));
        for _ in 0..policies {
            let policy = self.hash28();
            let assets = self.collection_len(asset_cap);
            let mut inner: BTreeMap<AssetName, u64> = BTreeMap::new();
            for _ in 0..assets {
                // A zero quantity is rejected ("MultiAsset cannot contain
                // zeros") — the ledger's canonical-form rule, not an encoder
                // choice.
                let quantity = self.coin().max(1);
                inner.insert(AssetName(self.bytes(32)), quantity);
            }
            // The decoder rejects an empty asset map under a policy id
            // ("Empty Assets are not allowed"), matching the ledger. Emitting
            // one would be a false positive against the encoder.
            if !inner.is_empty() {
                multi_asset.insert(policy, inner);
            }
        }
        Value {
            coin: self.lovelace(),
            multi_asset,
        }
    }

    pub fn plutus_data(&mut self, depth: u8) -> PlutusData {
        // Bound recursion: the encoder is recursive and a deep tree costs far
        // more per iteration than it buys in coverage.
        if depth == 0 {
            return match self.choice(2) {
                0 => PlutusData::Integer(self.boundary_bigint()),
                _ => PlutusData::Bytes(self.bytes(64)),
            };
        }
        match self.choice(5) {
            0 => {
                let n = self.collection_len(6);
                PlutusData::Constr(
                    self.u64(),
                    (0..n).map(|_| self.plutus_data(depth - 1)).collect(),
                )
            }
            1 => {
                let n = self.collection_len(6);
                PlutusData::Map(
                    (0..n)
                        .map(|_| (self.plutus_data(depth - 1), self.plutus_data(depth - 1)))
                        .collect(),
                )
            }
            2 => {
                let n = self.collection_len(6);
                PlutusData::List((0..n).map(|_| self.plutus_data(depth - 1)).collect())
            }
            3 => PlutusData::Integer(self.boundary_bigint()),
            _ => PlutusData::Bytes(self.bytes(64)),
        }
    }

    pub fn native_script(&mut self, depth: u8) -> NativeScript {
        if depth == 0 {
            // `script_pubkey = (0, addr_keyhash)` — bstr(28), zero-padded in
            // memory like every other 28-byte hash field.
            return NativeScript::ScriptPubkey(self.hash28().to_hash32_padded());
        }
        let children = |g: &mut Self| {
            let n = g.collection_len(4);
            (0..n)
                .map(|_| g.native_script(depth - 1))
                .collect::<Vec<_>>()
        };
        match self.choice(6) {
            0 => NativeScript::ScriptPubkey(self.hash28().to_hash32_padded()),
            1 => NativeScript::ScriptAll(children(self)),
            2 => NativeScript::ScriptAny(children(self)),
            3 => NativeScript::ScriptNOfK(self.u32(), children(self)),
            4 => NativeScript::InvalidBefore(SlotNo(self.u64())),
            _ => NativeScript::InvalidHereafter(SlotNo(self.u64())),
        }
    }

    pub fn metadatum(&mut self, depth: u8) -> TransactionMetadatum {
        if depth == 0 {
            return match self.choice(3) {
                0 => TransactionMetadatum::Int(self.u64() as i128),
                1 => TransactionMetadatum::Bytes(self.bytes(64)),
                _ => TransactionMetadatum::Text(
                    String::from_utf8_lossy(&self.bytes(64)).into_owned(),
                ),
            };
        }
        match self.choice(5) {
            0 => {
                let n = self.collection_len(6);
                TransactionMetadatum::Map(
                    (0..n)
                        .map(|_| (self.metadatum(depth - 1), self.metadatum(depth - 1)))
                        .collect(),
                )
            }
            1 => {
                let n = self.collection_len(6);
                TransactionMetadatum::List((0..n).map(|_| self.metadatum(depth - 1)).collect())
            }
            2 => TransactionMetadatum::Int(self.u64() as i128),
            3 => TransactionMetadatum::Bytes(self.bytes(64)),
            _ => TransactionMetadatum::Text(String::from_utf8_lossy(&self.bytes(64)).into_owned()),
        }
    }

    pub fn tx_input(&mut self) -> TransactionInput {
        TransactionInput {
            transaction_id: self.hash32(),
            index: self.u32(),
        }
    }

    pub fn tx_output(&mut self) -> TransactionOutput {
        self.tx_output_for(true)
    }

    /// A transaction output. `allow_map_form` is false before Babbage, where
    /// the post-Alonzo map encoding (and therefore inline datums and script
    /// references) does not exist.
    pub fn tx_output_for(&mut self, allow_map_form: bool) -> TransactionOutput {
        // The two output encodings are not interchangeable, and `is_legacy`
        // selects between them:
        //
        //   legacy (Shelley array): [address, value] or [address, value, datum_hash]
        //   post-Alonzo (map):      {0: address, 1: value, 2: datum, 3: script_ref}
        //
        // The legacy form has no slot for a script_ref or an inline datum, so a
        // legacy output carrying either is not representable — the encoder
        // drops it and the round-trip reports a difference the encoder did not
        // cause. Conway bodies do still contain legacy outputs (simple change
        // outputs), so both shapes are generated; they are just kept coherent.
        let is_legacy = !allow_map_form || self.bool();

        let datum = if is_legacy {
            match self.choice(2) {
                0 => OutputDatum::None,
                _ => OutputDatum::DatumHash(self.hash32()),
            }
        } else {
            match self.choice(3) {
                0 => OutputDatum::None,
                1 => OutputDatum::DatumHash(self.hash32()),
                _ => OutputDatum::InlineDatum {
                    data: self.plutus_data(2),
                    raw_cbor: None,
                },
            }
        };

        let script_ref = (!is_legacy && self.chance(100)).then(|| match self.choice(5) {
            0 => ScriptRef::NativeScript(self.native_script(2)),
            1 => ScriptRef::PlutusV1(self.bytes(64)),
            2 => ScriptRef::PlutusV2(self.bytes(64)),
            3 => ScriptRef::PlutusV3(self.bytes(64)),
            // PlutusV4 (Dijkstra, issue #1000) — wire tag 4. Structurally
            // decodable under any era (the decoder doesn't era-gate this
            // arm; PV-gating happens one layer up, at Phase-1/Phase-2
            // validation via `ledger_language_introduced_in`), so it's
            // safe to generate regardless of which era this output lands
            // in — same as the pre-existing V1/V2/V3 arms above.
            _ => ScriptRef::PlutusV4(self.bytes(64)),
        });

        TransactionOutput {
            address: self.address(),
            value: self.value(24),
            datum,
            script_ref,
            is_legacy,
            raw_cbor: None,
        }
    }

    /// A witness set, including the bootstrap witnesses that #939 turned on.
    ///
    /// Conway witness keys 0/1/2/3/6/7 need CBOR tag 258 from PV9
    /// (`encodeWithSetTag`) — omitted entirely before #939. Bootstrap
    /// witnesses (key 2) sort by the Byron address-root hash, NOT `WitVKey`'s
    /// blake2b224(vkey), and appear in no fixture in this repo.
    pub fn witness_set(&mut self) -> TransactionWitnessSet {
        self.witness_set_for(Era::Conway)
    }

    /// A witness set valid for `era`.
    ///
    /// The witness-set key space grew with the eras and the decoders hard-reject
    /// an out-of-era key:
    ///
    ///   Shelley   keys 0-2   (vkey, native scripts, bootstrap)
    ///   Alonzo    + 3 (plutus_v1), 4 (plutus_data), 5 (redeemers)
    ///   Babbage   + 6 (plutus_v2)
    ///   Conway    + 7 (plutus_v3)
    pub fn witness_set_for(&mut self, era: Era) -> TransactionWitnessSet {
        let has_plutus_v1 = era >= Era::Alonzo;
        let has_plutus_v2 = era >= Era::Babbage;
        let has_plutus_v3 = era >= Era::Conway;
        let vkeys = self.collection_len(24);
        let boots = self.collection_len(8);
        let natives = self.collection_len(8);
        let datums = self.collection_len(8);
        let redeemers = self.collection_len(8);
        // From Conway on, redeemers are a MAP keyed by `[tag, index]`, so two
        // redeemers sharing a key are not representable — the second silently
        // replaces the first. (The pre-Conway list form does allow duplicates;
        // that is what the `babbage_dup_redeemer_tx_572a9da4` fixture pins.)
        // Generating a collision would look like an encoder dropping data.
        let mut redeemer_set: Vec<Redeemer> = Vec::new();
        for _ in 0..redeemers {
            let redeemer = Redeemer {
                tag: match self.choice(6) {
                    0 => RedeemerTag::Spend,
                    1 => RedeemerTag::Mint,
                    2 => RedeemerTag::Cert,
                    3 => RedeemerTag::Reward,
                    4 => RedeemerTag::Vote,
                    _ => RedeemerTag::Propose,
                },
                index: self.u32(),
                data: self.plutus_data(2),
                ex_units: self.ex_units(),
            };
            if !redeemer_set
                .iter()
                .any(|r| r.tag == redeemer.tag && r.index == redeemer.index)
            {
                redeemer_set.push(redeemer);
            }
        }

        let script_list = |g: &mut Self, cap: usize| {
            let n = g.collection_len(cap);
            dedup_preserving_order((0..n).map(|_| g.bytes(64)).collect::<Vec<_>>())
        };
        TransactionWitnessSet {
            // Every witness-set collection is a CBOR `Set` from Conway on
            // (keys 0/1/2/3/4/6/7 carry tag 258 — #939), and the decoder
            // enforces uniqueness. A low-entropy input makes every generated
            // item identical, so dedup is what keeps that from reading as an
            // encoder defect.
            vkey_witnesses: dedup_preserving_order(
                (0..vkeys)
                    .map(|_| VKeyWitness {
                        vkey: self.bytes(32),
                        signature: self.bytes(64),
                    })
                    .collect(),
            ),
            native_scripts: dedup_preserving_order(
                (0..natives).map(|_| self.native_script(2)).collect(),
            ),
            bootstrap_witnesses: dedup_preserving_order(
                (0..boots)
                    .map(|_| BootstrapWitness {
                        vkey: self.bytes(32),
                        signature: self.bytes(64),
                        chain_code: self.bytes(32),
                        attributes: self.bytes(32),
                    })
                    .collect(),
            ),
            plutus_v1_scripts: if has_plutus_v1 {
                script_list(self, 4)
            } else {
                Vec::new()
            },
            plutus_v2_scripts: if has_plutus_v2 {
                script_list(self, 4)
            } else {
                Vec::new()
            },
            plutus_v3_scripts: if has_plutus_v3 {
                script_list(self, 4)
            } else {
                Vec::new()
            },
            plutus_data: if has_plutus_v1 {
                dedup_preserving_order((0..datums).map(|_| self.plutus_data(2)).collect())
            } else {
                Vec::new()
            },
            redeemers: if has_plutus_v1 {
                redeemer_set
            } else {
                Vec::new()
            },
            raw_redeemers_cbor: None,
            raw_plutus_data_cbor: None,
            original_script_data_hash: None,
        }
    }

    pub fn auxiliary_data(&mut self) -> AuxiliaryData {
        self.auxiliary_data_for(Era::Conway)
    }

    /// Auxiliary data for `era`.
    ///
    /// Scripts are generated for every era: #984 consolidated the three
    /// auxiliary-data decoders, which each populated a different subset (one
    /// had no `tag(259)` arm at all, so dugite's own encoder output decoded to
    /// entirely empty auxiliary data). Before that fix this generator had to
    /// exclude them from pre-Conway eras to avoid a permanent false positive.
    pub fn auxiliary_data_for(&mut self, _era: Era) -> AuxiliaryData {
        let labels = self.collection_len(8);
        let natives = self.collection_len(4);
        let mut metadata = BTreeMap::new();
        for _ in 0..labels {
            metadata.insert(self.u64(), self.metadatum(2));
        }
        AuxiliaryData {
            metadata,
            native_scripts: (0..natives).map(|_| self.native_script(2)).collect(),
            plutus_v1_scripts: Vec::new(),
            plutus_v2_scripts: Vec::new(),
            plutus_v3_scripts: Vec::new(),
            raw_cbor: None,
        }
    }

    /// A whole transaction for `era`, populating every body field the era's
    /// wire type actually carries.
    ///
    /// Era-awareness is the point, not a nicety. Leaving the era-specific
    /// fields empty is what let tx-body key 6 (`update`) sit with NO encoder
    /// arm at all while three decoders populated it — a pre-Conway transaction
    /// carrying a param update re-encoded into a body missing key 6, changing
    /// the transaction id. The Dijkstra-only fields (`sub_transactions`,
    /// `account_balance_intervals`, `direct_deposits`, `guards`) were in the
    /// same position: decoded, never generated, never checked.
    pub fn transaction(&mut self, era: Era) -> Transaction {
        let pre_conway = era < Era::Conway;
        let dijkstra = era >= Era::Dijkstra;

        // The per-era tx-body key matrix, taken from what the decoders actually
        // ACCEPT (they hard-reject an out-of-era key, per upstream's per-era
        // `SparseKeyed` `bodyFields` catch-all). Generating a field the target
        // era cannot carry is a false positive, not a finding — a Shelley body
        // with key 8 is rejected by the Shelley decoder, and correctly so.
        //
        //   Shelley                  keys 0-7
        //   Allegra / Mary / Alonzo  + 8 (validity_interval_start), 9 (mint)
        //   Alonzo only              + 11, 13, 14, 15
        //   Babbage                  + 11, 13-18
        //   Conway+                  + 19-22; key 6 (update) is GONE
        //   Dijkstra                 + 23, 25, 26 and key-14 guards
        let has_validity_start = era >= Era::Allegra;
        let has_mint = era >= Era::Allegra;
        let has_alonzo_keys = era >= Era::Alonzo; // 11, 13, 14, 15
        let has_babbage_keys = era >= Era::Babbage; // 16, 17, 18
        let has_conway_gov = era >= Era::Conway; // 19-22
        let inputs = self.collection_len(30);
        let outputs = self.collection_len(24);
        let certs = self.collection_len(19);
        let collateral = self.collection_len(8);
        let ref_inputs = self.collection_len(8);
        let signers = self.collection_len(24);
        let withdrawal_count = self.collection_len(24);
        let proposals = self.collection_len(6);
        let voters = self.collection_len(6);
        let mint_policies = self.collection_len(6);

        let mut withdrawals = BTreeMap::new();
        for _ in 0..withdrawal_count {
            withdrawals.insert(self.reward_account(), self.lovelace());
        }

        let mut mint: BTreeMap<PolicyId, BTreeMap<AssetName, i64>> = BTreeMap::new();
        for _ in 0..mint_policies {
            let policy = self.hash28();
            let assets = self.collection_len(24);
            let mut inner: BTreeMap<AssetName, i64> = BTreeMap::new();
            for _ in 0..assets {
                // Mint quantities are signed: a burn is negative. Both signs
                // matter to the encoder's integer path. Zero is not
                // representable ("MultiAsset cannot contain zeros").
                let quantity = match self.u64() as i64 {
                    0 => 1,
                    other => other,
                };
                inner.insert(AssetName(self.bytes(32)), quantity);
            }
            // Empty asset maps are rejected by the decoder, as in the ledger.
            if !inner.is_empty() {
                mint.insert(policy, inner);
            }
        }

        let mut voting_procedures: BTreeMap<Voter, BTreeMap<GovActionId, VotingProcedure>> =
            BTreeMap::new();
        for _ in 0..voters {
            let voter = self.voter();
            let n = self.collection_len(4);
            let mut inner = BTreeMap::new();
            for _ in 0..n {
                inner.insert(self.gov_action_id(), self.voting_procedure());
            }
            voting_procedures.insert(voter, inner);
        }

        // Conway `Set`-typed body fields are decoded with EnforceNoDuplicates,
        // so a duplicate is rejected at the strict-set layer before Phase-1
        // ever runs (#925). Generating one would be a permanent false positive:
        // the encoder is not what rejects it.
        let mut input_set: Vec<TransactionInput> = (0..inputs).map(|_| self.tx_input()).collect();
        input_set.sort();
        input_set.dedup();
        let mut collateral_set: Vec<TransactionInput> =
            (0..collateral).map(|_| self.tx_input()).collect();
        collateral_set.sort();
        collateral_set.dedup();
        let mut ref_input_set: Vec<TransactionInput> =
            (0..ref_inputs).map(|_| self.tx_input()).collect();
        ref_input_set.sort();
        ref_input_set.dedup();
        // Only the low 28 bytes of a `required_signers` entry reach the wire
        // (`addr_keyhash` is bstr(28)); the decoder pads back to Hash32. A
        // generated hash with non-zero high bytes would come back truncated —
        // the fixture's fault, not the encoder's.
        let mut signer_set: Vec<Hash32> = (0..signers)
            .map(|_| self.hash28().to_hash32_padded())
            .collect();
        signer_set.sort();
        signer_set.dedup();

        // `certificates` and `proposal_procedures` are OSet: still sets (tag 258
        // is UNCONDITIONAL upstream, no PV gate), so duplicates are rejected —
        // but ORDER IS LOAD-BEARING, since a registration must precede the
        // delegation that uses it. Running them through a sorting dedup was
        // #940. Dedup here keeps first occurrence and preserves order.
        let mut cert_set: Vec<Certificate> = Vec::new();
        for _ in 0..certs {
            let cert = self.certificate_for(era);
            if !cert_set.contains(&cert) {
                cert_set.push(cert);
            }
        }
        let mut proposal_set: Vec<ProposalProcedure> = Vec::new();
        for _ in 0..proposals {
            let proposal = self.proposal_procedure();
            if !proposal_set.contains(&proposal) {
                proposal_set.push(proposal);
            }
        }

        // ── Dijkstra-only body fields (keys 23, 25, 26 and the key-14 guards)
        let mut sub_txs: Vec<SubTransaction> = Vec::new();
        let mut balance_intervals: Vec<(Credential, AccountBalanceInterval)> = Vec::new();
        let mut direct_deposits: BTreeMap<Vec<u8>, Lovelace> = BTreeMap::new();
        let mut guard_set: Vec<Credential> = Vec::new();
        if dijkstra {
            let n = self.collection_len(4);
            for _ in 0..n {
                sub_txs.push(self.sub_transaction());
            }
            // `sub_transactions` is an OMap keyed by TxId; duplicates are
            // rejected (`EnforceNoDuplicates`) and the decoder derives each key
            // from the sub-body's own bytes.
            sub_txs = dedup_preserving_order(sub_txs);

            let n = self.collection_len(8);
            for _ in 0..n {
                // At least one bound MUST be Some — the decoder rejects
                // `[null, null]`, so generating it would be a false positive.
                let (lower, upper) = match self.choice(3) {
                    0 => (Some(self.lovelace()), None),
                    1 => (None, Some(self.lovelace())),
                    _ => (Some(self.lovelace()), Some(self.lovelace())),
                };
                balance_intervals
                    .push((self.credential(), AccountBalanceInterval { lower, upper }));
            }
            // Keyed by credential on the wire, so duplicates collapse.
            let mut seen: Vec<Credential> = Vec::new();
            balance_intervals.retain(|(cred, _)| {
                if seen.contains(cred) {
                    false
                } else {
                    seen.push(cred.clone());
                    true
                }
            });

            let n = self.collection_len(8);
            for _ in 0..n {
                direct_deposits.insert(self.reward_account(), self.lovelace());
            }

            let n = self.collection_len(16);
            guard_set = dedup_preserving_order((0..n).map(|_| self.credential()).collect());
            guard_set.sort();
        }

        let body = TransactionBody {
            inputs: input_set,
            outputs: (0..outputs)
                .map(|_| self.tx_output_for(has_babbage_keys))
                .collect(),
            fee: self.lovelace(),
            ttl: self.chance(180).then(|| SlotNo(self.u64())),
            certificates: cert_set,
            withdrawals,
            auxiliary_data_hash: self.chance(180).then(|| self.hash32()),
            validity_interval_start: (has_validity_start && self.chance(180))
                .then(|| SlotNo(self.u64())),
            mint: if has_mint { mint } else { BTreeMap::new() },
            script_data_hash: (has_alonzo_keys && self.chance(180)).then(|| self.hash32()),
            collateral: if has_alonzo_keys {
                collateral_set
            } else {
                Vec::new()
            },
            // Key 14 has one wire slot. On Dijkstra the encoder composes it
            // from `guards`; generating both would make the projection
            // ambiguous, so only one is populated per era.
            required_signers: if dijkstra || !has_alonzo_keys {
                Vec::new()
            } else {
                signer_set
            },
            network_id: (has_alonzo_keys && self.chance(128)).then(|| self.byte() & 1),
            collateral_return: (has_babbage_keys && self.chance(128)).then(|| self.tx_output()),
            total_collateral: (has_babbage_keys && self.chance(128)).then(|| self.lovelace()),
            reference_inputs: if has_babbage_keys {
                ref_input_set
            } else {
                Vec::new()
            },
            // Body key 6 — pre-Conway only. Conway replaces the
            // genesis-delegate update mechanism with governance
            // proposal_procedures (key 20), so a Conway body must not carry it.
            update: (pre_conway && self.chance(180)).then(|| self.update_proposal()),
            voting_procedures: if has_conway_gov {
                voting_procedures
            } else {
                BTreeMap::new()
            },
            proposal_procedures: if has_conway_gov {
                proposal_set
            } else {
                Vec::new()
            },
            treasury_value: (has_conway_gov && self.chance(128)).then(|| self.lovelace()),
            donation: (has_conway_gov && self.chance(128)).then(|| self.lovelace()),
            sub_transactions: if dijkstra { sub_txs } else { Vec::new() },
            account_balance_intervals: if dijkstra {
                balance_intervals
            } else {
                Vec::new()
            },
            direct_deposits: if dijkstra {
                direct_deposits
            } else {
                BTreeMap::new()
            },
            // Body key 14 is `guards` from Dijkstra on and `required_signers`
            // before it. The decoder populates BOTH from the same wire set, so
            // only one of them is generated per era.
            guards: if dijkstra { guard_set } else { Vec::new() },
        };

        Transaction {
            hash: Default::default(),
            era,
            body,
            witness_set: self.witness_set_for(era),
            // `is_valid` is only on the wire for Alonzo..Conway.
            //
            // Before Alonzo the phase-2 validity flag does not exist — dugite's
            // standalone format keeps a byte for it, and the Shelley/Allegra/
            // Mary decoders read it and force `true`. From Dijkstra it is
            // removed again (CIP-0167: the standalone shape is array(3), and
            // validity is determined by phase-2 evaluation rather than
            // signalled by the author), so the encoder omits it and the decoder
            // defaults to `true`.
            //
            // Generating `false` outside that window fails the round-trip on a
            // field the era does not carry.
            is_valid: !(Era::Alonzo..=Era::Conway).contains(&era) || self.bool(),
            auxiliary_data: self.chance(180).then(|| self.auxiliary_data_for(era)),
            raw_cbor: None,
            raw_body_cbor: None,
            raw_witness_cbor: None,
        }
    }
}

/// Clear the "as received on the wire" caches, so P2 compares SEMANTICS
/// rather than bytes.
///
/// These are the only fields that may legitimately differ between a
/// wire-decoded transaction and one decoded from our own re-encoding, and they
/// are identifiable by a rule rather than by taste: every one caches the
/// ORIGINAL encoding so that a hash taken over the received bytes stays valid.
/// Canonicalising an indefinite-length array to a definite one on re-encode
/// changes them all, correctly.
///
/// Most are `#[serde(skip)]`, which is a good first filter but NOT the
/// definition — `OutputDatum::InlineDatum::raw_cbor` is deliberately kept in
/// bincode so LSM round-trips preserve it, and is still a wire-bytes cache.
/// The test is what the field holds, not how it is serialised.
///
/// `hash` belongs to the same family — blake2b-256 over the body bytes AS
/// RECEIVED. Byte-level agreement is covered by P3, and the canonical-input
/// case by P4.
///
/// Everything else is compared: all 24 body fields, the full witness set,
/// auxiliary data, sub-transactions, era and `is_valid`, via the `PartialEq`
/// the types already derive.
///
/// Getting this list WRONG in the safe direction (nulling something that is
/// not a cache) would silently weaken the target, which is the defect this
/// rewrite exists to remove. The rule above is what keeps it honest: if a new
/// field appears here, it must be `#[serde(skip)]` and documented as
/// preserving the wire encoding.
pub fn normalise_for_comparison(tx: &mut Transaction) {
    tx.hash = Default::default();
    tx.raw_cbor = None;
    tx.raw_body_cbor = None;
    tx.raw_witness_cbor = None;

    for output in &mut tx.body.outputs {
        clear_output_caches(output);
    }
    if let Some(output) = tx.body.collateral_return.as_mut() {
        clear_output_caches(output);
    }
    for sub in &mut tx.body.sub_transactions {
        sub.raw_body_cbor = None;
        // `tx_id` is `blake2b_256(raw_sub_body_cbor)`, recomputed by the
        // decoder from the sub-body's own bytes — the same derived-from-wire
        // family as `Transaction.hash`, and equally not something the encoder
        // round-trips.
        sub.tx_id = Default::default();
        for output in &mut sub.outputs {
            clear_output_caches(output);
        }
    }

    tx.witness_set.raw_redeemers_cbor = None;
    tx.witness_set.raw_plutus_data_cbor = None;
    tx.witness_set.original_script_data_hash = None;

    if let Some(aux) = tx.auxiliary_data.as_mut() {
        aux.raw_cbor = None;
    }

    canonicalise_set_order(tx);
}

/// Sort the CBOR `Set`-typed fields, and only those.
///
/// From Conway on, `encode_set_for_era` emits `#6.258([* a])` with items sorted
/// lexicographically by their CBOR encoding. A transaction whose inputs arrived
/// in some other order therefore comes back reordered — correctly. Comparing
/// order here would report that canonicalisation as a defect.
///
/// The distinction is the ledger's own and is load-bearing in BOTH directions:
///
/// - `Set` fields (inputs, collateral, required_signers, reference_inputs)
///   decode into a Haskell `Set`, so their order is unobservable and sorting
///   both sides discards nothing real.
/// - `OSet` fields (`certificates`, `proposal_procedures`) are NOT sorted here,
///   because they are not sorted by the encoder either — order is semantically
///   load-bearing (a registration must precede the delegation that uses it).
///   Running them through the sorting encoder was #940. Sorting them here would
///   re-hide it.
///
/// Witness-set collections are likewise left alone: the encoder emits them in
/// the order given (#939 established that sorting them would BE the
/// divergence, since Haskell replays original bytes via `encodePreEncoded`).
///
/// Pre-Conway eras use a plain array and preserve order, so no sorting is
/// applied there — a reordering in those eras would be a real defect and stays
/// visible.
fn canonicalise_set_order(tx: &mut Transaction) {
    // Redeemers and datums are canonicalised in EVERY era — only their
    // container shape is era-gated (Conway map vs pre-Conway list), not the
    // ordering. `encode_redeemers` sorts by ascending `(tag, index)` — which
    // is literally the Conway map key — and `encode_datums` by ascending datum
    // hash. Both are pinned by unit tests in dugite-serialization
    // (`encode_redeemers_sorts_by_tag_then_index`,
    // `encode_datums_sorts_by_hash_and_tags_conway`).
    //
    // Any deterministic total order works here; these two only have to make
    // the comparison order-insensitive, not reproduce the encoder's exact
    // permutation. Sorting is stable, so duplicate keys (the
    // `babbage_dup_redeemer_tx_572a9da4` fixture has two) keep their relative
    // order on both sides.
    tx.witness_set
        .redeemers
        .sort_by_key(|r| (redeemer_tag_ord(&r.tag), r.index));
    tx.witness_set
        .plutus_data
        .sort_by_cached_key(dugite_serialization::encode_plutus_data);

    if !matches!(tx.era, Era::Conway | Era::Dijkstra) {
        return;
    }

    tx.body.inputs.sort();
    tx.body.collateral.sort();
    tx.body.reference_inputs.sort();
    tx.body.required_signers.sort_by_key(|h| h.0);

    // Body key 14 is ONE wire field with TWO in-memory views: the Conway
    // decoder fills `guards` with the full credential list AND
    // `required_signers` with the key-hash subset, from the same set. So a
    // transaction built with only `required_signers` populated comes back with
    // `guards` derived from it — correct, and not a difference the encoder
    // introduced. Pre-Dijkstra, `guards` is not independently representable,
    // so it is replaced by that projection on both sides.
    if tx.era < Era::Dijkstra {
        // Pre-Dijkstra, key 14 IS `required_signers`; `guards` is only the
        // decoder's derived view, so it is replaced by that projection.
        tx.body.guards = tx
            .body
            .required_signers
            .iter()
            .map(|h32| {
                let mut bytes = [0u8; 28];
                bytes.copy_from_slice(&h32.as_bytes()[..28]);
                Credential::VerificationKey(Hash::from_bytes(bytes))
            })
            .collect();
    } else {
        // From Dijkstra, key 14 IS `guards`; `required_signers` is the derived
        // view (the key-hash subset, padded to Hash32), so it is replaced by
        // that projection instead. Same field, opposite direction.
        tx.body.required_signers = tx
            .body
            .guards
            .iter()
            .filter_map(|cred| match cred {
                Credential::VerificationKey(h28) => Some(h28.to_hash32_padded()),
                Credential::Script(_) => None,
            })
            .collect();
        tx.body.required_signers.sort_by_key(|h| h.0);
    }

    // Either way the wire form is sorted, so the field mirroring it comes back
    // sorted too.
    tx.body.guards.sort();
    if tx.era >= Era::Dijkstra {
        // The Dijkstra arm alone also dedups before encoding — an OSet cannot
        // hold duplicates, so a repeated guard is not representable on the
        // wire and its loss is not an encoder defect.
        tx.body.guards.dedup();
    }

    for sub in &mut tx.body.sub_transactions {
        sub.inputs.sort();
        sub.reference_inputs.sort();
    }
}

/// Mirrors `dugite_serialization`'s private `redeemer_tag_ord`.
///
/// `RedeemerTag` does not derive `Ord`, and this only needs to be a consistent
/// total order applied to both sides — not the encoder's exact one.
fn redeemer_tag_ord(tag: &RedeemerTag) -> u8 {
    match tag {
        RedeemerTag::Spend => 0,
        RedeemerTag::Mint => 1,
        RedeemerTag::Cert => 2,
        RedeemerTag::Reward => 3,
        RedeemerTag::Vote => 4,
        RedeemerTag::Propose => 5,
        RedeemerTag::Guarding => 6,
    }
}

/// Remove duplicates while preserving first-occurrence order.
///
/// Used for every CBOR `Set`/`OSet`-typed collection the generators build. The
/// decoders enforce uniqueness (`EnforceNoDuplicates`), so a duplicate is
/// rejected before the encoder is ever judged — a permanent false positive
/// rather than a finding. Order is preserved rather than sorted because the
/// `OSet` fields (certificates, proposal procedures) carry load-bearing order,
/// and sorting them was #940.
pub fn dedup_preserving_order<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Clear the wire-bytes caches on a single output.
fn clear_output_caches(output: &mut TransactionOutput) {
    output.raw_cbor = None;
    if let OutputDatum::InlineDatum { raw_cbor, .. } = &mut output.datum {
        *raw_cbor = None;
    }
}

impl Gen<'_> {
    /// A pre-Conway update proposal — transaction body key 6.
    ///
    /// This field had NO encoder arm while Shelley, Alonzo and Babbage all
    /// decoded it, so a pre-Conway transaction carrying one re-encoded into a
    /// body missing key 6 and changed its own transaction id. Found by this
    /// generator's sibling target; the fix is in `encode_update_proposal`.
    pub fn update_proposal(&mut self) -> UpdateProposal {
        let n = self.collection_len(6).max(1);
        let mut proposed_updates = Vec::new();
        for _ in 0..n {
            // `genesishash` is bstr(28) on the wire; the decoder stores it via
            // `Hash28::to_hash32_padded`, so a full-width Hash32 would come
            // back truncated.
            proposed_updates.push((
                self.hash28().to_hash32_padded(),
                self.ppu_for(PpuShape::PreConway),
            ));
        }
        // Keyed by genesis hash on the wire — duplicates collapse.
        let mut seen = Vec::new();
        proposed_updates.retain(|(hash, _)| {
            if seen.contains(hash) {
                false
            } else {
                seen.push(*hash);
                true
            }
        });
        UpdateProposal {
            proposed_updates,
            epoch: self.u64(),
        }
    }

    /// A Dijkstra sub-transaction (body key 23).
    pub fn sub_transaction(&mut self) -> SubTransaction {
        let inputs = self.collection_len(8);
        let outputs = self.collection_len(8);
        let ref_inputs = self.collection_len(4);

        let mut input_set: Vec<TransactionInput> = (0..inputs).map(|_| self.tx_input()).collect();
        input_set.sort();
        input_set.dedup();
        let mut ref_input_set: Vec<TransactionInput> =
            (0..ref_inputs).map(|_| self.tx_input()).collect();
        ref_input_set.sort();
        ref_input_set.dedup();

        SubTransaction {
            // Recomputed by the decoder from the sub-body's own bytes, so the
            // generated value is irrelevant and is normalised out before
            // comparison.
            tx_id: Default::default(),
            inputs: input_set,
            outputs: (0..outputs).map(|_| self.tx_output()).collect(),
            ttl: self.chance(160).then(|| SlotNo(self.u64())),
            validity_interval_start: self.chance(160).then(|| SlotNo(self.u64())),
            reference_inputs: ref_input_set,
            auxiliary_data_hash: self.chance(160).then(|| self.hash32()),
            raw_body_cbor: None,
        }
    }
}
