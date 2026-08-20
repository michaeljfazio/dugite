use anyhow::{Context, Result};
use dugite_ledger::SlotConfig;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::Rational;
use dugite_primitives::value::Lovelace;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};

// ──────────────────────────────────────────────────────────────────────────
// Byron genesis
// ──────────────────────────────────────────────────────────────────────────

/// Byron genesis configuration (compatible with cardano-node byron-genesis.json)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
// Fields populated by serde deserialization from cardano-node genesis JSON
pub struct ByronGenesis {
    /// AVVM (Ada Voucher Vending Machine) distribution: base64 pubkey → lovelace
    #[serde(default)]
    pub avvm_distr: HashMap<String, String>,
    /// Non-AVVM initial balances: base58 Byron address → lovelace
    #[serde(default)]
    pub non_avvm_balances: HashMap<String, String>,
    /// Bootstrap stakeholders: KeyHash hex → weight. The KEY SET is the
    /// genesis-key authority (`gdGenesisKeyHashes`) both the delegation and
    /// update-proposal state machines gate on (issue #1084); the weight
    /// itself is irrelevant to that gate.
    #[serde(default, rename = "bootStakeholders")]
    pub boot_stakeholders: HashMap<String, u64>,
    /// Heavy (genesis-key) delegation certificates: issuer KeyHash hex → cert.
    #[serde(default, rename = "heavyDelegation")]
    pub heavy_delegation: HashMap<String, ByronHeavyDelegationEntry>,
    /// System start time (POSIX timestamp)
    #[serde(rename = "startTime")]
    pub _start_time: u64,
    /// Block version data (fee policy, slot duration, etc.)
    #[serde(default)]
    pub block_version_data: ByronBlockVersionData,
    /// Protocol constants (k, protocol magic)
    #[serde(default)]
    pub protocol_consts: ByronProtocolConsts,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
// Fields populated by serde deserialization from cardano-node genesis JSON
pub struct ByronBlockVersionData {
    #[serde(default)]
    pub slot_duration: String,
    // These are the GENESIS values — `UPI.State.adoptedProtocolParameters`
    // (issue #1084) is seeded from them at startup, then Byron's on-chain
    // update system may move them: measured at mainnet Byron epoch 100,
    // cardano-node reports maxBlockSize 32768 / maxTxSize 8192 where genesis
    // says 2000000 / 4096. `seed_byron_genesis` is the ONLY place these are
    // read as "the adopted values" — everywhere else must read
    // `LedgerState.byron.update.adopted_protocol_parameters` instead.
    #[serde(default, rename = "maxBlockSize")]
    pub max_block_size: String,
    #[serde(default, rename = "maxHeaderSize")]
    pub max_header_size: String,
    #[serde(default, rename = "maxTxSize")]
    pub max_tx_size: String,
    #[serde(default, rename = "maxProposalSize")]
    pub max_proposal_size: String,
    #[serde(default, rename = "scriptVersion")]
    pub script_version: u16,
    #[serde(default, rename = "mpcThd")]
    pub mpc_thd: String,
    #[serde(default, rename = "heavyDelThd")]
    pub heavy_del_thd: String,
    #[serde(default, rename = "updateVoteThd")]
    pub update_vote_thd: String,
    #[serde(default, rename = "updateProposalThd")]
    pub update_proposal_thd: String,
    #[serde(default, rename = "updateImplicit")]
    pub update_implicit: String,
    #[serde(default, rename = "softforkRule")]
    pub softfork_rule: ByronSoftforkRule,
    #[serde(default, rename = "txFeePolicy")]
    pub tx_fee_policy: ByronTxFeePolicy,
    #[serde(default, rename = "unlockStakeEpoch")]
    pub unlock_stake_epoch: String,
}

/// `heavyDelegation` map value — one heavyweight delegation certificate as
/// carried in genesis JSON. `cert`/`issuer_pk` are captured for completeness
/// but unused: Byron signature verification is a separate, deliberately
/// out-of-scope gap (see the #1084 design doc §3.6). `omega` is the
/// certificate's target epoch — always 0 in every real genesis.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByronHeavyDelegationEntry {
    #[allow(dead_code)]
    #[serde(default)]
    pub cert: String,
    #[serde(default, rename = "delegatePk")]
    pub delegate_pk: String,
    #[allow(dead_code)]
    #[serde(default, rename = "issuerPk")]
    pub issuer_pk: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub omega: u64,
}

/// `softforkRule` — genesis JSON encodes it as an OBJECT (`{initThd, minThd,
/// thdDecrement}`), unlike the on-chain `upprop` wire form, which is a plain
/// 3-tuple. Each value is a `LovelacePortion` numerator over the implicit
/// 1e15 denominator, same scale as the on-chain form.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByronSoftforkRule {
    #[serde(default, rename = "initThd")]
    pub init_thd: String,
    #[serde(default, rename = "minThd")]
    pub min_thd: String,
    #[serde(default, rename = "thdDecrement")]
    pub thd_decrement: String,
}

/// Byron's on-chain `txFeePolicy`, as the genesis file stores it.
///
/// These were parsed into `_summand` / `_multiplier` and DISCARDED, while
/// `ByronFeePolicy` in dugite-ledger hardcoded mainnet's values — and its comment
/// claimed "dugite does not yet parse the Byron `txFeePolicy`", which was not
/// true: it parsed it and dropped it. That is #1067's shape, a field decoded for
/// want of a destination, and it means any network whose genesis carries
/// DIFFERENT values would have been validated against mainnet's from block 1.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByronTxFeePolicy {
    /// `a`, the constant summand, in NANO scale (value x 1e9).
    #[serde(default, rename = "summand")]
    pub summand: String,
    /// `b`, the per-byte multiplier, in NANO scale (value x 1e9).
    #[serde(default, rename = "multiplier")]
    pub multiplier: String,
}

/// The nano scale both `txFeePolicy` values are stored in.
///
/// The struct's previous comment said "both values are x1e12". It is 1e9, and
/// the arithmetic settles it against the real files: mainnet and preprod both
/// carry `summand = 155381000000000` and `multiplier = 43946000000`, and only
/// 1e9 yields Byron's documented `a = 155381` lovelace and
/// `b = 43.946 = 21973/500`. Using 1e12 would have made every Byron minimum fee
/// a thousandfold too small — i.e. accepted transactions cardano-node rejects.
const BYRON_FEE_POLICY_NANO: u64 = 1_000_000_000;

/// The protocol major version at which the Conway era begins.
///
/// Used to decide whether a genesis file *starts* in Conway. A network below
/// this reaches Conway through the Babbage->Conway translation, which is where
/// upstream injects Conway's upgrade parameters; one at or above it never makes
/// that hop and needs them from genesis.
const CONWAY_PROTOCOL_MAJOR: u64 = 9;

impl ByronTxFeePolicy {
    /// `(summand_lovelace, (mult_num, mult_den))` in exact rational form.
    ///
    /// Returns `None` if either field is absent/unparseable, or if the summand is
    /// not a whole number of lovelace after de-scaling — Byron's `a` is a
    /// `Lovelace`, so a fractional value means the genesis is not one this code
    /// can honour, and silently truncating it would put a wrong fee on a
    /// consensus path.
    pub fn to_exact(&self) -> Option<(u64, (u64, u64))> {
        let summand_nano: u128 = self.summand.parse().ok()?;
        let mult_nano: u128 = self.multiplier.parse().ok()?;
        let nano = BYRON_FEE_POLICY_NANO as u128;
        if !summand_nano.is_multiple_of(nano) {
            return None;
        }
        let summand = u64::try_from(summand_nano / nano).ok()?;
        // Reduce mult_nano / 1e9 to lowest terms; the multiplier is genuinely
        // fractional (mainnet's is 21973/500) and must stay exact, because the
        // fee is `a + ceiling(size * b)` over exact rationals.
        let g = gcd_u128(mult_nano, nano).max(1);
        Some((
            summand,
            (
                u64::try_from(mult_nano / g).ok()?,
                u64::try_from(nano / g).ok()?,
            ),
        ))
    }
}

impl ByronBlockVersionData {
    /// Convert to the ledger's `ByronProtocolParameters`, for genesis seeding
    /// (issue #1084). `None` if any field fails to parse — a malformed
    /// genesis is a startup-fatal condition the caller reports, not a value
    /// this conversion silently defaults.
    pub fn to_protocol_parameters(
        &self,
    ) -> Option<dugite_ledger::eras::byron::ByronProtocolParameters> {
        let (tx_summand, tx_mult) = self.tx_fee_policy.to_exact()?;
        Some(dugite_ledger::eras::byron::ByronProtocolParameters {
            script_version: self.script_version,
            slot_duration: self.slot_duration.parse().ok()?,
            max_block_size: self.max_block_size.parse().ok()?,
            max_header_size: self.max_header_size.parse().ok()?,
            max_tx_size: self.max_tx_size.parse().ok()?,
            max_proposal_size: self.max_proposal_size.parse().ok()?,
            mpc_thd: self.mpc_thd.parse().ok()?,
            heavy_del_thd: self.heavy_del_thd.parse().ok()?,
            update_vote_thd: self.update_vote_thd.parse().ok()?,
            update_proposal_thd: self.update_proposal_thd.parse().ok()?,
            update_implicit: self.update_implicit.parse().ok()?,
            soft_fork_rule: (
                self.softfork_rule.init_thd.parse().ok()?,
                self.softfork_rule.min_thd.parse().ok()?,
                self.softfork_rule.thd_decrement.parse().ok()?,
            ),
            tx_fee_policy: (tx_summand, tx_mult),
            unlock_stake_epoch: self.unlock_stake_epoch.parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByronProtocolConsts {
    pub k: u64,
    pub protocol_magic: u64,
}

/// A genesis UTxO entry (address bytes + lovelace amount)
#[derive(Debug, Clone)]
pub struct GenesisUtxoEntry {
    pub address: Vec<u8>,
    pub lovelace: u64,
}

impl ByronGenesis {
    /// Load the Byron genesis and compute its Blake2b-256 hash.
    ///
    /// The hash is computed over the raw file content (canonical JSON), matching
    /// the Cardano reference implementation.
    pub fn load_with_hash(path: &Path) -> Result<(Self, dugite_primitives::hash::Hash32)> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Byron genesis: {}", path.display()))?;
        let genesis: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Byron genesis: {}", path.display()))?;
        let hash = dugite_primitives::hash::blake2b_256(content.as_bytes());
        debug!(
            genesis_hash = %hash.to_hex(),
            "Byron genesis hash computed"
        );
        Ok((genesis, hash))
    }

    /// Get the protocol magic from the genesis config
    pub fn protocol_magic(&self) -> u64 {
        self.protocol_consts.protocol_magic
    }

    /// Get the security parameter k
    pub fn security_param(&self) -> u64 {
        self.protocol_consts.k
    }

    /// Get the Byron slot duration in milliseconds from genesis config.
    /// Falls back to 20000ms (20s) if not specified or unparseable.
    pub fn slot_duration_ms(&self) -> u64 {
        self.block_version_data
            .slot_duration
            .parse::<u64>()
            .unwrap_or(20_000)
    }

    /// `bootStakeholders`' key set as `Hash28` — the genesis-key authority
    /// (`gdGenesisKeyHashes`) both the delegation and update-proposal state
    /// machines gate on (issue #1084). Weights are irrelevant to that gate.
    pub fn allowed_delegators(
        &self,
    ) -> std::collections::BTreeSet<dugite_primitives::hash::Hash28> {
        self.boot_stakeholders
            .keys()
            .filter_map(|hex| dugite_primitives::hash::Hash28::from_hex(hex).ok())
            .collect()
    }

    /// `heavyDelegation` as `(issuer, delegate)` `Hash28` pairs, for genesis
    /// seeding (issue #1084). The map KEY is already the issuer's KeyHash;
    /// the delegate is derived from `delegatePk` (a raw 64-byte extended
    /// verification key, standard base64) via the ledger's `byron_key_hash`.
    /// An entry that fails to parse is skipped with a warning rather than
    /// failing genesis load outright — matches `initial_utxos`'s posture on
    /// a malformed individual entry.
    pub fn heavy_delegation_pairs(
        &self,
    ) -> Vec<(
        dugite_primitives::hash::Hash28,
        dugite_primitives::hash::Hash28,
    )> {
        use base64::Engine;
        self.heavy_delegation
            .iter()
            .filter_map(|(issuer_hex, entry)| {
                let issuer = match dugite_primitives::hash::Hash28::from_hex(issuer_hex) {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::warn!(issuer_hex, "Byron heavyDelegation: bad issuer hash: {e}");
                        return None;
                    }
                };
                let delegate_pk =
                    match base64::engine::general_purpose::STANDARD.decode(&entry.delegate_pk) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(
                                issuer_hex,
                                "Byron heavyDelegation: bad delegatePk base64: {e}"
                            );
                            return None;
                        }
                    };
                let delegate = dugite_ledger::eras::byron::byron_key_hash(&delegate_pk);
                Some((issuer, delegate))
            })
            .collect()
    }

