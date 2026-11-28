//! Decode an ImpSpec `st` / `st_out` blob with the PRODUCTION Haskell-snapshot
//! decoder and turn it into the inputs of dugite's own snapshot-import path
//! (`LedgerState::from_haskell_snapshot`).
//!
//! ## What the dumps hold
//!
//! The patched `conformanceHook` / `testConformance` write the STS rule's own
//! state type (see `scripts/regenerate-conformance-corpus/patch-impspec-core.py`):
//!
//! * **NEWEPOCH** — a bare `NewEpochState = array(7)`, UTxO inline.
//! * **LEDGER** — a bare `LedgerState = array(2) [CertState, UTxOState]`, UTxO
//!   inline. The epoch, the slot and the treasury/reserves pots are NOT in the
//!   state; they come from `env` (`LedgerEnv`), which is why a LEDGER dump
//!   carries no snapshots, no blocks-made, no pool distribution and no
//!   non-myopic record at all. Those groups are reported as "absent from the
//!   Haskell dump", never as matched.
//!
//! `LedgerEnv` (cardano-ledger `a88b60bd`,
//! `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ledger.hs`, `data LedgerEnv`;
//! Conway's `Environment (LEDGER era)` is that type) is encoded as
//! `array(5) [ledgerSlotNo, ledgerEpochNo :: Maybe EpochNo, ledgerIx :: TxIx,
//! ledgerPp, ledgerAccount :: ChainAccountState]`.
//!
//! ## The ImpSpec network
//!
//! `shelleyModifyImpInitProtVer`'s genesis
//! (`eras/shelley/impl/testlib/Test/Cardano/Ledger/Shelley/ImpTest.hs`,
//! `instance ShelleyEraImp ShelleyEra`, cardano-ledger `a88b60bd`):
//! `sgNetworkId = Testnet`, `sgActiveSlotsCoeff = 20/100`, `sgSecurityParam =
//! 108`, `sgEpochLength = 4320`, `sgSlotLength = 1`, `sgUpdateQuorum = 5`,
//! `sgMaxLovelaceSupply = 45_000_000_000_000_000`, `sgSystemStart =
//! 2017-09-23T21:44:51Z`. Every genesis-derived input below is one of those.

use std::collections::HashMap;

use dugite_primitives::address::Address;
use dugite_primitives::hash::{Hash28, Hash32, PolicyId};
use dugite_primitives::time::{EpochNo, SlotNo};
use dugite_primitives::transaction::{
    OutputDatum, PlutusData, ScriptRef, TransactionInput, TransactionOutput,
};
use dugite_primitives::value::{AssetName, Value};
use dugite_serialization::haskell_snapshot::{
    decode_bare_ledger_state, decode_bare_new_epoch_state, HaskellLedgerState,
    HaskellNewEpochState, HaskellNonMyopic, HaskellPraosState, HaskellSnapShot, HaskellSnapShots,
    HaskellUtxoEntry,
};

use crate::upstream::ledger_rules_replay::vector::ImpVector;

/// `sgEpochLength` of the ImpSpec genesis.
pub const IMP_EPOCH_LENGTH: u64 = 4320;
/// `sgSecurityParam` of the ImpSpec genesis.
pub const IMP_SECURITY_PARAM: u64 = 108;
/// `sgActiveSlotsCoeff` of the ImpSpec genesis (20/100).
pub const IMP_ACTIVE_SLOTS_COEFF: f64 = 0.2;
/// `sgSystemStart` (2017-09-23T21:44:51Z) as POSIX milliseconds.
pub const IMP_SYSTEM_START_MS: u64 = 1_506_203_091_000;
/// `sgSlotLength` of the ImpSpec genesis, in milliseconds.
pub const IMP_SLOT_LENGTH_MS: u32 = 1_000;
/// `sgMaxLovelaceSupply` of the ImpSpec genesis.
pub const IMP_MAX_LOVELACE_SUPPLY: u64 = 45_000_000_000_000_000;

/// Which STS rule a vector exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Ledger,
    NewEpoch,
}

