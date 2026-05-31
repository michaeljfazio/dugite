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
    /// Bootstrap stakeholders: stakeholder ID → weight (deserialized for completeness)
    #[serde(default, rename = "bootStakeholders")]
    _boot_stakeholders: HashMap<String, serde_json::Value>,
    /// Heavy delegation certificates (deserialized for completeness)
    #[serde(default, rename = "heavyDelegation")]
    _heavy_delegation: HashMap<String, serde_json::Value>,
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
    #[serde(default, rename = "maxBlockSize")]
    pub _max_block_size: String,
    #[serde(default, rename = "maxTxSize")]
    _max_tx_size: String,
    #[serde(default, rename = "txFeePolicy")]
    _tx_fee_policy: ByronTxFeePolicy,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
// Fields populated by serde deserialization from cardano-node genesis JSON
pub struct ByronTxFeePolicy {
    /// Fee = summand + multiplier * tx_size (both values are x1e12)
    #[serde(default, rename = "summand")]
    _summand: String,
    #[serde(default, rename = "multiplier")]
    _multiplier: String,
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

    /// Extract the initial UTxO set from both nonAvvmBalances and avvmDistr.
    ///
    /// Returns decoded address bytes and lovelace amounts for all non-zero balances.
    /// For nonAvvmBalances, addresses are base58-decoded directly.
    /// For avvmDistr, base64url Ed25519 public keys are converted to Byron redeem addresses.
    pub fn initial_utxos(&self) -> Vec<GenesisUtxoEntry> {
        let mut entries = Vec::new();
        let protocol_magic = self.protocol_consts.protocol_magic;

        // Process nonAvvmBalances (base58 Byron addresses)
        for (addr_str, lovelace_str) in &self.non_avvm_balances {
            let lovelace: u64 = match lovelace_str.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            if lovelace == 0 {
                continue;
            }

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
            if lovelace == 0 {
                continue;
            }

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
            if *lovelace == 0 {
                continue;
            }
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
            params.min_fee_ref_script_cost_per_byte = cost;
        }

        // PlutusV3 cost model from Conway genesis
        if let Some(v3) = &self.plutus_v3_cost_model {
            debug!(
                count = v3.len(),
                "Loaded PlutusV3 cost model from Conway genesis"
            );
            params.cost_models.plutus_v3 = Some(v3.clone());
        }

        // PlutusV2 cost model: if not already set from Alonzo genesis or
        // on-chain protocol parameter updates, fall back to the initial V2
        // values cardano-node uses when no V2 is present in genesis files.
        //
        // These are the pre-Babbage `defaultV2CostModel` values from
        // `cardano-api/src/Cardano/Api/Genesis/Internal.hs` — the V2 cost
        // model as introduced at the Alonzo→Babbage HFC. They share the
        // first ~133 entries with V1 (V2 inherited V1's pricing for
        // pre-existing builtins and added new ones on top), then diverge
        // with `serialiseData`, `keccak256`, `blake2b224`, etc.
        //
        // On mainnet/preview/preprod, a Babbage-era ParameterChange action
        // updated V2 to the values starting `[100788, 420, ...]` —
        // dugite's on-chain `apply_protocol_param_update` applies that
        // automatically during sync, so we converge with public networks
        // after that epoch.
        //
        // On a Conway-direct devnet (cardano-testnet), no such
        // ParameterChange has ever run, so these initial values remain
        // authoritative. Using the post-Babbage values here would break
        // script integrity hash validation against cardano-node, which
        // computes `LangDepView` from these initial values.
        if params.cost_models.plutus_v2.is_none() {
            debug!("PlutusV2 cost model not set — loading pre-Babbage defaultV2CostModel");
            params.cost_models.plutus_v2 = Some(vec![
                205665, 812, 1, 1, 1000, 571, 0, 1, 1000, 24177, 4, 1, 1000, 32, 117366, 10475, 4,
                23000, 100, 23000, 100, 23000, 100, 23000, 100, 23000, 100, 23000, 100, 100, 100,
                23000, 100, 19537, 32, 175354, 32, 46417, 4, 221973, 511, 0, 1, 89141, 32, 497525,
                14068, 4, 2, 196500, 453240, 220, 0, 1, 1, 1000, 28662, 4, 2, 245000, 216773, 62,
                1, 1060367, 12586, 1, 208512, 421, 1, 187000, 1000, 52998, 1, 80436, 32, 43249, 32,
                1000, 32, 80556, 1, 57667, 4, 1000, 10, 197145, 156, 1, 197145, 156, 1, 204924,
                473, 1, 208896, 511, 1, 52467, 32, 64832, 32, 65493, 32, 22558, 32, 16563, 32,
                76511, 32, 196500, 453240, 220, 0, 1, 1, 69522, 11687, 0, 1, 60091, 32, 196500,
                453240, 220, 0, 1, 1, 196500, 453240, 220, 0, 1, 1, 1159724, 392670, 0, 2, 806990,
                30482, 4, 1927926, 82523, 4, 265318, 0, 4, 0, 85931, 32, 205665, 812, 1, 1, 41182,
                32, 212342, 32, 31220, 32, 32696, 32, 43357, 32, 32247, 32, 38314, 32, 35892428,
                10, 9462713, 1021, 10, 38887044, 32947, 10,
            ]);
        }

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

/// Convert a float to a rational approximation
fn float_to_rational(f: f64) -> Rational {
    if f == 0.0 {
        return Rational {
            numerator: 0,
            denominator: 1,
        };
    }
    // Use 1_000_000 as denominator for good precision
    let den = 1_000_000u64;
    let num = (f * den as f64).round() as u64;
    // Simplify with GCD
    let g = gcd(num, den);
    Rational {
        numerator: num / g,
        denominator: den / g,
    }
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
        assert_eq!(genesis.block_version_data._max_block_size, "2000000");

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
}