    /// Extract the initial UTxO set from both nonAvvmBalances and avvmDistr.
    ///
    /// Returns decoded address bytes and lovelace amounts for all non-zero balances.
    /// For nonAvvmBalances, addresses are base58-decoded directly.
    /// For avvmDistr, base64url Ed25519 public keys are converted to Byron redeem addresses.
    /// The genesis UTxO, including ZERO-VALUE entries.
    ///
    /// cardano-ledger keeps them, and both maps go through the same unfiltered
    /// path:
    ///
    /// ```haskell
    /// -- Cardano/Chain/UTxO/GenesisUTxO.hs
    /// genesisUtxo config = UTxO.fromBalances (avvmBalances <> nonAvvmBalances)
    /// -- Cardano/Chain/UTxO/UTxO.hs
    /// fromBalances = ... . concat . fmap (fromTxOut . uncurry TxOut)
    /// fromTxOut out = fromList [(TxInUtxo (coerce . serializeCborHash $ txOutAddress out) 0, out)]
    /// -- Cardano/Chain/Common/Lovelace.hs
    /// mkLovelace c | c <= maxLovelaceVal = Right (Lovelace c)
    /// ```
    ///
    /// a plain `M.toList` with no filter, and zero IS a valid Lovelace — only
    /// the upper bound is checked. Each TxIn is derived by hashing the output
    /// ADDRESS, so N distinct addresses give N distinct TxIns.
    ///
    /// dugite used to `continue` past zero-value entries, which made its UTxO
    /// set SMALLER than cardano-node's. Measured on preprod, whose Byron genesis
    /// carries 8 `nonAvvmBalances` entries of which SEVEN are zero:
    /// cardano-node's own dump reports count 8 at Byron epochs 1-3 where dugite
    /// reported 1. The balances agreed to the lovelace, which is exactly why
    /// every check that sums was blind to it.
    ///
    /// It is a false REJECT on block validity: a transaction spending one of
    /// those outputs is accepted by cardano-node — the UTxO exists — and was
    /// rejected here with `InputNotFound`, because it was never created.
    ///
    /// Shelley's `initialFunds` has the same property and the same fix; see
    /// `Cardano/Ledger/Shelley/Genesis.hs::genesisUTxO`, another unfiltered
    /// comprehension.
    pub fn initial_utxos(&self) -> Vec<GenesisUtxoEntry> {
        let mut entries = Vec::new();
        let protocol_magic = self.protocol_consts.protocol_magic;

        // Process nonAvvmBalances (base58 Byron addresses)
        for (addr_str, lovelace_str) in &self.non_avvm_balances {
            let lovelace: u64 = match lovelace_str.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Decode base58 Byron address
            match bs58::decode(addr_str).into_vec() {
                Ok(addr_bytes) => {
                    entries.push(GenesisUtxoEntry {
                        address: addr_bytes,
                        lovelace,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to decode Byron genesis address: {}: {}",
                        &addr_str[..40.min(addr_str.len())],
                        e
                    );
                }
            }
        }

        let non_avvm_count = entries.len();

        // Process avvmDistr (base64url Ed25519 public keys → Byron redeem addresses)
        for (pubkey_b64, lovelace_str) in &self.avvm_distr {
            let lovelace: u64 = match lovelace_str.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            match Self::avvm_to_address(pubkey_b64, protocol_magic) {
                Ok(addr_bytes) => {
                    entries.push(GenesisUtxoEntry {
                        address: addr_bytes,
                        lovelace,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to convert AVVM key to address: {}: {}",
                        &pubkey_b64[..20.min(pubkey_b64.len())],
                        e
                    );
                }
            }
        }

        let avvm_count = entries.len() - non_avvm_count;
        debug!(
            non_avvm = non_avvm_count,
            avvm = avvm_count,
            total = entries.len(),
            total_lovelace = entries.iter().map(|e| e.lovelace).sum::<u64>(),
            "Byron genesis: extracted initial UTxOs"
        );

        entries
    }

    /// Convert an AVVM base64url Ed25519 public key to a Byron redeem address.
    ///
    /// The AVVM distribution uses base64url-encoded 32-byte Ed25519 verification keys.
    /// These are converted to Byron redeem addresses following the Haskell reference:
    /// 1. Decode base64url → 32-byte Ed25519 public key
    /// 2. Build SpendingData::Redeem with the raw key bytes
    /// 3. Compute addrRoot = Blake2b-224(SHA3-256(CBOR([AddrType::Redeem, spending_data, attributes])))
    /// 4. Construct CRC-protected Byron address CBOR
    fn avvm_to_address(pubkey_b64: &str, protocol_magic: u64) -> Result<Vec<u8>> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        // Decode base64url key (Haskell normalizes -/_ to +/ before standard base64)
        let key_bytes = URL_SAFE_NO_PAD
            .decode(pubkey_b64)
            .or_else(|_| {
                // Try with padding
                use base64::engine::general_purpose::URL_SAFE;
                URL_SAFE.decode(pubkey_b64)
            })
            .or_else(|_| {
                // Try standard base64 as fallback
                use base64::engine::general_purpose::STANDARD;
                STANDARD.decode(pubkey_b64)
            })
            .with_context(|| "Invalid base64 AVVM key")?;

        anyhow::ensure!(
            key_bytes.len() == 32,
            "AVVM key must be 32 bytes, got {}",
            key_bytes.len()
        );

        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&key_bytes);

        // Build network tag: None for mainnet (764824073), Some(cbor(magic)) for testnets
        let network_tag = if protocol_magic == 764824073 {
            None
        } else {
            // Network tag is CBOR-serialized protocol magic as bytes
            let mut tag_buf = Vec::new();
            minicbor::encode(protocol_magic as u32, &mut tag_buf)
                .map_err(|e| anyhow::anyhow!("CBOR encode network tag: {e}"))?;
            Some(tag_buf)
        };

        // Build the redeem address via the in-house ByronAddressPayload (M6b).
        let payload = dugite_primitives::address::byron::ByronAddressPayload::new_redeem(
            &pubkey,
            network_tag,
        );
        Ok(payload.to_wire_bytes())
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Shelley genesis
// ──────────────────────────────────────────────────────────────────────────

/// Shelley genesis configuration (compatible with cardano-node shelley-genesis.json)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelleyGenesis {
    pub network_magic: u64,
    pub network_id: String,
    pub system_start: String,
    pub active_slots_coeff: f64,
    pub security_param: u64,
    pub epoch_length: u64,
    pub slot_length: u64,
    pub max_lovelace_supply: u64,
    pub max_k_e_s_evolutions: u64,
    pub slots_per_k_e_s_period: u64,
    pub update_quorum: u64,
    pub protocol_params: ShelleyGenesisProtocolParams,
    /// Genesis delegation keys: genesis_credential_hash → (delegate_hash, vrf_hash).
    /// Present on all networks; used for BFT overlay in early Shelley.
    #[serde(default)]
    pub gen_delegs: HashMap<String, GenDelegPair>,
    /// Initial UTxO set for the Shelley era. Keys are bech32-encoded Shelley
    /// addresses, values are lovelace amounts. Empty on mainnet/preview/preprod;
    /// used by custom devnets.
    #[serde(default)]
    pub initial_funds: HashMap<String, u64>,
    /// Initial staking configuration: pool registrations and stake delegations.
    /// Absent on mainnet/preview/preprod; used by custom devnets.
    #[serde(default)]
    pub staking: Option<ShelleyGenesisStaking>,
}

/// A genesis delegation pair: delegate key hash and VRF key hash.
#[derive(Debug, Clone, Deserialize)]
pub struct GenDelegPair {
    pub delegate: String,
    pub vrf: String,
}

/// Initial staking configuration from Shelley genesis.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShelleyGenesisStaking {
    /// Pool registrations: pool_id_hex → pool parameters.
    #[serde(default)]
    pub pools: HashMap<String, ShelleyGenesisPool>,
    /// Stake delegations: stake_credential_hex → pool_id_hex.
    #[serde(default)]
    pub stake: HashMap<String, String>,
}

/// A pool registration entry in Shelley genesis staking config.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelleyGenesisPool {
    pub cost: u64,
    pub margin: f64,
    #[serde(default)]
    #[allow(dead_code)] // deserialized but not used for ledger state
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub owners: Vec<String>,
    pub pledge: u64,
    /// VRF key hash (hex-encoded). Older cardano-cli emits this under
    /// `publicKey`; cardano-cli 11.0.0.0+ emits it as `vrf`.
    #[serde(alias = "vrf")]
    pub public_key: String,
    #[serde(default)]
    #[allow(dead_code)] // deserialized but not used for ledger state
    pub relays: Vec<serde_json::Value>,
    /// Reward account. Older cardano-cli emits this under `rewardAccount`;
    /// cardano-cli 11.0.0.0+ emits it under `accountAddress`.
    #[serde(alias = "accountAddress")]
    pub reward_account: serde_json::Value,
}

/// Protocol parameters as specified in Shelley genesis
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelleyGenesisProtocolParams {
    pub min_fee_a: u64,
    pub min_fee_b: u64,
    pub max_block_body_size: u64,
    pub max_tx_size: u64,
    pub max_block_header_size: u64,
    pub key_deposit: u64,
    pub pool_deposit: u64,
    pub e_max: u64,
    #[serde(alias = "nOpt")]
    pub n_opt: u64,
    pub a0: f64,
    pub rho: f64,
    pub tau: f64,
    pub min_pool_cost: u64,
    #[serde(default)]
    pub min_u_tx_o_value: u64,
    /// Decentralisation parameter (d). 1 = fully federated, 0 = fully decentralised.
    /// Deprecated since Babbage (always 0 in Conway).
    #[serde(alias = "decentralisationParam", default)]
    pub decentralisation_param: f64,
    pub protocol_version: ProtocolVersion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProtocolVersion {
    pub major: u64,
    pub minor: u64,
}

impl ShelleyGenesis {
    /// Validate genesis parameters that would cause panics or undefined behavior
    /// if zero.
    ///
    /// Called from `load_with_hash` immediately after deserialization so that
    /// degenerate genesis files are rejected at startup rather than causing
    /// divide-by-zero / modulo-by-zero panics later in consensus.
    ///
    /// Issues #545 E8/E9 (consensus defense-in-depth via `checked_div` /
    /// `checked_rem`) and #546 (startup rejection) both contribute here.
    pub fn validate(&self) -> Result<()> {
        if self.slots_per_k_e_s_period == 0 {
            anyhow::bail!(
                "Invalid Shelley genesis: slotsPerKESPeriod is 0 — \
                 this would cause a divide-by-zero in KES period validation. \
                 Expected a positive value (mainnet/preview/preprod use 129600)."
            );
        }
        if self.max_k_e_s_evolutions == 0 {
            anyhow::bail!(
                "Invalid Shelley genesis: maxKESEvolutions is 0 — \
                 every block's KES key would be immediately expired. \
                 Expected a positive value (mainnet/preview/preprod use 62)."
            );
        }
        if self.epoch_length == 0 {
            anyhow::bail!(
                "Invalid Shelley genesis: epochLength is 0 — \
                 this would cause a modulo-by-zero in the nonce contribution window check. \
                 Expected a positive value (mainnet uses 432000)."
            );
        }
        if self.security_param == 0 {
            anyhow::bail!(
                "Invalid Shelley genesis: securityParam (k) is 0 — \
                 the stability window would collapse to zero, preventing any block from \
                 being considered stable. Expected a positive value (mainnet uses 2160)."
            );
        }
        Ok(())
    }

    /// Load the Shelley genesis and compute its Blake2b-256 hash.
    ///
    /// The hash is computed over the raw file content (canonical JSON), matching
    /// the Cardano reference implementation. This hash is used as the initial
    /// value for the rolling nonce (eta_v) in consensus.
    pub fn load_with_hash(path: &Path) -> Result<(Self, dugite_primitives::hash::Hash32)> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Shelley genesis: {}", path.display()))?;
        let genesis: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Shelley genesis: {}", path.display()))?;
        genesis.validate()?;
        let hash = dugite_primitives::hash::blake2b_256(content.as_bytes());
        debug!(
            genesis_hash = %hash.to_hex(),
            "Shelley genesis hash computed"
        );
        Ok((genesis, hash))
    }

    /// Apply genesis parameters to protocol parameters, keeping Conway-era
    /// defaults for fields not present in Shelley genesis.
    pub fn apply_to_protocol_params(&self, params: &mut ProtocolParameters) {
        let gp = &self.protocol_params;
        params.min_fee_a = gp.min_fee_a;
        params.min_fee_b = gp.min_fee_b;
        params.max_block_body_size = gp.max_block_body_size;
        params.max_tx_size = gp.max_tx_size;
        params.max_block_header_size = gp.max_block_header_size;
        params.key_deposit = Lovelace(gp.key_deposit);
        params.pool_deposit = Lovelace(gp.pool_deposit);
        params.e_max = gp.e_max;
        params.n_opt = gp.n_opt;
        params.a0 = float_to_rational(gp.a0);
        params.rho = float_to_rational(gp.rho);
        params.tau = float_to_rational(gp.tau);
        params.min_pool_cost = Lovelace(gp.min_pool_cost);
        // Flat Shelley/Allegra/Mary minimum UTxO value (issue #919). Haskell
        // `getMinCoinTxOut` returns this directly for Shelley/Allegra and
        // scales it in Mary's `scaledMinDeposit` — see
        // `ProtocolParameters::min_coin_for_output`.
        params.min_utxo_value = Lovelace(gp.min_u_tx_o_value);
        params.protocol_version_major = gp.protocol_version.major;
        params.protocol_version_minor = gp.protocol_version.minor;
        params.active_slots_coeff = self.active_slots_coeff;
        params.d = float_to_rational(gp.decentralisation_param);
    }

    /// Derive the SlotConfig for Plutus time conversion, anchored at the
    /// Shelley hard-fork boundary.
    ///
    /// Cardano's Plutus `slotToPOSIXTime` (and the Ouroboros HardFork history
    /// `slotToWallclock`) translate Shelley-era slots relative to the Shelley
    /// era's *own* start, NOT relative to the Byron network genesis.  For
    /// mainnet, Byron used 20-second slots from 2017-09-23; the Shelley hard
    /// fork occurred at epoch 208, absolute slot 4,492,800 (2020-07-29
    /// 21:44:51 UTC).  A linear conversion anchored at the Byron network start
    /// with zero_slot=0 places a Shelley slot roughly 2.75 years too early,
    /// causing every time/deadline-checking script to spuriously fail.
    ///
    /// Cross-validated against Haskell `Ouroboros.Consensus.HardFork.History
    /// .Qry.slotToWallclock`: the formula uses `boundTime eraStart` as the
    /// anchor — i.e. the era's start boundary, not the network genesis.
    ///
    /// Parameters:
    /// - `shelley_transition_epoch`: The epoch at which Byron ended and Shelley
    ///   started (e.g. 208 for mainnet, 4 for preprod, 0 for preview/devnets).
    /// - `byron_epoch_size`: Slots per Byron epoch (10 * security_param k, e.g.
    ///   21600 for mainnet/preprod, 0 for preview since Byron never existed).
    /// - `byron_slot_duration_ms`: Duration of a Byron slot in milliseconds
    ///   (20000 for mainnet/preprod, unused when transition_epoch == 0).
    ///
    /// For networks where `shelley_transition_epoch == 0` (preview, devnets),
    /// the Shelley era starts at slot 0 so this collapses to the simpler
    /// `(system_start, 0)` anchor, which is correct.
    ///
    /// Mainnet verification:
    ///   zero_slot = 208 * 21600 = 4_492_800
    ///   zero_time = 1_506_203_091_000 + 4_492_800 * 20_000 = 1_596_059_091_000
    pub fn slot_config(
        &self,
        shelley_transition_epoch: u64,
        byron_epoch_size: u64,
        byron_slot_duration_ms: u64,
    ) -> SlotConfig {
        let network_start_ms = chrono::DateTime::parse_from_rfc3339(&self.system_start)
            .map(|dt| dt.timestamp_millis() as u64)
            .unwrap_or(0);

        // First absolute slot of the Shelley era.
        // For instant-transition networks (epoch 0) this is 0.
        let zero_slot = shelley_transition_epoch.saturating_mul(byron_epoch_size);

        // Wall-clock time (POSIX ms) at the Shelley era start.
        // For instant-transition networks zero_slot==0 so zero_time==network_start_ms.
        let zero_time =
            network_start_ms.saturating_add(zero_slot.saturating_mul(byron_slot_duration_ms));

        // slot_length in genesis is in seconds; SlotConfig needs milliseconds.
        let slot_length_ms = (self.slot_length * 1000) as u32;

        SlotConfig {
            zero_time,
            zero_slot,
            slot_length: slot_length_ms,
            // Per-tx safe-zone horizon is injected by the tx validator
            // (`LedgerTxValidator::validate`) using
            // `EraHistory::safe_zone_horizon_slot(ledger_tip)` immediately
            // before each `evaluate_plutus_scripts` call. The static
            // SlotConfig built here has no per-tip knowledge.
            safe_zone_horizon_slot: None,
        }
    }

    /// Convert genesis delegations to wire-format triples for N2C encoding.
    ///
    /// Each entry is (genesis_key_hash_28, delegate_key_hash_28, vrf_hash_32)
    /// as raw bytes. Entries that fail hex-decoding or have wrong lengths are
    /// skipped with a warning.
    pub fn gen_delegs_entries(&self) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let mut entries = Vec::with_capacity(self.gen_delegs.len());
        for (genesis_hash_hex, pair) in &self.gen_delegs {
            let genesis_hash = match hex::decode(genesis_hash_hex) {
                Ok(b) if b.len() == 28 => b,
                _ => {
                    warn!(
                        hash = %genesis_hash_hex,
                        "Shelley genesis: skipping genDeleg with invalid genesis key hash"
                    );
                    continue;
                }
            };
            let delegate_hash = match hex::decode(&pair.delegate) {
                Ok(b) if b.len() == 28 => b,
                _ => {
                    warn!(
                        hash = %pair.delegate,
                        "Shelley genesis: skipping genDeleg with invalid delegate hash"
                    );
                    continue;
                }
            };
            let vrf_hash = match hex::decode(&pair.vrf) {
                Ok(b) if b.len() == 32 => b,
                _ => {
                    warn!(
                        hash = %pair.vrf,
                        "Shelley genesis: skipping genDeleg with invalid VRF hash"
                    );
                    continue;
                }
            };
            entries.push((genesis_hash, delegate_hash, vrf_hash));
        }
        entries
    }

    /// Extract initial UTxO entries from Shelley genesis `initialFunds`.
    ///
    /// Each entry maps a Shelley address (bech32 or hex-encoded) to a lovelace
    /// amount. The resulting `GenesisUtxoEntry` can be fed to
    /// `seed_genesis_utxos()` since the Haskell node uses the same TxId
    /// derivation as Byron genesis: `TxId = Blake2b_256(raw_address_bytes)`,
    /// `TxIx = 0`.
    pub fn initial_utxos(&self) -> Vec<GenesisUtxoEntry> {
        let mut entries = Vec::with_capacity(self.initial_funds.len());
        for (addr_str, lovelace) in &self.initial_funds {
            // No zero-value skip, for the same reason as Byron's:
            // `Cardano/Ledger/Shelley/Genesis.hs::genesisUTxO` is an unfiltered
            // comprehension over `sgInitialFunds`, keying each TxIn on the
            // address via `initialFundsPseudoTxIn`. Omitting a zero-value entry
            // leaves an input cardano-node has and dugite does not.
            // Try bech32 first, then hex (Haskell accepts both formats)
            let address = if let Ok((_hrp, data)) = bech32::decode(addr_str) {
                data
            } else if let Ok(data) = hex::decode(addr_str) {
                data
            } else {
                warn!(
                    address = %addr_str,
                    "Shelley genesis: skipping initialFunds entry with unparseable address"
                );
                continue;
            };
            entries.push(GenesisUtxoEntry {
                address,
                lovelace: *lovelace,
            });
        }
        if !entries.is_empty() {
            let total: u64 = entries.iter().map(|e| e.lovelace).sum();
            info!(
                count = entries.len(),
                total_lovelace = total,
                "Shelley genesis: parsed initialFunds"
            );
        }
        entries
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Alonzo genesis
// ──────────────────────────────────────────────────────────────────────────

/// Alonzo genesis configuration (compatible with cardano-node alonzo-genesis.json)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlonzoGenesis {
    pub lovelace_per_u_tx_o_word: Option<u64>,
    pub execution_prices: AlonzoExPrices,
    pub max_tx_ex_units: AlonzoExUnits,
    pub max_block_ex_units: AlonzoExUnits,
    pub max_value_size: u64,
    pub collateral_percentage: u64,
    pub max_collateral_inputs: u64,
    #[serde(default)]
    pub cost_models: HashMap<String, serde_json::Value>,
    /// Haskell `agExtraConfig :: StrictMaybe AlonzoExtraConfig` (#1046).
    ///
    /// Written by `cardano-cli ... create-testnet-data`, absent from
    /// mainnet/preview/preprod. See
    /// [`AlonzoGenesis::apply_extra_config_cost_models`] for the semantics —
    /// it is a `curPParams`-only, per-language override and the ONLY way a node
    /// gets a cost model it was not handed by a genesis field or an on-chain
    /// PPU.
    #[serde(default)]
    pub extra_config: Option<AlonzoExtraConfig>,
}