/// Which `NewEpochState` field groups the dump actually carries. A group the
/// dump does not carry cannot be compared and must be reported as such.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carried {
    /// Snapshots, blocks-made, pool distribution, non-myopic (EpochState tail
    /// and the NES-level fields). Only a full `NewEpochState` has them.
    pub nes_level: bool,
}

/// A decoded ImpSpec state, ready to import and to project.
pub struct ImpState {
    pub kind: RuleKind,
    /// The production decoder's `NewEpochState`. For a LEDGER dump the
    /// NES-level fields the dump does not carry are empty/zero — see
    /// [`Carried`].
    pub nes: HaskellNewEpochState,
    /// The UTxO, converted the way the node's import converts a tables blob.
    pub utxo: Vec<(TransactionInput, TransactionOutput)>,
    pub carried: Carried,
    /// `LedgerEnv.ledgerSlotNo` (LEDGER only).
    pub env_slot: Option<u64>,
    /// `nesRu` is `SJust` (NEWEPOCH only). The production decoder skips the
    /// field; see [`reconstruct_tip_slot`].
    pub reward_update_present: bool,
}

/// Decode `st` (or `st_out`) of `vec` for its rule.
///
/// `which` is only used in error text. `env` is the vector's `env` (LEDGER
/// pots and epoch come from there, for both `st` and `st_out`: the rule does not
/// change them).
pub fn decode_imp_state(kind: RuleKind, bytes: &[u8], vec: &ImpVector) -> Result<ImpState, String> {
    match kind {
        RuleKind::NewEpoch => {
            let (nes, entries) = decode_bare_new_epoch_state(bytes)
                .map_err(|e| format!("NewEpochState decode: {e}"))?;
            let reward_update_present = nes_reward_update_present(bytes)?;
            Ok(ImpState {
                kind,
                nes,
                utxo: convert_utxo(&entries)?,
                carried: Carried { nes_level: true },
                env_slot: None,
                reward_update_present,
            })
        }
        RuleKind::Ledger => {
            let ls =
                decode_bare_ledger_state(bytes).map_err(|e| format!("LedgerState decode: {e}"))?;
            let env = decode_ledger_env(&vec.env_cbor)?;
            let empty = || HaskellSnapShot {
                stake: HashMap::new(),
                delegations: HashMap::new(),
                pool_params: HashMap::new(),
            };
            let nes = HaskellNewEpochState {
                epoch: EpochNo(env.epoch),
                blocks_made_prev: HashMap::new(),
                blocks_made_cur: HashMap::new(),
                treasury: env.treasury,
                reserves: env.reserves,
                cur_pparams: ls.gov_state.cur_pparams.clone(),
                prev_pparams: ls.gov_state.prev_pparams.clone(),
                deposited: ls.deposited,
                fees: ls.fees,
                donation: ls.donation,
                cert_state: ls.cert_state,
                snapshots: HaskellSnapShots {
                    mark: empty(),
                    set: empty(),
                    go: empty(),
                    fee: 0,
                },
                pool_distr: HashMap::new(),
                pool_distr_total_stake: 0,
                gov_state: ls.gov_state,
                instant_stake: ls.instant_stake,
                non_myopic: HaskellNonMyopic::default(),
            };
            Ok(ImpState {
                kind,
                nes,
                utxo: convert_utxo(&ls.utxo)?,
                carried: Carried { nes_level: false },
                env_slot: Some(env.slot),
                reward_update_present: false,
            })
        }
    }
}

/// The fields of `LedgerEnv` the replay needs.
struct LedgerEnv {
    slot: u64,
    epoch: u64,
    treasury: u64,
    reserves: u64,
}

fn decode_ledger_env(env_cbor: &[u8]) -> Result<LedgerEnv, String> {
    let mut d = minicbor::Decoder::new(env_cbor);
    let n = d
        .array()
        .map_err(|e| format!("LedgerEnv: {e}"))?
        .ok_or("LedgerEnv: indefinite array")?;
    if n != 5 {
        return Err(format!("LedgerEnv: expected array(5), got array({n})"));
    }
    let slot = d.u64().map_err(|e| format!("LedgerEnv.slot: {e}"))?;
    let epoch = match d
        .array()
        .map_err(|e| format!("LedgerEnv.epoch: {e}"))?
        .ok_or("LedgerEnv.epoch: indefinite")?
    {
        0 => slot / IMP_EPOCH_LENGTH,
        1 => d.u64().map_err(|e| format!("LedgerEnv.epoch: {e}"))?,
        n => return Err(format!("LedgerEnv.epoch: Maybe of length {n}")),
    };
    let _ix = d.u64().map_err(|e| format!("LedgerEnv.ix: {e}"))?;
    d.skip().map_err(|e| format!("LedgerEnv.pp: {e}"))?;
    let acct = d
        .array()
        .map_err(|e| format!("LedgerEnv.account: {e}"))?
        .ok_or("LedgerEnv.account: indefinite")?;
    if acct != 2 {
        return Err(format!("LedgerEnv.account: expected array(2), got {acct}"));
    }
    let treasury = d.u64().map_err(|e| format!("treasury: {e}"))?;
    let reserves = d.u64().map_err(|e| format!("reserves: {e}"))?;
    Ok(LedgerEnv {
        slot,
        epoch,
        treasury,
        reserves,
    })
}

/// `nesRu :: StrictMaybe PulsingRewUpdate` is `NewEpochState[4]`; `SJust` is
/// `array(1)`. The production decoder skips the field (dugite recomputes the
/// reward update), so the harness reads only its presence.
fn nes_reward_update_present(bytes: &[u8]) -> Result<bool, String> {
    let mut d = minicbor::Decoder::new(bytes);
    d.array().map_err(|e| format!("NES: {e}"))?;
    for i in 0..4 {
        d.skip().map_err(|e| format!("NES[{i}]: {e}"))?;
    }
    let n = d
        .array()
        .map_err(|e| format!("NES[4]: {e}"))?
        .ok_or("NES[4]: indefinite")?;
    Ok(n == 1)
}

/// The slot dugite's imported ledger tip should sit at.
///
/// A real node that holds a `nesRu = SJust` already has its reward pulser
/// running, which Haskell starts once the tip passes `epochStart + 4k/f`
/// (`RewardTiming`); dugite re-derives the same flag from the imported tip slot
/// (`finalise_genesis_derived_windows`). So the tip is placed at the epoch's
/// last slot when `nesRu` is present and at its first slot when it is not.
pub fn reconstruct_tip_slot(epoch: u64, reward_update_present: bool) -> u64 {
    let first = epoch * IMP_EPOCH_LENGTH;
    if reward_update_present {
        first + IMP_EPOCH_LENGTH - 1
    } else {
        first
    }
}