/// Haskell `AlonzoExtraConfig { aecCostModels :: Maybe CostModels }`
/// (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Genesis.hs`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlonzoExtraConfig {
    #[serde(default)]
    pub cost_models: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlonzoExPrices {
    /// Older cardano-cli emits `prSteps`; cardano-cli 11.0.0.0+ emits `priceSteps`.
    #[serde(alias = "priceSteps")]
    pub pr_steps: AlonzoRational,
    /// Older cardano-cli emits `prMem`; cardano-cli 11.0.0.0+ emits `priceMemory`.
    #[serde(alias = "priceMemory")]
    pub pr_mem: AlonzoRational,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AlonzoRational {
    Struct { numerator: u64, denominator: u64 },
    Float(f64),
}

impl AlonzoRational {
    pub fn to_rational(&self) -> Rational {
        match self {
            AlonzoRational::Struct {
                numerator,
                denominator,
            } => Rational {
                numerator: *numerator,
                denominator: *denominator,
            },
            AlonzoRational::Float(f) => float_to_rational(*f),
        }
    }

    pub fn numerator(&self) -> u64 {
        self.to_rational().numerator
    }

    pub fn denominator(&self) -> u64 {
        self.to_rational().denominator
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlonzoExUnits {
    /// Older cardano-cli emits `exUnitsMem`; cardano-cli 11.0.0.0+ emits `memory`.
    #[serde(alias = "memory")]
    pub ex_units_mem: u64,
    /// Older cardano-cli emits `exUnitsSteps`; cardano-cli 11.0.0.0+ emits `steps`.
    #[serde(alias = "steps")]
    pub ex_units_steps: u64,
}

impl AlonzoGenesis {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Alonzo genesis: {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Alonzo genesis: {}", path.display()))
    }

    /// Load the genesis file and compute its Blake2b-256 hash.
    ///
    /// The hash is computed over the raw file content (canonical JSON), matching
    /// the Cardano reference implementation.
    pub fn load_with_hash(path: &Path) -> Result<(Self, dugite_primitives::hash::Hash32)> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Alonzo genesis: {}", path.display()))?;
        let genesis: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Alonzo genesis: {}", path.display()))?;
        let hash = dugite_primitives::hash::blake2b_256(content.as_bytes());
        debug!(
            genesis_hash = %hash.to_hex(),
            "Alonzo genesis hash computed"
        );
        Ok((genesis, hash))
    }

    /// Apply Alonzo genesis parameters to protocol parameters
    pub fn apply_to_protocol_params(&self, params: &mut ProtocolParameters) {
        debug!(
            max_tx_ex_mem = self.max_tx_ex_units.ex_units_mem,
            max_tx_ex_steps = self.max_tx_ex_units.ex_units_steps,
            max_val_size = self.max_value_size,
            collateral_pct = self.collateral_percentage,
            "Applying Alonzo genesis params"
        );

        // Execution unit prices
        params.execution_costs.step_price = Rational {
            numerator: self.execution_prices.pr_steps.numerator(),
            denominator: self.execution_prices.pr_steps.denominator(),
        };
        params.execution_costs.mem_price = Rational {
            numerator: self.execution_prices.pr_mem.numerator(),
            denominator: self.execution_prices.pr_mem.denominator(),
        };

        // Execution unit limits
        params.max_tx_ex_units.mem = self.max_tx_ex_units.ex_units_mem;
        params.max_tx_ex_units.steps = self.max_tx_ex_units.ex_units_steps;
        params.max_block_ex_units.mem = self.max_block_ex_units.ex_units_mem;
        params.max_block_ex_units.steps = self.max_block_ex_units.ex_units_steps;

        // Size and collateral
        params.max_val_size = self.max_value_size;
        params.collateral_percentage = self.collateral_percentage;
        params.max_collateral_inputs = self.max_collateral_inputs;

        // UTxO cost
        if let Some(lovelace_per_word) = self.lovelace_per_u_tx_o_word {
            // Convert lovelacePerUTxOWord -> adaPerUTxOByte using the exact
            // formula from cardano-ledger's Babbage translation:
            //
            //   coinsPerUTxOWordToCoinsPerUTxOByte (CoinPerWord (Coin c)) =
            //       CoinPerByte (CompactCoin (fromIntegral (c `div` 8)))
            //
            // ref: eras/babbage/impl/src/Cardano/Ledger/Babbage/PParams.hs
            //      (function `coinsPerUTxOWordToCoinsPerUTxOByte`)
            //
            // This is the canonical Haskell-side conversion, not an
            // approximation — the Alonzo genesis value is denominated in
            // lovelace per 8-byte word and Babbage exposes the per-byte rate
            // by integer division.  Babbage+ chains override this via a
            // protocol-parameter update before any UTxO costs are checked
            // against it, so the (rare, single-lovelace) rounding loss from
            // integer division is unobservable at consensus level.
            params.ada_per_utxo_byte = Lovelace(lovelace_per_word / 8);
            // Preserve the exact word-denominated value too (issue #919):
            // Alonzo's own `utxoEntrySize * coinsPerUTxOWord` minimum-UTxO
            // formula needs the LOSSLESS word value, not the lossy `/8`
            // byte-denominated derivation above (which is Babbage/Conway's
            // formula and must never be applied while PV <= 6 is in force).
            params.coins_per_utxo_word = Lovelace(lovelace_per_word);
        }

        // Cost models
        if let Some(v1_value) = self.cost_models.get("PlutusV1") {
            if let Some(costs) = parse_cost_model(v1_value) {
                debug!(count = costs.len(), "Loaded PlutusV1 cost model");
                params.cost_models.plutus_v1 = Some(costs);
            }
        }
        if let Some(v2_value) = self.cost_models.get("PlutusV2") {
            if let Some(costs) = parse_cost_model(v2_value) {
                debug!(count = costs.len(), "Loaded PlutusV2 cost model");
                params.cost_models.plutus_v2 = Some(costs);
            }
        }
        // PlutusV3 may also appear in Alonzo genesis on newer testnets
        if let Some(v3_value) = self.cost_models.get("PlutusV3") {
            if let Some(costs) = parse_cost_model(v3_value) {
                debug!(
                    count = costs.len(),
                    "Loaded PlutusV3 cost model from Alonzo genesis"
                );
                params.cost_models.plutus_v3 = Some(costs);
            }
        }
    }

    /// Apply `extraConfig.costModels` — Haskell `alonzoInjectCostModels` /
    /// `overrideCostModels` (#1046).
    ///
    /// `cardano-ledger`'s `AlonzoGenesis` carries an optional
    /// `agExtraConfig :: StrictMaybe AlonzoExtraConfig` whose sole field is
    /// `aecCostModels :: Maybe CostModels`
    /// (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Genesis.hs`). The transition
    /// config applies it in
    /// `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Transition.hs`:
    ///
    /// ```haskell
    /// alonzoInjectCostModels cfg =
    ///   case agExtraConfig $ cfg ^. tcTranslationContextL of
    ///     SNothing -> id
    ///     SJust aec -> overrideCostModels (aecCostModels aec)
    ///
    /// overrideCostModels = \case
    ///   Nothing -> id
    ///   -- Injected cost models override the era-translated ones (the
    ///   -- fixed-length PlutusV1/PlutusV3 genesis fields), so a testnet can
    ///   -- carry full cost models without an on-chain parameter update.
    ///   Just cms ->
    ///     nesEsL . curPParamsEpochStateL . ppCostModelsL
    ///       %~ flip updateCostModels (CostModelsUpdate cms)
    /// ```
    ///
    /// Two properties are load-bearing and are why this is a SEPARATE method
    /// rather than part of [`Self::apply_to_protocol_params`]:
    ///
    /// 1. **`curPParamsEpochStateL` only.** The override never touches
    ///    `prevPParams`. That is exactly why cardano-node reports
    ///    `cur = [V1, V2, V3]` but `prev = [V1, V3]` on a `create-testnet-data`
    ///    devnet — the shape #994 observed and could not explain.
    /// 2. **`updateCostModels` is a per-language update**, not a wholesale
    ///    replacement: languages named in `extraConfig` override, others are
    ///    retained.
    ///
    /// This is the ONLY mechanism by which a node acquires a cost model it was
    /// not given by a genesis field or an on-chain PPU. cardano-ledger has no
    /// "default PlutusV2" anywhere — `defaultV2CostModel` lives in **cardano-api**
    /// (`Cardano.Api.Genesis.Internal`) and is a value written INTO a generated
    /// genesis file, not a runtime injection. dugite used to hardcode that
    /// constant and inject it unconditionally, which happened to match on the
    /// devnet (where `create-testnet-data` writes the identical values into
    /// `extraConfig`) while being wrong on mainnet/preview/preprod, whose
    /// alonzo-genesis files carry no `extraConfig` at all and whose
    /// cardano-node therefore has NO PlutusV2 until a real PPU installs one.
    pub fn apply_extra_config_cost_models(&self, params: &mut ProtocolParameters) {
        let Some(extra) = self.extra_config.as_ref() else {
            return;
        };
        for (lang, value) in extra.cost_models.iter() {
            let Some(costs) = parse_cost_model(value) else {
                warn!(
                    lang = %lang,
                    "Alonzo genesis extraConfig.costModels entry could not be parsed — ignoring"
                );
                continue;
            };
            let count = costs.len();
            match lang.as_str() {
                "PlutusV1" => params.cost_models.plutus_v1 = Some(costs),
                "PlutusV2" => params.cost_models.plutus_v2 = Some(costs),
                "PlutusV3" => params.cost_models.plutus_v3 = Some(costs),
                other => {
                    warn!(
                        lang = %other,
                        "Alonzo genesis extraConfig.costModels names an unknown Plutus \
                         language — ignoring"
                    );
                    continue;
                }
            }
            debug!(
                lang = %lang,
                count,
                "Applied cost model from Alonzo genesis extraConfig (Haskell \
                 alonzoInjectCostModels — curPParams only)"
            );
        }
    }
}

/// Parse a cost model from JSON.
///
/// Cost models come in several formats:
/// - Array of integers: `[val1, val2, ...]`
/// - Indexed map: `{"key-0": val, "key-1": val, ...}` (Conway genesis)
/// - Named map: `{"paramName": val, ...}` (Alonzo genesis) — sorted alphabetically
fn parse_cost_model(value: &serde_json::Value) -> Option<Vec<i64>> {
    match value {
        serde_json::Value::Array(arr) => {
            let costs: Vec<i64> = arr.iter().filter_map(|v| v.as_i64()).collect();
            if costs.len() == arr.len() {
                Some(costs)
            } else {
                None
            }
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return None;
            }
            // Check if keys are "key-N" format (indexed)
            // Safety: map.is_empty() is checked above, so .next() always returns Some
            let first_key = map.keys().next().expect("map is non-empty (checked above)");
            if first_key.starts_with("key-") {
                let mut indexed: Vec<(usize, i64)> = Vec::new();
                for (k, v) in map {
                    if let Some(idx) = k.strip_prefix("key-").and_then(|s| s.parse::<usize>().ok())
                    {
                        if let Some(val) = v.as_i64() {
                            indexed.push((idx, val));
                        }
                    }
                }
                indexed.sort_by_key(|(idx, _)| *idx);
                Some(indexed.into_iter().map(|(_, v)| v).collect())
            } else {
                // Named parameters (Alonzo genesis format) — sort alphabetically
                let mut named: Vec<(&String, i64)> = map
                    .iter()
                    .filter_map(|(k, v)| v.as_i64().map(|val| (k, val)))
                    .collect();
                named.sort_by_key(|(k, _)| k.to_owned());
                Some(named.into_iter().map(|(_, v)| v).collect())
            }
        }
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Conway genesis
// ──────────────────────────────────────────────────────────────────────────

/// Constitution section of the Conway genesis file.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConwayGenesisConstitution {
    pub anchor: ConwayGenesisAnchor,
    /// Hex-encoded guardrail script hash (optional).
    #[serde(default)]
    pub script: Option<String>,
}

/// Anchor embedded in the Conway genesis constitution section.
#[derive(Debug, Clone, Deserialize)]
pub struct ConwayGenesisAnchor {
    pub url: String,
    #[serde(rename = "dataHash")]
    pub data_hash: String,
}

/// Conway genesis configuration (compatible with cardano-node conway-genesis.json)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConwayGenesis {
    pub pool_voting_thresholds: PoolVotingThresholds,
    #[serde(alias = "dRepVotingThresholds")]
    pub d_rep_voting_thresholds: DRepVotingThresholds,
    pub committee_min_size: u64,
    pub committee_max_term_length: u64,
    pub gov_action_lifetime: u64,
    pub gov_action_deposit: u64,
    #[serde(alias = "dRepDeposit")]
    pub d_rep_deposit: u64,
    #[serde(alias = "dRepActivity")]
    pub d_rep_activity: u64,
    #[serde(default)]
    pub min_fee_ref_script_cost_per_byte: Option<u64>,
    #[serde(default)]
    pub plutus_v3_cost_model: Option<Vec<i64>>,
    #[serde(default)]
    pub constitution: Option<ConwayGenesisConstitution>,
    /// `initialDReps` is kept as a raw `serde_json::Value` because the schema
    /// varies across networks; `initial_dreps_as_entries()` extracts typed
    /// entries defensively.
    #[serde(default, rename = "initialDReps")]
    pub initial_dreps: serde_json::Value,
    #[serde(default)]
    pub committee: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolVotingThresholds {
    pub committee_normal: f64,
    pub committee_no_confidence: f64,
    pub hard_fork_initiation: f64,
    pub motion_no_confidence: f64,
    #[serde(default)]
    pub pp_security_group: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DRepVotingThresholds {
    pub motion_no_confidence: f64,
    pub committee_normal: f64,
    pub committee_no_confidence: f64,
    pub update_to_constitution: f64,
    pub hard_fork_initiation: f64,
    #[serde(default)]
    pub pp_network_group: f64,
    #[serde(default)]
    pub pp_economic_group: f64,
    #[serde(default)]
    pub pp_technical_group: f64,
    #[serde(default)]
    pub pp_gov_group: f64,
    pub treasury_withdrawal: f64,
}

impl ConwayGenesis {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Conway genesis: {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Conway genesis: {}", path.display()))
    }

    /// Load the genesis file and compute its Blake2b-256 hash.
    ///
    /// The hash is computed over the raw file content (canonical JSON), matching
    /// the Cardano reference implementation.
    pub fn load_with_hash(path: &Path) -> Result<(Self, dugite_primitives::hash::Hash32)> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Conway genesis: {}", path.display()))?;
        let genesis: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse Conway genesis: {}", path.display()))?;
        let hash = dugite_primitives::hash::blake2b_256(content.as_bytes());
        debug!(
            genesis_hash = %hash.to_hex(),
            "Conway genesis hash computed"
        );
        Ok((genesis, hash))
    }

    /// Apply Conway genesis parameters to protocol parameters
    pub fn apply_to_protocol_params(&self, params: &mut ProtocolParameters) {
        debug!(
            drep_deposit = self.d_rep_deposit,
            drep_activity = self.d_rep_activity,
            gov_action_deposit = self.gov_action_deposit,
            gov_action_lifetime = self.gov_action_lifetime,
            committee_min_size = self.committee_min_size,
            "Applying Conway genesis params"
        );

        // Governance parameters
        params.drep_deposit = Lovelace(self.d_rep_deposit);
        params.drep_activity = self.d_rep_activity;
        params.gov_action_deposit = Lovelace(self.gov_action_deposit);
        params.gov_action_lifetime = self.gov_action_lifetime;
        params.committee_min_size = self.committee_min_size;
        params.committee_max_term_length = self.committee_max_term_length;

        // DRep voting thresholds
        let dvt = &self.d_rep_voting_thresholds;
        params.dvt_no_confidence = float_to_rational(dvt.motion_no_confidence);
        params.dvt_committee_normal = float_to_rational(dvt.committee_normal);
        params.dvt_committee_no_confidence = float_to_rational(dvt.committee_no_confidence);
        params.dvt_constitution = float_to_rational(dvt.update_to_constitution);
        params.dvt_hard_fork = float_to_rational(dvt.hard_fork_initiation);
        params.dvt_treasury_withdrawal = float_to_rational(dvt.treasury_withdrawal);
        params.dvt_pp_network_group = float_to_rational(dvt.pp_network_group);
        params.dvt_pp_economic_group = float_to_rational(dvt.pp_economic_group);
        params.dvt_pp_technical_group = float_to_rational(dvt.pp_technical_group);
        params.dvt_pp_gov_group = float_to_rational(dvt.pp_gov_group);

        if let Some(cost) = self.min_fee_ref_script_cost_per_byte {
            // Conway genesis always specifies an integer (e.g. 15); store as the
            // NonNegativeInterval cost/1.
            params.min_fee_ref_script_cost_per_byte = dugite_primitives::transaction::Rational {
                numerator: cost,
                denominator: 1,
            };
        }

        // PlutusV3 cost model from Conway genesis — ONLY when genesis itself
        // declares a Conway-or-later protocol version.
        //
        // Upstream never puts V3 into the initial parameters. It arrives with
        // the Babbage->Conway translation:
        //
        //   -- cardano-ledger, Conway/PParams.hs :: upgradeConwayPParams
        //   cppCostModels = updateCostModels bppCostModels
        //                     (mkCostModels [(PlutusV3, ucppPlutusV3CostModel)])
        //
        // a per-language INSERT over Babbage's {V1,V2}. dugite applied it at
        // STARTUP instead, so every pre-Conway epoch carried a V3 cost model
        // cardano-node does not have. MEASURED on preprod against the node's own
        // state: epochs 7-13 (Alonzo and early Babbage) read
        // `costModels {PlutusV1, PlutusV3}` where the oracle reads `{PlutusV1}`.
        // #1046's mechanism exactly, one language later — an era's genesis field
        // applied outside the era that introduces it.
        //
        // It cannot simply be deleted: `ConwayRules::on_era_transition` seeds V3
        // only for `from_era == Babbage`, and a devnet whose genesis IS Conway
        // never makes that hop, so nothing would ever seed it there — which is
        // #764's failure (empty `language_views` => wrong `script_data_hash` on
        // every V3 transaction).
        //
        // Note that gap is DUGITE'S, not upstream's. Consensus runs the whole
        // translation chain even when every fork triggers at epoch 0:
        // `injectInitialExtLedgerState` calls `State.extendToSlot ... (SlotNo 0)`,
        // which walks the telescope one era at a time and so still executes
        // Babbage->Conway (`HardFork/Combinator/Embed/Nary.hs`). dugite collapses
        // that into a single hop, which is why it needs a startup seed where
        // upstream needs none.
        //
        // The condition is DERIVED from genesis rather than pinned to a network:
        // Conway is protocol version 9, so a genesis declaring major >= 9 begins
        // in Conway and will get no Babbage->Conway translation; anything lower
        // reaches Conway through that translation and must not have V3 early.
        // Measured across the real files — mainnet 2, preprod 2, preview 6,
        // devnet 10 — so the split lands exactly on the networks that need each
        // branch.
        if let Some(v3) = &self.plutus_v3_cost_model {
            if params.protocol_version_major >= CONWAY_PROTOCOL_MAJOR {
                debug!(
                    count = v3.len(),
                    pv = params.protocol_version_major,
                    "Loaded PlutusV3 cost model from Conway genesis (genesis begins in Conway)"
                );
                params.cost_models.plutus_v3 = Some(v3.clone());
            } else {
                debug!(
                    pv = params.protocol_version_major,
                    "Conway genesis carries a PlutusV3 cost model, but genesis begins \
                     before Conway — leaving it to the Babbage->Conway translation"
                );
            }
        }

        // #1046: NO default PlutusV2 injection.
        //
        // dugite used to hardcode cardano-api's `defaultV2CostModel` here and
        // inject it whenever V2 was absent. cardano-ledger has no such default:
        // a node's cost models come from the genesis FIELDS, from
        // `agExtraConfig.aecCostModels` (see
        // `AlonzoGenesis::apply_extra_config_cost_models`), or from an on-chain
        // PPU — and nowhere else. `defaultV2CostModel` lives in cardano-api's
        // `Cardano.Api.Genesis.Internal` as a value written INTO a generated
        // genesis file, not as a runtime fallback.
        //
        // The injection was invisible on the devnet only because
        // `create-testnet-data` writes those exact 175 values into
        // `extraConfig.costModels.PlutusV2`, so the wrong mechanism produced the
        // right numbers there. On mainnet/preview/preprod there is no
        // `extraConfig` at all, so cardano-node has NO PlutusV2 until a real PPU
        // installs one, and dugite reported one it should not have had — a
        // `costModels` wire divergence, and a latent
        // accept-where-Haskell-rejects (a V2 script would EXECUTE on dugite and
        // fail on cardano-node with `CollectErrors [NoCostModel PlutusV2]`).

        // Pool voting thresholds
        let pvt = &self.pool_voting_thresholds;
        params.pvt_motion_no_confidence = float_to_rational(pvt.motion_no_confidence);
        params.pvt_committee_normal = float_to_rational(pvt.committee_normal);
        params.pvt_committee_no_confidence = float_to_rational(pvt.committee_no_confidence);
        params.pvt_hard_fork = float_to_rational(pvt.hard_fork_initiation);
        params.pvt_pp_security_group = float_to_rational(pvt.pp_security_group);
    }

    /// Extract the committee quorum threshold from Conway genesis.
    /// Returns (numerator, denominator) if the committee section has a threshold.
    pub fn committee_threshold(&self) -> Option<(u64, u64)> {
        let committee = self.committee.as_ref()?;
        let threshold = committee.get("threshold")?;
        let num = threshold.get("numerator")?.as_u64()?;
        let den = threshold.get("denominator")?.as_u64()?;
        Some((num, den))
    }

    /// Extract committee members from Conway genesis.
    ///
    /// Returns a list of (credential_hash_bytes, expiration_epoch) pairs.
    /// Keys in genesis are formatted as "scriptHash-<hex>" or "keyHash-<hex>".
    pub fn committee_members(&self) -> Vec<([u8; 32], u64)> {
        let committee = match self.committee.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let members = match committee.get("members").and_then(|m| m.as_object()) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut result = Vec::new();
        for (key, expiry) in members {
            let expiration = match expiry.as_u64() {
                Some(e) => e,
                None => continue,
            };
            // Parse "scriptHash-<hex>" or "keyHash-<hex>" format
            let (hex_str, is_script) = if let Some(h) = key.strip_prefix("scriptHash-") {
                (h, true)
            } else if let Some(h) = key.strip_prefix("keyHash-") {
                (h, false)
            } else {
                continue;
            };
            if let Ok(bytes) = hex::decode(hex_str) {
                // Committee credentials are 28 bytes; pad to 32 for our Hash32 representation.
                // Byte 28 encodes the credential type: 0x00=key, 0x01=script
                // (matching Credential::to_typed_hash32).
                let mut hash = [0u8; 32];
                let len = bytes.len().min(28);
                hash[..len].copy_from_slice(&bytes[..len]);
                if is_script {
                    hash[28] = 0x01;
                }
                result.push((hash, expiration));
            }
        }
        result
    }

    /// Convert the parsed constitution into the ledger's [`Constitution`] type.
    /// Returns `None` if no constitution is declared in genesis or the encoded
    /// hashes fail to parse.
    pub fn to_ledger_constitution(&self) -> Option<dugite_primitives::transaction::Constitution> {
        use dugite_primitives::hash::{Hash28, Hash32};
        use dugite_primitives::transaction::{Anchor, Constitution};
        let cg = self.constitution.as_ref()?;
        let data_hash = Hash32::from_hex(&cg.anchor.data_hash).ok()?;
        let script_hash = cg.script.as_ref().and_then(|s| Hash28::from_hex(s).ok());
        Some(Constitution {
            anchor: Anchor {
                url: cg.anchor.url.clone(),
                data_hash,
            },
            script_hash,
        })
    }

    /// Extract initial DReps as `(credential_hash28, deposit)` pairs.
    ///
    /// Returns an empty `Vec` if `initialDReps` is absent or schema-mismatched.
    /// The `initialDReps` JSON is a map from hex-encoded credential hashes to
    /// an object containing at least a `deposit` field. Anchor parsing is
    /// omitted for now (preview/preprod genesis has no anchors on initial
    /// DReps, so there is no verified schema to parse against).
    pub fn initial_dreps_as_entries(&self) -> Vec<(dugite_primitives::hash::Hash28, u64)> {
        use dugite_primitives::hash::Hash28;
        let Some(obj) = self.initial_dreps.as_object() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (hex_cred, entry) in obj {
            let Ok(cred) = Hash28::from_hex(hex_cred) else {
                continue;
            };
            let deposit = entry.get("deposit").and_then(|v| v.as_u64()).unwrap_or(0);
            out.push((cred, deposit));
        }
        out
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Dijkstra genesis
// ──────────────────────────────────────────────────────────────────────────

// The parsed shape lives in `dugite-primitives::genesis::dijkstra` so other
// crates (`dugite-ledger`, tests) can consume it without depending on
// `dugite-node`.  This module only provides the file-loading wrapper that
// mirrors `AlonzoGenesis::load_with_hash` / `ConwayGenesis::load_with_hash`.

pub use dugite_primitives::genesis::DijkstraGenesis;

/// File-system loader for `dijkstra-genesis.json` that also returns the
/// Blake2b-256 hash of the raw file bytes (canonical JSON), matching the
/// Cardano reference implementation's genesis-file hashing convention.
pub fn load_dijkstra_genesis_with_hash(
    path: &Path,
) -> Result<(DijkstraGenesis, dugite_primitives::hash::Hash32)> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read Dijkstra genesis: {}", path.display()))?;
    let genesis = DijkstraGenesis::from_json_str(&content)
        .with_context(|| format!("Failed to parse Dijkstra genesis: {}", path.display()))?;
    let hash = dugite_primitives::hash::blake2b_256(content.as_bytes());
    debug!(
        genesis_hash = %hash.to_hex(),
        "Dijkstra genesis hash computed"
    );
    Ok((genesis, hash))
}

/// Convert a JSON genesis number to the EXACT rational it denotes.
///
/// Haskell parses these fields (`NonNegativeInterval`, `UnitInterval`) from
/// JSON via `Scientific`, which is an exact decimal — `0.0000721` becomes
/// `721 % 10000000`, not an approximation. dugite must match, because every
/// one of these values is consensus-critical:
///
/// * `executionPrices.priceSteps` / `priceMemory` — Plutus script fees, so the
///   min-fee of every script transaction
/// * `rho`, `tau` — monetary expansion and treasury cut, i.e. the reward pot
/// * `a0` — pool reward saturation
/// * `decentralisationParam` — leader election
/// * every `dvt_*` / `pvt_*` governance voting threshold
///
/// This used to force a denominator of 1_000_000 and round into it, which is
/// exact only for values expressible in millionths. Mainnet's real
/// `priceSteps` is `0.0000721`: `round(0.0000721 * 1e6) = 72`, giving
/// `9/125000 = 0.000072` — a 0.14% error in the steps price, silently applied
/// to every script fee dugite computed.
///
/// f64 Display in Rust emits the shortest decimal string that round-trips to
/// the same f64, which reproduces the literal the genesis file contained, so
/// parsing that string recovers the intended decimal exactly.
fn float_to_rational(f: f64) -> Rational {
    if f == 0.0 || !f.is_finite() {
        return Rational {
            numerator: 0,
            denominator: 1,
        };
    }

    let s = format!("{f}");
    if let Some(r) = decimal_str_to_rational(&s) {
        return r;
    }

    // Fallback for shapes the decimal parser cannot represent in u64 (absurd
    // exponents). Previous behaviour, which is at least bounded.
    let den = 1_000_000u64;
    let num = (f * den as f64).round() as u64;
    let g = gcd(num, den).max(1);
    Rational {
        numerator: num / g,
        denominator: den / g,
    }
}

/// Parse a plain or exponential decimal string into an exact reduced rational.
///
/// Returns `None` if the value cannot be represented in `u64/u64` (in which
/// case the caller falls back), or if the string is not a number we recognise.
fn decimal_str_to_rational(s: &str) -> Option<Rational> {
    let s = s.trim();
    // Negative values are not valid for any of these genesis fields.
    if s.starts_with('-') {
        return None;
    }
    let (mantissa, exp) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (s, 0i32),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }

    let digits = format!("{int_part}{frac_part}");
    let mut numerator: u128 = digits.parse().ok()?;
    // Scale of the mantissa, then apply the exponent.
    let scale = frac_part.len() as i32 - exp;
    let mut denominator: u128 = 1;
    if scale >= 0 {
        denominator = 10u128.checked_pow(u32::try_from(scale).ok()?)?;
    } else {
        numerator = numerator.checked_mul(10u128.checked_pow(u32::try_from(-scale).ok()?)?)?;
    }

    let g = gcd_u128(numerator, denominator).max(1);
    let numerator = numerator / g;
    let denominator = denominator / g;
    Some(Rational {
        numerator: u64::try_from(numerator).ok()?,
        denominator: u64::try_from(denominator).ok()?,
    })
}

fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

#[cfg(test)]
mod float_to_rational_tests {
    use super::*;

    /// Genesis intervals must convert to the EXACT rational the decimal
    /// denotes, matching Haskell's `Scientific`-based JSON parsing.
    ///
    /// The regression: a hardcoded 1_000_000 denominator turned mainnet's real
    /// `priceSteps = 0.0000721` into `9/125000 = 0.000072`, a 0.14% error
    /// applied to every Plutus script fee dugite computed.
    #[test]
    fn converts_genesis_decimals_exactly() {
        let cases: &[(f64, u64, u64)] = &[
            // The bug: 7 decimal places, not expressible in millionths.
            (0.0000721, 721, 10_000_000),
            // Mainnet / devnet values that were already correct.
            (0.0577, 577, 10_000),
            (0.003, 3, 1_000),
            (0.2, 1, 5),
            (0.3, 3, 10),
            (0.05, 1, 20),
            (0.51, 51, 100),
            (0.67, 67, 100),
            (0.75, 3, 4),
            // Integers and unit bounds.
            (1.0, 1, 1),
            (0.0, 0, 1),
            // Finer precision than millionths in a governance threshold.
            (0.5100001, 5_100_001, 10_000_000),
        ];
        for &(input, num, den) in cases {
            let r = float_to_rational(input);
            assert_eq!(
                (r.numerator, r.denominator),
                (num, den),
                "float_to_rational({input}) must be exactly {num}/{den}, got {}/{}",
                r.numerator,
                r.denominator
            );
        }
    }

    /// Every converted value must round-trip to the original decimal.
    #[test]
    fn conversion_is_lossless_for_genesis_shaped_values() {
        for input in [0.0000721_f64, 0.0577, 0.003, 0.2, 0.3, 0.05, 0.51, 0.67] {
            let r = float_to_rational(input);
            let back = r.numerator as f64 / r.denominator as f64;
            assert!(
                (back - input).abs() < f64::EPSILON * input.max(1.0),
                "{input} round-tripped to {back} via {}/{}",
                r.numerator,
                r.denominator
            );
        }
    }

    #[test]
    fn handles_exponential_notation() {
        // 7.21e-5 is the same value as 0.0000721.
        assert_eq!(
            decimal_str_to_rational("7.21e-5"),
            Some(Rational {
                numerator: 721,
                denominator: 10_000_000
            })
        );
        assert_eq!(
            decimal_str_to_rational("5e-1"),
            Some(Rational {
                numerator: 1,
                denominator: 2
            })
        );
    }

    #[test]
    fn rejects_non_numeric_and_negative() {
        assert_eq!(decimal_str_to_rational("-0.5"), None);
        assert_eq!(decimal_str_to_rational("abc"), None);
        assert_eq!(decimal_str_to_rational("0.1.2"), None);
    }
}

#[cfg(test)]
mod byron_fee_policy_tests {
    use super::*;

    /// The values mainnet AND preprod both carry, verbatim from their
    /// `byron-genesis.json`. Pinned as a FIXTURE of what the files say, not as
    /// the policy dugite uses — the point of the change these test is that the
    /// policy comes from the file.
    const REAL_SUMMAND_NANO: &str = "155381000000000";
    const REAL_MULTIPLIER_NANO: &str = "43946000000";

    fn policy(summand: &str, multiplier: &str) -> ByronTxFeePolicy {
        ByronTxFeePolicy {
            summand: summand.to_string(),
            multiplier: multiplier.to_string(),
        }
    }

    /// The de-scaling reproduces Byron's documented `a = 155381` and
    /// `b = 21973/500`, which is what fixes the 1e9-vs-1e12 question the struct's
    /// old comment got wrong.
    #[test]
    fn real_genesis_descales_to_byrons_documented_a_and_b() {
        let (summand, (num, den)) = policy(REAL_SUMMAND_NANO, REAL_MULTIPLIER_NANO)
            .to_exact()
            .expect("the real mainnet/preprod policy must parse");
        assert_eq!(summand, 155_381, "a, in lovelace");
        assert_eq!((num, den), (21_973, 500), "b, exact and in lowest terms");
        // 21973/500 == 43.946, cross-multiplied so the check stays in integers:
        // a float comparison could pass on a value that merely rounds the same.
        assert_eq!(num * 1_000, den * 43_946, "b must be exactly 43.946");
    }

    /// The scale is 1e9. If it were 1e12 the summand would de-scale to 155
    /// lovelace instead of 155381 — a thousandfold-too-small minimum fee, i.e.
    /// accepting transactions cardano-node rejects. This is the assertion that
    /// goes red if anyone "corrects" the scale back to the old comment's claim.
    #[test]
    fn the_scale_is_nano_not_pico() {
        let (summand, _) = policy(REAL_SUMMAND_NANO, REAL_MULTIPLIER_NANO)
            .to_exact()
            .unwrap();
        assert_eq!(summand, 155_381);
        assert_ne!(summand, 155, "155 is what a 1e12 scale would produce");
    }

    /// A fractional summand is REFUSED rather than truncated. Byron's `a` is a
    /// `Lovelace`; rounding one silently would put a wrong fee on a consensus
    /// path, which is precisely what a pinned constant did before.
    #[test]
    fn a_non_integral_summand_is_refused_not_rounded() {
        assert_eq!(
            policy("155381000000001", REAL_MULTIPLIER_NANO).to_exact(),
            None
        );
        // And the boundary just below, so the check is not accidentally lenient.
        assert_eq!(policy("999999999", REAL_MULTIPLIER_NANO).to_exact(), None);
    }

    /// A multiplier that is a whole number still reduces correctly, and one that
    /// is not stays exact — no float anywhere in the path.
    #[test]
    fn multiplier_stays_exact_whole_or_fractional() {
        // 2e9 / 1e9 = 2/1
        let (_, mult) = policy(REAL_SUMMAND_NANO, "2000000000").to_exact().unwrap();
        assert_eq!(mult, (2, 1));
        // 1 / 1e9 — the smallest representable, must NOT collapse to 0
        let (_, mult) = policy(REAL_SUMMAND_NANO, "1").to_exact().unwrap();
        assert_eq!(mult, (1, 1_000_000_000));
    }

    #[test]
    fn garbage_and_empty_are_refused() {
        assert_eq!(policy("", REAL_MULTIPLIER_NANO).to_exact(), None);
        assert_eq!(policy(REAL_SUMMAND_NANO, "not-a-number").to_exact(), None);
        assert_eq!(ByronTxFeePolicy::default().to_exact(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conway_genesis_parses_constitution() {
        let json = r#"{
            "poolVotingThresholds": {
                "committeeNormal": 0.51, "committeeNoConfidence": 0.51,
                "hardForkInitiation": 0.51, "motionNoConfidence": 0.51,
                "ppSecurityGroup": 0.51
            },
            "dRepVotingThresholds": {
                "motionNoConfidence": 0.67, "committeeNormal": 0.67,
                "committeeNoConfidence": 0.6, "updateToConstitution": 0.75,
                "hardForkInitiation": 0.6, "ppNetworkGroup": 0.67,
                "ppEconomicGroup": 0.67, "ppTechnicalGroup": 0.67,
                "ppGovGroup": 0.75, "treasuryWithdrawal": 0.67
            },
            "committeeMinSize": 0, "committeeMaxTermLength": 365,
            "govActionLifetime": 6, "govActionDeposit": 1000000000,
            "dRepDeposit": 500000000, "dRepActivity": 20,
            "constitution": {
                "anchor": {
                    "url": "https://example.com/constitution.md",
                    "dataHash": "ca41a91f399259bcefe57f9858e91f6d00e1a38d6d9c63d4052914ea7bd70cb2"
                }
            }
        }"#;
        let genesis: ConwayGenesis = serde_json::from_str(json).unwrap();
        let ledger_const = genesis
            .to_ledger_constitution()
            .expect("constitution parsed");
        assert_eq!(
            ledger_const.anchor.url,
            "https://example.com/constitution.md"
        );
        assert!(ledger_const.script_hash.is_none());
        // initialDReps absent → empty
        assert!(genesis.initial_dreps_as_entries().is_empty());
    }

    /// A Conway genesis carrying a PlutusV3 cost model, minimal but complete
    /// enough to deserialise.
    fn conway_genesis_with_v3() -> ConwayGenesis {
        let json = r#"{
            "poolVotingThresholds": {
                "committeeNormal": 0.51, "committeeNoConfidence": 0.51,
                "hardForkInitiation": 0.51, "motionNoConfidence": 0.51,
                "ppSecurityGroup": 0.51
            },
            "dRepVotingThresholds": {
                "motionNoConfidence": 0.67, "committeeNormal": 0.67,
                "committeeNoConfidence": 0.6, "updateToConstitution": 0.75,
                "hardForkInitiation": 0.6, "ppNetworkGroup": 0.67,
                "ppEconomicGroup": 0.67, "ppTechnicalGroup": 0.67,
                "ppGovGroup": 0.75, "treasuryWithdrawal": 0.67
            },
            "committeeMinSize": 0, "committeeMaxTermLength": 365,
            "govActionLifetime": 6, "govActionDeposit": 1000000000,
            "dRepDeposit": 500000000, "dRepActivity": 20,
            "plutusV3CostModel": [11, 22, 33]
        }"#;
        serde_json::from_str(json).expect("conway genesis parses")
    }

    /// A genesis that begins BEFORE Conway must not carry a PlutusV3 cost model
    /// in its initial parameters. Upstream inserts V3 in `upgradeConwayPParams`,
    /// i.e. at the Babbage->Conway translation, so any earlier epoch has none.
    ///
    /// RED under the previous code, which applied the field unconditionally:
    /// measured on preprod, epochs 7-13 read `{PlutusV1, PlutusV3}` against
    /// cardano-node's `{PlutusV1}`.
    #[test]
    fn conway_genesis_v3_cost_model_is_withheld_before_conway() {
        let g = conway_genesis_with_v3();
        // mainnet and preprod declare major 2; preview declares 6.
        for pv in [2u64, 6, 8] {
            let mut params = ProtocolParameters::mainnet_defaults();
            params.protocol_version_major = pv;
            params.cost_models.plutus_v3 = None;

            g.apply_to_protocol_params(&mut params);

            assert_eq!(
                params.cost_models.plutus_v3, None,
                "genesis at protocol major {pv} begins before Conway, so V3 must \
                 wait for the Babbage->Conway translation that introduces it"
            );
        }
    }

    /// The other half, and the reason the field cannot simply be dropped:
    /// `ConwayRules::on_era_transition` seeds V3 only for `from_era == Babbage`,
    /// so a devnet whose genesis IS Conway never makes that hop. Withholding it
    /// there would leave `language_views` empty and give every PlutusV3
    /// transaction a wrong `script_data_hash` — #764's failure.
    #[test]
    fn conway_genesis_v3_cost_model_is_applied_when_genesis_begins_in_conway() {
        let g = conway_genesis_with_v3();
        // `create-testnet-data` writes major 10.
        for pv in [CONWAY_PROTOCOL_MAJOR, 10, 11] {
            let mut params = ProtocolParameters::mainnet_defaults();
            params.protocol_version_major = pv;
            params.cost_models.plutus_v3 = None;

            g.apply_to_protocol_params(&mut params);

            assert_eq!(
                params.cost_models.plutus_v3,
                Some(vec![11, 22, 33]),
                "genesis at protocol major {pv} begins in Conway, so nothing else \
                 will ever seed V3"
            );
        }
    }

    #[test]
    fn test_float_to_rational() {
        let r = float_to_rational(0.3);
        assert_eq!(r.numerator, 3);
        assert_eq!(r.denominator, 10);

        let r = float_to_rational(0.05);
        assert_eq!(r.numerator, 1);
        assert_eq!(r.denominator, 20);

        let r = float_to_rational(0.003);
        assert_eq!(r.numerator, 3);
        assert_eq!(r.denominator, 1000);
    }

    #[test]
    fn test_parse_alonzo_genesis() {
        let json = r#"{
            "lovelacePerUTxOWord": 34482,
            "executionPrices": {
                "prSteps": { "numerator": 721, "denominator": 10000000 },
                "prMem": { "numerator": 577, "denominator": 10000 }
            },
            "maxTxExUnits": { "exUnitsMem": 10000000, "exUnitsSteps": 10000000000 },
            "maxBlockExUnits": { "exUnitsMem": 50000000, "exUnitsSteps": 40000000000 },
            "maxValueSize": 5000,
            "collateralPercentage": 150,
            "maxCollateralInputs": 3,
            "costModels": {
                "PlutusV1": {}
            }
        }"#;

        let genesis: AlonzoGenesis = serde_json::from_str(json).unwrap();
        assert_eq!(genesis.max_value_size, 5000);
        assert_eq!(genesis.collateral_percentage, 150);
        assert_eq!(genesis.max_collateral_inputs, 3);
        assert_eq!(genesis.max_tx_ex_units.ex_units_mem, 10000000);
        assert_eq!(genesis.max_block_ex_units.ex_units_steps, 40000000000);
        assert_eq!(genesis.execution_prices.pr_steps.numerator(), 721);
        assert_eq!(genesis.execution_prices.pr_mem.denominator(), 10000);

        let mut pp = ProtocolParameters::mainnet_defaults();
        genesis.apply_to_protocol_params(&mut pp);
        assert_eq!(pp.max_val_size, 5000);
        assert_eq!(pp.collateral_percentage, 150);
        assert_eq!(pp.max_tx_ex_units.mem, 10000000);
        assert_eq!(pp.execution_costs.step_price.numerator, 721);
    }

    #[test]
    fn test_parse_conway_genesis() {
        let json = r#"{
            "poolVotingThresholds": {
                "committeeNormal": 0.51,
                "committeeNoConfidence": 0.51,
                "hardForkInitiation": 0.51,
                "motionNoConfidence": 0.51,
                "ppSecurityGroup": 0.51
            },
            "dRepVotingThresholds": {
                "motionNoConfidence": 0.67,
                "committeeNormal": 0.67,
                "committeeNoConfidence": 0.6,
                "updateToConstitution": 0.75,
                "hardForkInitiation": 0.6,
                "ppNetworkGroup": 0.67,
                "ppEconomicGroup": 0.67,
                "ppTechnicalGroup": 0.67,
                "ppGovGroup": 0.75,
                "treasuryWithdrawal": 0.67
            },
            "committeeMinSize": 7,
            "committeeMaxTermLength": 146,
            "govActionLifetime": 6,
            "govActionDeposit": 100000000000,
            "dRepDeposit": 500000000,
            "dRepActivity": 20,
            "minFeeRefScriptCostPerByte": 15
        }"#;

        let genesis: ConwayGenesis = serde_json::from_str(json).unwrap();
        assert_eq!(genesis.committee_min_size, 7);
        assert_eq!(genesis.d_rep_deposit, 500000000);
        assert_eq!(genesis.gov_action_deposit, 100000000000);
        assert_eq!(genesis.d_rep_activity, 20);

        let mut pp = ProtocolParameters::mainnet_defaults();
        genesis.apply_to_protocol_params(&mut pp);
        assert_eq!(pp.drep_deposit, Lovelace(500000000));
        assert_eq!(pp.gov_action_deposit, Lovelace(100000000000));
        assert_eq!(pp.committee_min_size, 7);
        assert_eq!(pp.committee_max_term_length, 146);
        // DRep voting thresholds
        assert_eq!(pp.dvt_constitution.numerator, 3);
        assert_eq!(pp.dvt_constitution.denominator, 4); // 0.75

        // No committee section → empty members and no threshold
        assert!(genesis.committee_threshold().is_none());
        assert!(genesis.committee_members().is_empty());
    }

    #[test]
    fn test_conway_genesis_committee_members() {
        let json = r#"{
            "poolVotingThresholds": {
                "committeeNormal": 0.51, "committeeNoConfidence": 0.51,
                "hardForkInitiation": 0.51, "motionNoConfidence": 0.51, "ppSecurityGroup": 0.51
            },
            "dRepVotingThresholds": {
                "motionNoConfidence": 0.67, "committeeNormal": 0.67, "committeeNoConfidence": 0.6,
                "updateToConstitution": 0.75, "hardForkInitiation": 0.6, "ppNetworkGroup": 0.67,
                "ppEconomicGroup": 0.67, "ppTechnicalGroup": 0.67, "ppGovGroup": 0.75,
                "treasuryWithdrawal": 0.67
            },
            "committeeMinSize": 1,
            "committeeMaxTermLength": 146,
            "govActionLifetime": 6,
            "govActionDeposit": 100000000,
            "dRepDeposit": 500000000,
            "dRepActivity": 20,
            "committee": {
                "members": {
                    "scriptHash-ff9babf23fef3f54ec29132c07a8e23807d7b395b143ecd8ff79f4c7": 1000,
                    "keyHash-aabbccdd00112233445566778899aabbccddeeff00112233445566778899aabb": 500
                },
                "threshold": { "numerator": 2, "denominator": 3 }
            }
        }"#;

        let genesis: ConwayGenesis = serde_json::from_str(json).unwrap();

        // Threshold
        let (num, den) = genesis.committee_threshold().unwrap();
        assert_eq!(num, 2);
        assert_eq!(den, 3);

        // Members
        let members = genesis.committee_members();
        assert_eq!(members.len(), 2);

        // Check the scriptHash member (28-byte credential padded to 32)
        let script_hash_hex = "ff9babf23fef3f54ec29132c07a8e23807d7b395b143ecd8ff79f4c7";
        let expected_bytes = hex::decode(script_hash_hex).unwrap();
        let found = members.iter().any(|(hash, exp)| {
            hash[..28] == expected_bytes[..]
                && hash[28] == 0x01
                && hash[29..] == [0, 0, 0]
                && *exp == 1000
        });
        assert!(
            found,
            "scriptHash member not found with correct expiration and type byte"
        );

        // Check keyHash member
        let found_key = members.iter().any(|(_, exp)| *exp == 500);
        assert!(
            found_key,
            "keyHash member not found with correct expiration"
        );
    }

    #[test]
    fn test_parse_byron_genesis() {
        let json = r#"{
            "avvmDistr": {
                "Y2FyZGFubyBpcyBhd2Vzb21l": "1000000"
            },
            "nonAvvmBalances": {
                "37btjrVyb4KEB2STADSsj3MYSAdj52X9FgGzKZEiHbsyZH1r39ZZRH6FvkSRMxaVBMPKknvEPYhHPV1Qgr6FSNLF1sfhaMQ4bDYB2Y3FNkPZCz": "3333000000",
                "2cWKMJemoBajcwN6kT4oHXBH5JTwHtCFhVYKDRAS1QbjKZJj8GUZPF7v9G5DxaJfmUqidz": "999000000"
            },
            "bootStakeholders": {},
            "heavyDelegation": {},
            "startTime": 1654041600,
            "blockVersionData": {
                "slotDuration": "20000",
                "maxBlockSize": "2000000",
                "maxTxSize": "4096",
                "txFeePolicy": {
                    "summand": "155381000000000",
                    "multiplier": "43946000000"
                }
            },
            "protocolConsts": {
                "k": 2160,
                "protocolMagic": 764824073
            }
        }"#;

        let genesis: ByronGenesis = serde_json::from_str(json).unwrap();
        assert_eq!(genesis.protocol_magic(), 764824073);
        assert_eq!(genesis.security_param(), 2160);
        assert_eq!(genesis._start_time, 1654041600);
        assert_eq!(genesis.non_avvm_balances.len(), 2);
        assert_eq!(genesis.avvm_distr.len(), 1);
        assert_eq!(genesis.block_version_data.slot_duration, "20000");
        assert_eq!(genesis.block_version_data.max_block_size, "2000000");

        // Test initial_utxos extraction
        let utxos = genesis.initial_utxos();
        assert_eq!(utxos.len(), 2);
        // Verify lovelace amounts
        let total: u64 = utxos.iter().map(|e| e.lovelace).sum();
        assert_eq!(total, 3333000000 + 999000000);
    }

    #[test]
    fn test_parse_shelley_genesis() {
        let json = r#"{
            "networkMagic": 2,
            "networkId": "Testnet",
            "systemStart": "2022-10-25T00:00:00Z",
            "activeSlotsCoeff": 0.05,
            "securityParam": 432,
            "epochLength": 86400,
            "slotLength": 1,
            "maxLovelaceSupply": 45000000000000000,
            "maxKESEvolutions": 62,
            "slotsPerKESPeriod": 129600,
            "updateQuorum": 5,
            "protocolParams": {
                "minFeeA": 44,
                "minFeeB": 155381,
                "maxBlockBodySize": 65536,
                "maxTxSize": 16384,
                "maxBlockHeaderSize": 1100,
                "keyDeposit": 2000000,
                "poolDeposit": 500000000,
                "eMax": 18,
                "nOpt": 150,
                "a0": 0.3,
                "rho": 0.003,
                "tau": 0.2,
                "minPoolCost": 340000000,
                "minUTxOValue": 1000000,
                "protocolVersion": { "major": 6, "minor": 0 }
            }
        }"#;

        let genesis: ShelleyGenesis = serde_json::from_str(json).unwrap();
        assert_eq!(genesis.network_magic, 2);
        assert_eq!(genesis.system_start, "2022-10-25T00:00:00Z");
        assert_eq!(genesis.active_slots_coeff, 0.05);
        assert_eq!(genesis.epoch_length, 86400);
        assert_eq!(genesis.protocol_params.n_opt, 150);
        assert_eq!(genesis.protocol_params.min_pool_cost, 340000000);

        // Apply to protocol params
        let mut pp = ProtocolParameters::mainnet_defaults();
        genesis.apply_to_protocol_params(&mut pp);
        assert_eq!(pp.n_opt, 150);
        assert_eq!(pp.min_pool_cost, Lovelace(340000000));
        assert_eq!(pp.max_block_body_size, 65536);
    }

    #[test]
    fn test_byron_genesis_load_with_hash() {
        // Write a temporary Byron genesis JSON file and verify load_with_hash
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("byron-genesis.json");
        let json = r#"{
            "avvmDistr": {},
            "nonAvvmBalances": {},
            "bootStakeholders": {},
            "heavyDelegation": {},
            "startTime": 1654041600,
            "blockVersionData": {
                "slotDuration": "20000",
                "maxBlockSize": "2000000",
                "maxTxSize": "4096",
                "txFeePolicy": { "summand": "155381000000000", "multiplier": "43946000000" }
            },
            "protocolConsts": { "k": 2160, "protocolMagic": 764824073 }
        }"#;
        std::fs::write(&path, json).unwrap();

        let (genesis, hash) = ByronGenesis::load_with_hash(&path).unwrap();
        assert_eq!(genesis.protocol_magic(), 764824073);
        assert_eq!(genesis.security_param(), 2160);

        // Hash should be deterministic for the same content
        let expected = dugite_primitives::hash::blake2b_256(json.as_bytes());
        assert_eq!(hash, expected);

        // Hash should be non-zero
        assert_ne!(hash, dugite_primitives::hash::Hash32::ZERO);
    }

    #[test]
    fn test_shelley_genesis_load_with_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shelley-genesis.json");
        let json = r#"{
            "networkMagic": 2,
            "networkId": "Testnet",
            "systemStart": "2022-10-25T00:00:00Z",
            "activeSlotsCoeff": 0.05,
            "securityParam": 432,
            "epochLength": 86400,
            "slotLength": 1,
            "maxLovelaceSupply": 45000000000000000,
            "maxKESEvolutions": 62,
            "slotsPerKESPeriod": 129600,
            "updateQuorum": 5,
            "protocolParams": {
                "minFeeA": 44,
                "minFeeB": 155381,
                "maxBlockBodySize": 65536,
                "maxTxSize": 16384,
                "maxBlockHeaderSize": 1100,
                "keyDeposit": 2000000,
                "poolDeposit": 500000000,
                "eMax": 18,
                "nOpt": 150,
                "a0": 0.3,
                "rho": 0.003,
                "tau": 0.2,
                "minPoolCost": 340000000,
                "minUTxOValue": 1000000,
                "protocolVersion": { "major": 6, "minor": 0 }
            }
        }"#;
        std::fs::write(&path, json).unwrap();

        let (genesis, hash) = ShelleyGenesis::load_with_hash(&path).unwrap();
        assert_eq!(genesis.network_magic, 2);

        // Hash should be deterministic
        let expected = dugite_primitives::hash::blake2b_256(json.as_bytes());
        assert_eq!(hash, expected);
        assert_ne!(hash, dugite_primitives::hash::Hash32::ZERO);
    }

    #[test]
    fn test_genesis_hash_differs_between_files() {
        let dir = tempfile::tempdir().unwrap();

        let path1 = dir.path().join("genesis1.json");
        let json1 = r#"{
            "avvmDistr": {},
            "nonAvvmBalances": {},
            "bootStakeholders": {},
            "heavyDelegation": {},
            "startTime": 1654041600,
            "blockVersionData": {
                "slotDuration": "20000",
                "maxBlockSize": "2000000",
                "maxTxSize": "4096",
                "txFeePolicy": { "summand": "155381000000000", "multiplier": "43946000000" }
            },
            "protocolConsts": { "k": 2160, "protocolMagic": 764824073 }
        }"#;
        std::fs::write(&path1, json1).unwrap();

        let path2 = dir.path().join("genesis2.json");
        let json2 = r#"{
            "avvmDistr": {},
            "nonAvvmBalances": {},
            "bootStakeholders": {},
            "heavyDelegation": {},
            "startTime": 1654041600,
            "blockVersionData": {
                "slotDuration": "20000",
                "maxBlockSize": "2000000",
                "maxTxSize": "4096",
                "txFeePolicy": { "summand": "155381000000000", "multiplier": "43946000000" }
            },
            "protocolConsts": { "k": 2160, "protocolMagic": 1 }
        }"#;
        std::fs::write(&path2, json2).unwrap();

        let (_, hash1) = ByronGenesis::load_with_hash(&path1).unwrap();
        let (_, hash2) = ByronGenesis::load_with_hash(&path2).unwrap();

        // Different genesis files must produce different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_avvm_to_address_produces_valid_byron_address() {
        // Use a known AVVM key from mainnet genesis
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let addr_bytes = ByronGenesis::avvm_to_address(pubkey_b64, 764824073).unwrap();

        // Should be valid CBOR that decodes as a Byron address
        let payload =
            dugite_primitives::address::byron::ByronAddressPayload::from_wire_bytes(&addr_bytes)
                .unwrap();

        // Redeem address type
        assert_eq!(
            payload.addr_type,
            dugite_primitives::address::byron::ByronAddrType::Redeem
        );
        // Address root should be 28 bytes
        assert_eq!(payload.root.len(), 28);
    }

    #[test]
    fn test_avvm_to_address_mainnet_no_network_tag() {
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let addr_bytes = ByronGenesis::avvm_to_address(pubkey_b64, 764824073).unwrap();

        let payload =
            dugite_primitives::address::byron::ByronAddressPayload::from_wire_bytes(&addr_bytes)
                .unwrap();

        // Mainnet should have empty attributes (CBOR-encoded empty map = 0xa0)
        assert_eq!(
            payload.attributes.as_slice(),
            &[0xa0],
            "Mainnet AVVM address should have empty attributes map"
        );
    }

    #[test]
    fn test_avvm_to_address_testnet_has_network_tag() {
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        // Preview testnet magic = 2
        let addr_bytes = ByronGenesis::avvm_to_address(pubkey_b64, 2).unwrap();

        let payload =
            dugite_primitives::address::byron::ByronAddressPayload::from_wire_bytes(&addr_bytes)
                .unwrap();

        // Testnet should have non-empty attributes (network tag attribute present)
        assert!(
            !payload.attributes.is_empty() && payload.attributes != [0xa0],
            "Testnet AVVM address should have network tag, got attributes={:?}",
            payload.attributes
        );
    }

    #[test]
    fn test_avvm_tx_hash_deterministic() {
        let pubkey_b64 = "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=";
        let addr_bytes = ByronGenesis::avvm_to_address(pubkey_b64, 764824073).unwrap();

        // The tx hash should be blake2b_256 of the address bytes
        let tx_hash = dugite_primitives::hash::blake2b_256(&addr_bytes);

        // Re-derive and check determinism
        let addr_bytes2 = ByronGenesis::avvm_to_address(pubkey_b64, 764824073).unwrap();
        let tx_hash2 = dugite_primitives::hash::blake2b_256(&addr_bytes2);

        assert_eq!(tx_hash, tx_hash2, "AVVM tx hash must be deterministic");
    }

    #[test]
    fn test_avvm_genesis_initial_utxos() {
        let json = r#"{
            "avvmDistr": {
                "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=": "9999300000000",
                "-0Np4pyTOWF26iXWVIvu6fhz9QupwWRS2hcCaOEYlw0=": "3760024000000"
            },
            "nonAvvmBalances": {},
            "bootStakeholders": {},
            "heavyDelegation": {},
            "startTime": 1506203091,
            "blockVersionData": {
                "slotDuration": "20000",
                "maxBlockSize": "2000000",
                "maxTxSize": "4096",
                "txFeePolicy": { "summand": "155381000000000", "multiplier": "43946000000" }
            },
            "protocolConsts": { "k": 2160, "protocolMagic": 764824073 }
        }"#;

        let genesis: ByronGenesis = serde_json::from_str(json).unwrap();
        let utxos = genesis.initial_utxos();

        assert_eq!(utxos.len(), 2, "Should have 2 AVVM UTxOs");
        let total: u64 = utxos.iter().map(|e| e.lovelace).sum();
        assert_eq!(total, 9999300000000 + 3760024000000);

        // Each address should be valid Byron CBOR
        for entry in &utxos {
            let payload = dugite_primitives::address::byron::ByronAddressPayload::from_wire_bytes(
                &entry.address,
            )
            .unwrap();
            assert_eq!(
                payload.addr_type,
                dugite_primitives::address::byron::ByronAddrType::Redeem
            );
        }
    }

    #[test]
    fn test_mixed_avvm_and_non_avvm_utxos() {
        let json = r#"{
            "avvmDistr": {
                "-0BJDi-gauylk4LptQTgjMeo7kY9lTCbZv12vwOSTZk=": "1000000"
            },
            "nonAvvmBalances": {
                "37btjrVyb4KEB2STADSsj3MYSAdj52X9FgGzKZEiHbsyZH1r39ZZRH6FvkSRMxaVBMPKknvEPYhHPV1Qgr6FSNLF1sfhaMQ4bDYB2Y3FNkPZCz": "2000000"
            },
            "bootStakeholders": {},
            "heavyDelegation": {},
            "startTime": 1506203091,
            "blockVersionData": {
                "slotDuration": "20000",
                "maxBlockSize": "2000000",
                "maxTxSize": "4096",
                "txFeePolicy": { "summand": "155381000000000", "multiplier": "43946000000" }
            },
            "protocolConsts": { "k": 2160, "protocolMagic": 764824073 }
        }"#;

        let genesis: ByronGenesis = serde_json::from_str(json).unwrap();
        let utxos = genesis.initial_utxos();

        assert_eq!(utxos.len(), 2, "Should have 1 non-AVVM + 1 AVVM");
        let total: u64 = utxos.iter().map(|e| e.lovelace).sum();
        assert_eq!(total, 3000000);
    }

    #[test]
    fn test_shelley_genesis_gen_delegs() {
        let json = r#"{
            "networkMagic": 2,
            "networkId": "Testnet",
            "systemStart": "2022-10-25T00:00:00Z",
            "activeSlotsCoeff": 0.05,
            "securityParam": 432,
            "epochLength": 86400,
            "slotLength": 1,
            "maxLovelaceSupply": 45000000000000000,
            "maxKESEvolutions": 62,
            "slotsPerKESPeriod": 129600,
            "updateQuorum": 5,
            "protocolParams": {
                "minFeeA": 44, "minFeeB": 155381, "maxBlockBodySize": 65536,
                "maxTxSize": 16384, "maxBlockHeaderSize": 1100, "keyDeposit": 2000000,
                "poolDeposit": 500000000, "eMax": 18, "nOpt": 150, "a0": 0.3,
                "rho": 0.003, "tau": 0.2, "minPoolCost": 340000000,
                "protocolVersion": { "major": 6, "minor": 0 }
            },
            "genDelegs": {
                "12b0f443d02861948a0fce9541916b014e8402984c7b83ad70a834ce": {
                    "delegate": "7c54a168c731f2f44ced620f3cca7c2bd90731cab223d5167aa994e6",
                    "vrf": "62d546a35e1be66a2b06e29558ef33f4222f1c466adbb59b52d800964d4e60ec"
                },
                "93fd5083ff20e7ab5570948831730073143bea5a5d5539852ed45889": {
                    "delegate": "3b783a80aeceb95567b3468bfcb4a9a57a904b02e6eb7ca5a85fda81",
                    "vrf": "50ca594e6c1aa30dce4e9c2d3a5c3e0a37a4e84d2d8f23f42fded2bd73a132e7"
                }
            }
        }"#;

        let genesis: ShelleyGenesis = serde_json::from_str(json).unwrap();
        assert_eq!(genesis.gen_delegs.len(), 2);

        let entries = genesis.gen_delegs_entries();
        assert_eq!(entries.len(), 2);

        // Verify byte lengths: genesis key hash = 28, delegate hash = 28, VRF hash = 32
        for (genesis_hash, delegate_hash, vrf_hash) in &entries {
            assert_eq!(
                genesis_hash.len(),
                28,
                "Genesis key hash should be 28 bytes"
            );
            assert_eq!(delegate_hash.len(), 28, "Delegate hash should be 28 bytes");
            assert_eq!(vrf_hash.len(), 32, "VRF hash should be 32 bytes");
        }
    }

    #[test]
    fn test_shelley_genesis_gen_delegs_from_preview_file() {
        // Load the actual preview Shelley genesis and verify genDelegs parse
        let path = std::path::Path::new("../../config/preview/shelley-genesis.json");
        if !path.exists() {
            return; // skip if config files not available
        }
        let (genesis, _hash) = ShelleyGenesis::load_with_hash(path).unwrap();
        let entries = genesis.gen_delegs_entries();
        assert!(
            !entries.is_empty(),
            "Preview Shelley genesis should have genDelegs"
        );
        // Preview has 7 genesis delegates
        assert_eq!(entries.len(), 7);
    }

    #[test]
    fn test_shelley_genesis_initial_funds() {
        let json = r#"{
            "networkMagic": 42,
            "networkId": "Testnet",
            "systemStart": "2024-01-01T00:00:00Z",
            "activeSlotsCoeff": 0.05,
            "securityParam": 10,
            "epochLength": 500,
            "slotLength": 1,
            "maxLovelaceSupply": 45000000000000000,
            "maxKESEvolutions": 62,
            "slotsPerKESPeriod": 129600,
            "updateQuorum": 5,
            "protocolParams": {
                "minFeeA": 44, "minFeeB": 155381, "maxBlockBodySize": 65536,
                "maxTxSize": 16384, "maxBlockHeaderSize": 1100, "keyDeposit": 2000000,
                "poolDeposit": 500000000, "eMax": 18, "nOpt": 150, "a0": 0.3,
                "rho": 0.003, "tau": 0.2, "minPoolCost": 340000000,
                "protocolVersion": { "major": 6, "minor": 0 }
            },
            "initialFunds": {
                "6000000000000000000000000000000000000000000000000000000001": 1000000000,
                "6000000000000000000000000000000000000000000000000000000002": 2000000000
            }
        }"#;

        let genesis: ShelleyGenesis = serde_json::from_str(json).unwrap();
        assert_eq!(genesis.initial_funds.len(), 2);

        let utxos = genesis.initial_utxos();
        assert_eq!(utxos.len(), 2);

        let total: u64 = utxos.iter().map(|e| e.lovelace).sum();
        assert_eq!(total, 3000000000);

        // Verify addresses are valid decoded bytes
        for utxo in &utxos {
            assert!(
                !utxo.address.is_empty(),
                "Address bytes should not be empty"
            );
        }
    }

    #[test]
    fn test_shelley_genesis_staking() {
        let json = r#"{
            "networkMagic": 42,
            "networkId": "Testnet",
            "systemStart": "2024-01-01T00:00:00Z",
            "activeSlotsCoeff": 0.05,
            "securityParam": 10,
            "epochLength": 500,
            "slotLength": 1,
            "maxLovelaceSupply": 45000000000000000,
            "maxKESEvolutions": 62,
            "slotsPerKESPeriod": 129600,
            "updateQuorum": 5,
            "protocolParams": {
                "minFeeA": 44, "minFeeB": 155381, "maxBlockBodySize": 65536,
                "maxTxSize": 16384, "maxBlockHeaderSize": 1100, "keyDeposit": 2000000,
                "poolDeposit": 500000000, "eMax": 18, "nOpt": 150, "a0": 0.3,
                "rho": 0.003, "tau": 0.2, "minPoolCost": 340000000,
                "protocolVersion": { "major": 6, "minor": 0 }
            },
            "staking": {
                "pools": {
                    "00000000000000000000000000000001": {
                        "cost": 340000000,
                        "margin": 0.02,
                        "metadata": null,
                        "owners": ["00000000000000000000000000000099"],
                        "pledge": 100000000,
                        "publicKey": "62d546a35e1be66a2b06e29558ef33f4222f1c466adbb59b52d800964d4e60ec",
                        "relays": [],
                        "rewardAccount": {
                            "credential": { "keyHash": "00000000000000000000000000000099" },
                            "network": "Testnet"
                        }
                    }
                },
                "stake": {
                    "00000000000000000000000000000099": "00000000000000000000000000000001"
                }
            }
        }"#;

        let genesis: ShelleyGenesis = serde_json::from_str(json).unwrap();
        let staking = genesis.staking.as_ref().unwrap();
        assert_eq!(staking.pools.len(), 1);
        assert_eq!(staking.stake.len(), 1);

        // Verify pool fields
        let pool = staking.pools.values().next().unwrap();
        assert_eq!(pool.cost, 340000000);
        assert_eq!(pool.pledge, 100000000);
        assert_eq!(pool.owners.len(), 1);
    }

    #[test]
    fn test_shelley_genesis_empty_optional_fields() {
        // Verify that ShelleyGenesis parses correctly when genDelegs,
        // initialFunds, and staking are absent (mainnet/preview/preprod case)
        let json = r#"{
            "networkMagic": 2,
            "networkId": "Testnet",
            "systemStart": "2022-10-25T00:00:00Z",
            "activeSlotsCoeff": 0.05,
            "securityParam": 432,
            "epochLength": 86400,
            "slotLength": 1,
            "maxLovelaceSupply": 45000000000000000,
            "maxKESEvolutions": 62,
            "slotsPerKESPeriod": 129600,
            "updateQuorum": 5,
            "protocolParams": {
                "minFeeA": 44, "minFeeB": 155381, "maxBlockBodySize": 65536,
                "maxTxSize": 16384, "maxBlockHeaderSize": 1100, "keyDeposit": 2000000,
                "poolDeposit": 500000000, "eMax": 18, "nOpt": 150, "a0": 0.3,
                "rho": 0.003, "tau": 0.2, "minPoolCost": 340000000,
                "protocolVersion": { "major": 6, "minor": 0 }
            }
        }"#;

        let genesis: ShelleyGenesis = serde_json::from_str(json).unwrap();
        assert!(genesis.gen_delegs.is_empty());
        assert!(genesis.initial_funds.is_empty());
        assert!(genesis.staking.is_none());
        assert!(genesis.gen_delegs_entries().is_empty());
        assert!(genesis.initial_utxos().is_empty());
    }

    // ── Issue #545 E8/E9 tests: ShelleyGenesis::validate() ────────────────

    /// Helper: build a minimal valid ShelleyGenesis struct for testing.
    fn make_valid_shelley_genesis() -> ShelleyGenesis {
        ShelleyGenesis {
            network_magic: 2,
            network_id: "Testnet".to_string(),
            system_start: "2022-10-25T00:00:00Z".to_string(),
            active_slots_coeff: 0.05,
            security_param: 2160,
            epoch_length: 432000,
            slot_length: 1,
            max_lovelace_supply: 45_000_000_000_000_000,
            max_k_e_s_evolutions: 62,
            slots_per_k_e_s_period: 129600,
            update_quorum: 5,
            protocol_params: ShelleyGenesisProtocolParams {
                min_fee_a: 44,
                min_fee_b: 155381,
                max_block_body_size: 65536,
                max_tx_size: 16384,
                max_block_header_size: 1100,
                key_deposit: 2000000,
                pool_deposit: 500000000,
                e_max: 18,
                n_opt: 150,
                a0: 0.3,
                rho: 0.003,
                tau: 0.2,
                decentralisation_param: 0.5,
                min_pool_cost: 340000000,
                min_u_tx_o_value: 1000000,
                protocol_version: ProtocolVersion { major: 2, minor: 0 },
            },
            gen_delegs: Default::default(),
            initial_funds: Default::default(),
            staking: None,
        }
    }

    #[test]
    fn issue_545_e8_valid_genesis_passes_validate() {
        let genesis = make_valid_shelley_genesis();
        assert!(
            genesis.validate().is_ok(),
            "Valid genesis should pass validate()"
        );
    }

    #[test]
    fn issue_545_e8_slots_per_kes_period_zero_rejected() {
        let mut genesis = make_valid_shelley_genesis();
        genesis.slots_per_k_e_s_period = 0;
        let result = genesis.validate();
        assert!(
            result.is_err(),
            "slotsPerKESPeriod=0 must be rejected, got: {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("slotsPerKESPeriod"),
            "Error must mention slotsPerKESPeriod, got: {msg}"
        );
    }

    #[test]
    fn issue_545_e8_max_kes_evolutions_zero_rejected() {
        let mut genesis = make_valid_shelley_genesis();
        genesis.max_k_e_s_evolutions = 0;
        let result = genesis.validate();
        assert!(
            result.is_err(),
            "maxKESEvolutions=0 must be rejected, got: {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("maxKESEvolutions"),
            "Error must mention maxKESEvolutions, got: {msg}"
        );
    }

    #[test]
    fn issue_545_e9_epoch_length_zero_rejected() {
        let mut genesis = make_valid_shelley_genesis();
        genesis.epoch_length = 0;
        let result = genesis.validate();
        assert!(
            result.is_err(),
            "epochLength=0 must be rejected, got: {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("epochLength"),
            "Error must mention epochLength, got: {msg}"
        );
    }

    #[test]
    fn issue_545_e9_security_param_zero_rejected() {
        let mut genesis = make_valid_shelley_genesis();
        genesis.security_param = 0;
        let result = genesis.validate();
        assert!(
            result.is_err(),
            "securityParam=0 must be rejected, got: {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("securityParam"),
            "Error must mention securityParam, got: {msg}"
        );
    }

    // ── SlotConfig anchoring tests ────────────────────────────────────────────

    /// Build a minimal ShelleyGenesis for slot_config tests.
    fn shelley_genesis_for_slot_config(system_start: &str, slot_length_s: u64) -> ShelleyGenesis {
        ShelleyGenesis {
            network_magic: 764824073,
            network_id: "Mainnet".to_string(),
            system_start: system_start.to_string(),
            active_slots_coeff: 0.05,
            security_param: 2160,
            epoch_length: 432000,
            slot_length: slot_length_s,
            max_lovelace_supply: 45_000_000_000_000_000,
            max_k_e_s_evolutions: 62,
            slots_per_k_e_s_period: 129600,
            update_quorum: 5,
            protocol_params: ShelleyGenesisProtocolParams {
                min_fee_a: 44,
                min_fee_b: 155381,
                max_block_body_size: 65536,
                max_tx_size: 16384,
                max_block_header_size: 1100,
                key_deposit: 2000000,
                pool_deposit: 500000000,
                e_max: 18,
                n_opt: 150,
                a0: 0.3,
                rho: 0.003,
                tau: 0.2,
                decentralisation_param: 0.0,
                min_pool_cost: 340000000,
                min_u_tx_o_value: 1000000,
                protocol_version: ProtocolVersion { major: 8, minor: 0 },
            },
            gen_delegs: Default::default(),
            initial_funds: Default::default(),
            staking: None,
        }
    }

    /// Mainnet: systemStart = "2017-09-23T21:44:51Z" (Byron network start).
    /// Shelley hard fork occurred at epoch 208, slot 4_492_800.
    /// Byron epoch size = 21_600 slots (10 * k=2160).
    /// Byron slot duration = 20_000 ms.
    ///
    /// Expected Shelley anchor:
    ///   zero_slot = 208 * 21_600 = 4_492_800
    ///   zero_time = 1_506_203_091_000 + 4_492_800 * 20_000 = 1_596_059_091_000
    ///
    /// This matches the known-correct SlotConfig::default() mainnet constants and
    /// the proof-of-correctness test in crates/dugite-uplc/tests/zz_slotcfg_probe.rs.
    #[test]
    fn slot_config_mainnet_anchors_at_shelley_start() {
        let sg = shelley_genesis_for_slot_config("2017-09-23T21:44:51Z", 1);
        // Mainnet: k=2160, epoch_size=10*k=21600, Byron slot=20s, transition epoch 208
        let shelley_transition_epoch = 208u64;
        let byron_epoch_size = 21_600u64; // 10 * k
        let byron_slot_duration_ms = 20_000u64; // 20 seconds

        let sc = sg.slot_config(
            shelley_transition_epoch,
            byron_epoch_size,
            byron_slot_duration_ms,
        );

        assert_eq!(
            sc.zero_slot, 4_492_800,
            "zero_slot must be the first Shelley slot (208 * 21600)"
        );
        assert_eq!(
            sc.zero_time, 1_596_059_091_000,
            "zero_time must be the Shelley hard-fork POSIX time in ms \
             (2020-07-29 21:44:51 UTC), not the Byron network start"
        );
        assert_eq!(sc.slot_length, 1_000, "Shelley slot length must be 1000 ms");
    }

    /// Preview testnet: shelley_transition_epoch=0, so the Shelley era starts
    /// at slot 0 and zero_time == system_start.  Byron params are irrelevant.
    #[test]
    fn slot_config_preview_zero_transition() {
        // Preview system_start = "2022-10-25T00:00:00Z" = 1666656000_000 ms
        let sg = shelley_genesis_for_slot_config("2022-10-25T00:00:00Z", 1);
        let sc = sg.slot_config(0, 0, 20_000);

        assert_eq!(
            sc.zero_slot, 0,
            "Preview: zero_slot must be 0 (instant Shelley transition)"
        );
        assert_eq!(
            sc.zero_time, 1_666_656_000_000,
            "Preview: zero_time must equal system_start ms"
        );
        assert_eq!(sc.slot_length, 1_000);
    }

    /// Preprod testnet: shelley_transition_epoch=4, byron_epoch_size=21600,
    /// byron_slot_duration_ms=20000. system_start = "2022-06-01T00:00:00Z".
    ///
    ///   zero_slot = 4 * 21600 = 86400
    ///   zero_time = 1654041600000 + 86400 * 20000 = 1654041600000 + 1728000000 = 1655769600000
    ///
    /// Cross-check: Shelley hard fork on preprod = 2022-06-21 00:00:00 UTC = 1655769600 s.
    #[test]
    fn slot_config_preprod_anchors_at_shelley_start() {
        let sg = shelley_genesis_for_slot_config("2022-06-01T00:00:00Z", 1);
        let sc = sg.slot_config(4, 21_600, 20_000);

        assert_eq!(sc.zero_slot, 86_400, "Preprod: zero_slot = 4 * 21600");
        assert_eq!(
            sc.zero_time, 1_655_769_600_000,
            "Preprod: zero_time must be 2022-06-21 00:00:00 UTC in ms"
        );
        assert_eq!(sc.slot_length, 1_000);
    }

    // ── #1046: cost models come from genesis fields / extraConfig / PPU ─────
    //
    // cardano-ledger has NO default PlutusV2. `alonzoInjectCostModels`
    // (eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Transition.hs) applies
    // `agExtraConfig.aecCostModels` and nothing else:
    //
    //   alonzoInjectCostModels cfg =
    //     case agExtraConfig $ cfg ^. tcTranslationContextL of
    //       SNothing -> id
    //       SJust aec -> overrideCostModels (aecCostModels aec)
    //
    //   overrideCostModels (Just cms) =
    //     nesEsL . curPParamsEpochStateL . ppCostModelsL
    //       %~ flip updateCostModels (CostModelsUpdate cms)
    //
    // Two properties under test: cur-only, and per-language update.

    fn alonzo_genesis_json(extra_config: Option<&str>) -> String {
        let extra = match extra_config {
            Some(e) => format!(", \"extraConfig\": {e}"),
            None => String::new(),
        };
        format!(
            r#"{{
              "lovelacePerUTxOWord": 34482,
              "executionPrices": {{ "prSteps": {{"numerator":721,"denominator":10000000}},
                                    "prMem": {{"numerator":577,"denominator":10000}} }},
              "maxTxExUnits": {{ "exUnitsMem": 10000000, "exUnitsSteps": 10000000000 }},
              "maxBlockExUnits": {{ "exUnitsMem": 50000000, "exUnitsSteps": 40000000000 }},
              "maxValueSize": 5000,
              "collateralPercentage": 150,
              "maxCollateralInputs": 3,
              "costModels": {{ "PlutusV1": [1, 2, 3] }}
              {extra}
            }}"#
        )
    }

    /// A real-network shape (no `extraConfig`): PlutusV2 must stay ABSENT.
    /// This is the #1046 regression — dugite used to inject a hardcoded
    /// `defaultV2CostModel` here, giving it a cost model cardano-node does not
    /// have. Reachable consequence: a V2 script would EXECUTE on dugite and fail
    /// on cardano-node with `CollectErrors [NoCostModel PlutusV2]`.
    #[test]
    fn alonzo_genesis_without_extra_config_yields_no_plutus_v2() {
        let g: AlonzoGenesis = serde_json::from_str(&alonzo_genesis_json(None)).unwrap();
        assert!(
            g.extra_config.is_none(),
            "mainnet/preview/preprod alonzo-genesis has no extraConfig"
        );

        let mut params = ProtocolParameters::mainnet_defaults();
        params.cost_models.plutus_v1 = None;
        params.cost_models.plutus_v2 = None;
        params.cost_models.plutus_v3 = None;

        g.apply_to_protocol_params(&mut params);
        g.apply_extra_config_cost_models(&mut params);

        assert_eq!(params.cost_models.plutus_v1, Some(vec![1, 2, 3]));
        assert_eq!(
            params.cost_models.plutus_v2, None,
            "no extraConfig ⇒ NO PlutusV2 cost model (#1046); cardano-ledger has \
             no default and dugite must not invent one"
        );
    }

    /// A `create-testnet-data` devnet shape: `extraConfig` supplies V1+V2, so
    /// PlutusV2 IS present — and V1 is OVERRIDDEN, since `updateCostModels` is a
    /// per-language update. This is what keeps devnet parity intact without the
    /// hardcoded default (#994's stated fear).
    #[test]
    fn alonzo_genesis_extra_config_overrides_per_language() {
        let g: AlonzoGenesis = serde_json::from_str(&alonzo_genesis_json(Some(
            r#"{ "costModels": { "PlutusV1": [10, 20, 30], "PlutusV2": [40, 50] } }"#,
        )))
        .unwrap();

        let mut params = ProtocolParameters::mainnet_defaults();
        params.cost_models.plutus_v1 = None;
        params.cost_models.plutus_v2 = None;
        params.cost_models.plutus_v3 = None;

        g.apply_to_protocol_params(&mut params);
        // Stand in for the Conway genesis contributing its V3 field before the
        // override runs — the override must not disturb it.
        params.cost_models.plutus_v3 = Some(vec![7, 8, 9]);
        g.apply_extra_config_cost_models(&mut params);

        assert_eq!(
            params.cost_models.plutus_v1,
            Some(vec![10, 20, 30]),
            "extraConfig OVERRIDES the era-translated V1"
        );
        assert_eq!(
            params.cost_models.plutus_v2,
            Some(vec![40, 50]),
            "extraConfig supplies V2 — this is where the devnet's V2 comes from"
        );
        assert_eq!(
            params.cost_models.plutus_v3,
            Some(vec![7, 8, 9]),
            "a language NOT named in extraConfig is retained (per-language update, \
             not a wholesale replacement)"
        );
    }

    /// The devnet's real generated genesis carries V2 in `extraConfig`, NOT in
    /// the top-level `costModels`. Pinning the shape, because reading only the
    /// top-level field is what made #994 conclude no genesis file supplied V2.
    #[test]
    fn devnet_style_extra_config_is_where_v2_actually_lives() {
        let g: AlonzoGenesis = serde_json::from_str(&alonzo_genesis_json(Some(
            r#"{ "costModels": { "PlutusV2": [99] } }"#,
        )))
        .unwrap();

        assert!(
            !g.cost_models.contains_key("PlutusV2"),
            "top-level costModels has no V2 — that field is V1-only on every \
             real alonzo-genesis"
        );
        assert_eq!(
            g.extra_config
                .as_ref()
                .and_then(|e| e.cost_models.get("PlutusV2"))
                .and_then(parse_cost_model),
            Some(vec![99]),
            "V2 lives in extraConfig.costModels"
        );
    }
}