/// Convert inline UTxO entries to dugite's output type, exactly as the node's
/// `import_haskell_ledger_snapshot` converts a MemPack tables blob.
fn convert_utxo(
    entries: &[HaskellUtxoEntry],
) -> Result<Vec<(TransactionInput, TransactionOutput)>, String> {
    entries
        .iter()
        .map(|e| {
            let input = TransactionInput {
                transaction_id: e.txid,
                index: e.txix as u32,
            };
            let (txout, _) =
                dugite_serialization::mempack::txout::decode_mempack_txout(&e.txout_mempack)
                    .map_err(|er| {
                        format!("MemPack TxOut for {}#{}: {er}", e.txid.to_hex(), e.txix)
                    })?;
            if txout.address.is_empty() {
                return Err(format!("empty address for {}#{}", e.txid.to_hex(), e.txix));
            }
            let address = Address::from_bytes(&txout.address)
                .map_err(|er| format!("address for {}#{}: {er}", e.txid.to_hex(), e.txix))?;
            let mut value = Value::lovelace(txout.coin);
            if let Some(rep) = &txout.multi_asset {
                let triples = dugite_serialization::mempack::compact::parse_multi_asset_rep(
                    rep,
                    txout.num_assets as usize,
                )
                .map_err(|er| format!("multi-asset for {}#{}: {er}", e.txid.to_hex(), e.txix))?;
                for (pid, name, qty) in triples {
                    let asset = AssetName::new(name).map_err(|er| {
                        format!("asset name for {}#{}: {er}", e.txid.to_hex(), e.txix)
                    })?;
                    *value
                        .multi_asset
                        .entry(PolicyId::from_bytes(pid))
                        .or_default()
                        .entry(asset)
                        .or_default() += qty;
                }
            }
            let datum = if let Some(h) = txout.datum_hash {
                OutputDatum::DatumHash(Hash32::from_bytes(h))
            } else if let Some(cbor) = &txout.datum {
                match dugite_serialization::decode_plutus_data_cbor(cbor) {
                    Ok(data) => OutputDatum::InlineDatum {
                        data,
                        raw_cbor: Some(cbor.clone()),
                    },
                    Err(_) => OutputDatum::InlineDatum {
                        data: PlutusData::Bytes(cbor.clone()),
                        raw_cbor: Some(cbor.clone()),
                    },
                }
            } else {
                OutputDatum::None
            };
            let script_ref = match txout.script_ref.as_deref() {
                Some(blob) => {
                    use dugite_serialization::mempack::txout::{
                        parse_script_ref_kind, ScriptRefKind,
                    };
                    let kind = parse_script_ref_kind(blob).map_err(|er| {
                        format!("script ref for {}#{}: {er}", e.txid.to_hex(), e.txix)
                    })?;
                    Some(match kind {
                        ScriptRefKind::Native(cbor) => ScriptRef::NativeScript(
                            dugite_serialization::decode_native_script_cbor(&cbor).map_err(
                                |er| {
                                    format!(
                                        "native script for {}#{}: {er}",
                                        e.txid.to_hex(),
                                        e.txix
                                    )
                                },
                            )?,
                        ),
                        ScriptRefKind::Plutus { lang_tag, body } => match lang_tag {
                            0 => ScriptRef::PlutusV1(body),
                            1 => ScriptRef::PlutusV2(body),
                            2 => ScriptRef::PlutusV3(body),
                            3 => ScriptRef::PlutusV4(body),
                            t => return Err(format!("unknown Plutus language tag {t}")),
                        },
                    })
                }
                None => None,
            };
            Ok((
                input,
                TransactionOutput {
                    address,
                    value,
                    datum,
                    script_ref,
                    is_legacy: false,
                    raw_cbor: None,
                },
            ))
        })
        .collect()
}

/// A placeholder `HaskellPraosState`: nonces are consensus state, not ledger
/// state, and no ImpSpec dump carries them.
fn zero_praos() -> HaskellPraosState {
    HaskellPraosState {
        last_slot: None,
        opcert_counters: HashMap::new(),
        evolving_nonce: Hash32::ZERO,
        candidate_nonce: Hash32::ZERO,
        epoch_nonce: Hash32::ZERO,
        previous_epoch_nonce: Hash32::ZERO,
        lab_nonce: Hash32::ZERO,
        last_epoch_block_nonce: Hash32::ZERO,
    }
}

/// Wrap a decoded state in the `HaskellLedgerState` that
/// `LedgerState::from_haskell_snapshot` takes.
///
/// The tip is not part of an ImpSpec dump; it is placed at `tip_slot` with a
/// fixed hash, and the harness's block connects to it.
pub fn to_haskell_ledger_state(
    nes: HaskellNewEpochState,
    tip_slot: u64,
    tip_hash: Hash32,
) -> HaskellLedgerState {
    HaskellLedgerState {
        tip_slot: SlotNo(tip_slot),
        tip_block_no: 1,
        tip_hash,
        epoch: nes.epoch,
        new_epoch_state: nes,
        praos_state: zero_praos(),
    }
}

/// Hash28 → typed Hash32 (28-byte hash, type byte at index 28), the key the
/// ledger uses for credentials (`Credential::to_typed_hash32`).
pub fn typed_hash32(tag: u8, h: &Hash28) -> Hash32 {
    let mut b = [0u8; 32];
    b[..28].copy_from_slice(h.as_bytes());
    b[28] = tag;
    Hash32::from_bytes(b)
}
