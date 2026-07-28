use crate::transaction::{CostModels, ExUnitPrices, ExUnits, Rational};
use crate::value::{Lovelace, Value};
use serde::{Deserialize, Serialize};

/// Lovelace per byte of serialized transaction body, used as the linear
/// coefficient in the minimum-fee formula (`fee = a × tx_size + b`).
///
/// Cardano upstream introduced the dedicated `CoinPerByte` type in
/// Babbage (`cardano-ledger/libs/cardano-ledger-core/src/Cardano/Ledger/Coin.hs`)
/// and re-affirmed the rename of the PParam from `minFeeA` to `txFeePerByte`
/// in Dijkstra (`eras/dijkstra/impl/.../PParams.hs`, field `dppTxFeePerByte`).
///
/// On the wire the field is still encoded as a CBOR `uint` at PParams key 0
/// — the type carries semantic intent only. JSON serialization is
/// `derive(newtype)` over `Word64`, so `CoinPerByte(44)` renders as the bare
/// integer `44` — identical to the legacy `Coin` shape. See Phase 4.3 of
/// issue #462 for the full audit.
///
/// Construct via [`CoinPerByte::new`] or `.into()` and read with `.lovelace()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CoinPerByte(pub u64);

impl CoinPerByte {
    /// Construct a new `CoinPerByte` from a raw lovelace-per-byte value.
    pub const fn new(lovelace_per_byte: u64) -> Self {
        CoinPerByte(lovelace_per_byte)
    }

    /// Underlying `u64` lovelace-per-byte coefficient.
    pub const fn lovelace(self) -> u64 {
        self.0
    }
}

impl From<u64> for CoinPerByte {
    fn from(v: u64) -> Self {
        CoinPerByte(v)
    }
}

impl From<CoinPerByte> for u64 {
    fn from(v: CoinPerByte) -> Self {
        v.0
    }
}

impl std::fmt::Display for CoinPerByte {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Complete protocol parameters (Conway era and forward; identical wire shape
/// in Dijkstra apart from the four new ref-script tier keys 34-37 which are
/// tracked under #462 Phase 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolParameters {
    /// Linear coefficient of the minimum-fee formula:
    /// `fee = min_fee_a × tx_size_bytes + min_fee_b`.
    ///
    /// Encoded at PParams key 0 as a CBOR `uint` and serialised in JSON as a
    /// plain integer under the modern name `"txFeePerByte"` (with the legacy
    /// alias `"minFeeA"` accepted on deserialization for older fixtures).
    ///
    /// In Dijkstra (#462 Phase 4.3) the Haskell ledger renamed
    /// `cppMinFeeA`/`bppMinFeeA`/`sppMinFeeA` to `*TxFeePerByte` and bound
    /// the typed-newtype `CoinPerByte` to it. The byte representation is
    /// unchanged; this struct retains the historical Rust field name for
    /// source compatibility. Prefer the [`Self::tx_fee_per_byte`] accessor
    /// when expressing intent — that returns a [`CoinPerByte`] wrapper.
    #[serde(alias = "txFeePerByte", alias = "minFeeA")]
    pub min_fee_a: u64,
    pub min_fee_b: u64,

    // Size limits
    pub max_block_body_size: u64,
    pub max_tx_size: u64,
    pub max_block_header_size: u64,

    // Staking
    pub key_deposit: Lovelace,
    pub pool_deposit: Lovelace,
    pub e_max: u64,    // maximum epoch for pool retirement
    pub n_opt: u64,    // desired number of stake pools
    pub a0: Rational,  // pool pledge influence
    pub rho: Rational, // monetary expansion
    pub tau: Rational, // treasury growth rate

    // Minimum values
    pub min_pool_cost: Lovelace,
    pub ada_per_utxo_byte: Lovelace,
    /// Shelley/Allegra/Mary flat minimum UTxO value (`minUTxOValue` PParam,
    /// wire key 15). Shelley and Allegra's `getMinCoinTxOut` return this
    /// value UNCONDITIONALLY — the TxOut is never inspected in those eras.
    /// Mary's `scaledMinDeposit` returns this flat value for ada-only
    /// outputs and derives `coinsPerUTxOWord = mv \`quot\` 27` from it for
    /// multi-asset outputs (see [`Self::min_coin_for_output`]).
    ///
    /// Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/TxOut.hs:94`,
    /// `eras/allegra/impl/src/Cardano/Ledger/Allegra/TxOut.hs:31`,
    /// `eras/mary/impl/src/Cardano/Ledger/Mary/TxOut.hs:52-76`. Mainnet value:
    /// 1_000_000 lovelace, set at genesis and never updated on-chain for the
    /// whole Shelley→Mary span (issue #919).
    #[serde(default = "default_min_utxo_value")]
    pub min_utxo_value: Lovelace,
    /// Alonzo-only `coinsPerUTxOWord` PParam (wire key 17 in Alonzo-era
    /// PPUs) — an INDEPENDENT parameter, NOT derived from `min_utxo_value`.
    /// Used in `utxoEntrySize txOut * coinsPerUTxOWord`
    /// (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxOut.hs:370-372,504-517`).
    ///
    /// Superseded by `ada_per_utxo_byte` (`coinsPerUTxOByte`) at the
    /// Alonzo→Babbage hard fork via the one-way
    /// `coinsPerUTxOWordToCoinsPerUTxOByte` (÷8) translation — see
    /// [`Self::apply_key17_update`].
    #[serde(default = "default_coins_per_utxo_word")]
    pub coins_per_utxo_word: Lovelace,

    // Plutus
    pub cost_models: CostModels,
    pub execution_costs: ExUnitPrices,
    pub max_tx_ex_units: ExUnits,
    pub max_block_ex_units: ExUnits,
    pub max_val_size: u64,
    pub collateral_percentage: u64,
    pub max_collateral_inputs: u64,
    /// Conway `minFeeRefScriptCostPerByte` — a `NonNegativeInterval` (rational),
    /// not an integer.  Mainnet is `15/1`, but a governance ParameterChange may
    /// set a fractional value, so it is stored as a full `Rational` to stay
    /// byte-exact with cardano-ledger (`ppMinFeeRefScriptCostPerByte`).
    pub min_fee_ref_script_cost_per_byte: Rational,

    // Conway governance
    pub drep_deposit: Lovelace,
    pub drep_activity: u64,
    pub gov_action_deposit: Lovelace,
    pub gov_action_lifetime: u64,
    pub committee_min_size: u64,
    pub committee_max_term_length: u64,

    // Governance voting thresholds (as rationals)
    /// DRep voting threshold for PP changes — network group
    pub dvt_pp_network_group: Rational,
    /// DRep voting threshold for PP changes — economic group
    pub dvt_pp_economic_group: Rational,
    /// DRep voting threshold for PP changes — technical group
    pub dvt_pp_technical_group: Rational,
    /// DRep voting threshold for PP changes — governance group
    pub dvt_pp_gov_group: Rational,
    /// DRep voting threshold for HardForkInitiation
    pub dvt_hard_fork: Rational,
    /// DRep voting threshold for NoConfidence
    pub dvt_no_confidence: Rational,
    /// DRep voting threshold for UpdateCommittee (normal state)
    pub dvt_committee_normal: Rational,
    /// DRep voting threshold for UpdateCommittee (no confidence state)
    pub dvt_committee_no_confidence: Rational,
    /// DRep voting threshold for NewConstitution
    pub dvt_constitution: Rational,
    /// DRep voting threshold for TreasuryWithdrawals
    pub dvt_treasury_withdrawal: Rational,
    /// SPO voting threshold for MotionNoConfidence
    pub pvt_motion_no_confidence: Rational,
    /// SPO voting threshold for UpdateCommittee (normal state)
    pub pvt_committee_normal: Rational,
    /// SPO voting threshold for UpdateCommittee (no confidence state)
    pub pvt_committee_no_confidence: Rational,
    /// SPO voting threshold for HardForkInitiation
    pub pvt_hard_fork: Rational,
    /// SPO voting threshold for security-relevant protocol parameter changes
    pub pvt_pp_security_group: Rational,

    // Protocol version
    pub protocol_version_major: u64,
    pub protocol_version_minor: u64,

    // Consensus
    /// Active slot coefficient (probability of a slot having a block)
    #[serde(default = "default_active_slot_coeff")]
    pub active_slots_coeff: f64,

    /// Decentralisation parameter (d): fraction of slots reserved for BFT
    /// overlay schedule. Range [0, 1] where 0 = fully decentralised (Praos),
    /// 1 = fully federated (BFT). Deprecated since Babbage (always 0).
    ///
    /// Stored as rational numerator/denominator. When d >= 0.8, the Haskell
    /// reward calculation forces eta = 1 (no performance adjustment).
    #[serde(default)]
    pub d: Rational,

    // Dijkstra-era parameters (PParams map keys 34-37).
    //
    // These re-parameterise the ref-script tiering that was hardcoded in Conway
    // (1 MiB block limit / 200 KiB tx limit / 25_600-byte stride / 1.2×
    // multiplier).  The fields are `None` for pre-Dijkstra protocol versions
    // and are populated when a ParameterChange governance action carrying keys
    // 34-37 is ratified, or when the Dijkstra genesis file seeds them.
    //
    // Haskell source: `cardano-ledger:eras/dijkstra/impl/src/Cardano/Ledger/
    // Dijkstra/PParams.hs`, `PParamUpdate` tags 34-37.
    /// PParam key 34 — maximum total bytes of reference scripts across all
    /// transactions in a block (`maxRefScriptSizePerBlock`, Word32).
    ///
    /// Default (per `dijkstra-genesis.json`): 1 MiB = 1_048_576.
    #[serde(default)]
    pub max_ref_script_size_per_block: Option<u32>,

    /// PParam key 35 — maximum total bytes of reference scripts a single
    /// transaction may use (`maxRefScriptSizePerTx`, Word32).
    ///
    /// Default: 200 KiB = 204_800.
    #[serde(default)]
    pub max_ref_script_size_per_tx: Option<u32>,

    /// PParam key 36 — stride (in bytes) of each pricing tier for reference
    /// scripts (`refScriptCostStride`, NonZero Word32).
    ///
    /// Default: 25_600.
    #[serde(default)]
    pub ref_script_cost_stride: Option<u32>,

    /// PParam key 37 — per-tier fee multiplier for reference scripts
    /// (`refScriptCostMultiplier`, PositiveInterval encoded as rational).
    ///
    /// Stored as `(numerator, denominator)`; default 6/5 = 1.2.
    #[serde(default)]
    pub ref_script_cost_multiplier: Option<Rational>,
}

fn default_active_slot_coeff() -> f64 {
    0.05
}

/// Mainnet Shelley genesis `minUTxOValue` — never updated on-chain across the
/// whole Shelley→Mary span (issue #919).
fn default_min_utxo_value() -> Lovelace {
    Lovelace(1_000_000)
}

/// Mainnet Alonzo genesis `lovelacePerUTxOWord` (34_482), stored losslessly
/// (the `/8` translation to `ada_per_utxo_byte` is a one-way, lossy integer
/// division — see [`ProtocolParameters::apply_key17_update`]).
fn default_coins_per_utxo_word() -> Lovelace {
    Lovelace(34_482)
}

impl ProtocolParameters {
    /// Calculate the minimum fee for a transaction
    pub fn min_fee(&self, tx_size: u64) -> Lovelace {
        Lovelace(self.min_fee_a * tx_size + self.min_fee_b)
    }

    /// Dijkstra-named accessor returning the linear fee coefficient as a
    /// typed [`CoinPerByte`] (see #462 Phase 4.3).
    ///
    /// The byte representation on the wire and in JSON is unchanged from
    /// Conway — this accessor purely surfaces the semantic intent.
    pub fn tx_fee_per_byte(&self) -> CoinPerByte {
        CoinPerByte(self.min_fee_a)
    }

    /// Set the linear fee coefficient from a typed [`CoinPerByte`].
    pub fn set_tx_fee_per_byte(&mut self, c: CoinPerByte) {
        self.min_fee_a = c.lovelace();
    }

    /// Active slot coefficient (1/20 = 0.05 on mainnet)
    pub fn active_slot_coeff(&self) -> f64 {
        self.active_slots_coeff
    }

    /// Active slot coefficient as a rational (numerator, denominator).
    ///
    /// Converts the f64 `active_slots_coeff` to an exact rational by searching
    /// for the smallest denominator that reconstructs the float exactly.
    /// For mainnet (0.05) this returns (1, 20).
    pub fn active_slot_coeff_rational(&self) -> (u64, u64) {
        f64_to_rational(self.active_slots_coeff)
    }

    /// Babbage/Conway minimum UTxO value for a simple ADA-only output (no
    /// multi-assets, no datum), estimating its serialized size at 29 bytes.
    ///
    /// NOTE: this is a Babbage/Conway-only (PV >= 7) estimate — pre-Babbage
    /// eras never use the serialized-size formula at all. Callers that need
    /// the correct minimum for the era IN FORCE must use
    /// [`Self::min_coin_for_output`] instead (issue #919).
    pub fn babbage_min_utxo_ada_only(&self) -> Lovelace {
        // Babbage formula: coins_per_utxo_byte * (160 + output_size)
        // Simple ADA-only output is ~29 bytes serialized
        Lovelace(self.ada_per_utxo_byte.0 * (160 + 29))
    }

    /// Minimum UTxO for an output with a specific serialized size.
    /// Uses the Babbage/Conway formula: coins_per_utxo_byte * (160 + output_size)
    /// where 160 is the constant overhead for the UTxO entry itself.
    ///
    /// NOTE: Babbage/Conway (PV >= 7) only. See [`Self::min_coin_for_output`]
    /// for the full per-era dispatch (issue #919).
    pub fn min_utxo_for_output_size(&self, output_size_bytes: u64) -> Lovelace {
        Lovelace(self.ada_per_utxo_byte.0 * (160 + output_size_bytes))
    }

    /// Per-era minimum-coin dispatch for Phase-1 Rule 5 (`OutputTooSmall`).
    ///
    /// Haskell's `getMinCoinTxOut` is defined PER-ERA — a Babbage-shaped
    /// calculation is structurally impossible to run against a Shelley
    /// `TxOut` in the reference implementation, since each era's UTXO rule
    /// only ever sees its OWN era's PParams type. Applying one formula
    /// unconditionally across all eras (the pre-#919 bug) produces false
    /// `OutputTooSmall` rejections on real Shelley/Allegra/Mary mainnet
    /// transactions whenever `ada_per_utxo_byte` has already been seeded
    /// from the Alonzo genesis file (which dugite applies unconditionally
    /// at node startup, regardless of the chain's current era).
    ///
    /// `has_datum_hash` — true iff the output carries an
    /// `OutputDatum::DatumHash` (Alonzo has no inline-datum concept; that
    /// arrives in Babbage). `output_size_bytes` — the output's true
    /// serialized CBOR length, only consulted by the Babbage+ branch.
    ///
    /// Dispatch (PV = `protocol_version_major`):
    /// - PV <= 3 (Shelley, Allegra): flat `min_utxo_value`, TxOut ignored.
    ///   (`eras/shelley/impl/.../TxOut.hs:94`, `eras/allegra/impl/.../TxOut.hs:31`)
    /// - PV == 4 (Mary): `scaledMinDeposit` — ada-only short-circuits to the
    ///   flat value; multi-asset scales by [`Value::mary_value_size`].
    ///   (`eras/mary/impl/.../TxOut.hs:52-76`)
    /// - PV 5-6 (Alonzo): `utxoEntrySize * coinsPerUTxOWord`, no ada-only
    ///   short-circuit. (`eras/alonzo/impl/.../TxOut.hs:370-372,504-517`)
    /// - PV >= 7 (Babbage/Conway+): unchanged serialized-size formula.
    ///
    /// The Haskell comparison predicates (`Shelley/Rules/Utxo.hs:521-538` for
    /// Shelley/Allegra/Mary, `Alonzo/Rules/Utxo.hs:387-405` for Alonzo+) both
    /// reduce to a coin-only `<` comparison against the value returned here:
    /// Shelley/Allegra/Mary compare `coin(txOut) < getMinCoinTxOut` directly;
    /// Alonzo+'s `Val.pointwise (>=)` against an injected coin-only value
    /// additionally asserts every non-ada quantity is `>= 0`, which is a
    /// structural invariant of dugite's `u64`-quantity `Value` and therefore
    /// never independently fails.
    pub fn min_coin_for_output(
        &self,
        value: &Value,
        has_datum_hash: bool,
        output_size_bytes: u64,
    ) -> Lovelace {
        match self.protocol_version_major {
            0..=3 => {
                // Shelley (2) / Allegra (3): flat param, TxOut unused.
                self.min_utxo_value
            }
            4 => {
                // Mary `scaledMinDeposit`.
                if value.is_pure_ada() {
                    self.min_utxo_value
                } else {
                    let mv = self.min_utxo_value.0;
                    let coins_per_word = mv / 27; // `mv \`quot\` 27`
                    let size = value.mary_value_size();
                    Lovelace(mv.max(coins_per_word * (27 + size)))
                }
            }
            5 | 6 => {
                // Alonzo `utxoEntrySize * coinsPerUTxOWord`.
                let data_hash_size = if has_datum_hash { 10 } else { 0 };
                let entry_size = 27 + value.mary_value_size() + data_hash_size;
                Lovelace(entry_size * self.coins_per_utxo_word.0)
            }
            _ => {
                // Babbage/Conway+ (PV >= 7): unchanged serialized-size formula.
                self.min_utxo_for_output_size(output_size_bytes)
            }
        }
    }

    /// Apply an incoming PParams wire-key-17 update, disambiguating by the
    /// protocol version currently IN FORCE on `self` — i.e. BEFORE any
    /// `protocol_version_major` bump carried by the SAME update is applied
    /// (callers must invoke this before overwriting `protocol_version_major`).
    ///
    /// Wire key 17 is `coinsPerUTxOWord` in Alonzo-era PParamUpdates (PV <= 6)
    /// and `coinsPerUTxOByte` in Babbage+ PParamUpdates (PV >= 7) — an
    /// identical CBOR shape (a bare Lovelace `uint`) with two different
    /// semantics. Haskell's own `coinsPerUTxOWordToCoinsPerUTxOByte`
    /// translation (`eras/babbage/impl/src/Cardano/Ledger/Babbage/PParams.hs`)
    /// runs exactly once, at the Alonzo->Babbage hard-fork era-translation
    /// boundary. dugite instead re-derives the byte value eagerly on every
    /// Alonzo-era key-17 update, so it is always current by the time the
    /// hard fork lands — idempotent, since only the LAST word value before
    /// the fork matters, and eager derivation keeps it continuously in sync.
    pub fn apply_key17_update(&mut self, v: Lovelace) {
        if self.protocol_version_major <= 6 {
            self.coins_per_utxo_word = v;
            self.ada_per_utxo_byte = Lovelace(v.0 / 8);
        } else {
            self.ada_per_utxo_byte = v;
        }
    }

    /// Default mainnet parameters (Conway era, approximate)
    pub fn mainnet_defaults() -> Self {
        ProtocolParameters {
            min_fee_a: 44,
            min_fee_b: 155381,
            max_block_body_size: 90112,
            max_tx_size: 16384,
            max_block_header_size: 1100,
            key_deposit: Lovelace(2_000_000),
            pool_deposit: Lovelace(500_000_000),
            e_max: 18,
            n_opt: 500,
            a0: Rational {
                numerator: 3,
                denominator: 10,
            },
            rho: Rational {
                numerator: 3,
                denominator: 1000,
            },
            tau: Rational {
                numerator: 2,
                denominator: 10,
            },
            min_pool_cost: Lovelace(170_000_000),
            ada_per_utxo_byte: Lovelace(4310),
            min_utxo_value: default_min_utxo_value(),
            coins_per_utxo_word: default_coins_per_utxo_word(),
            cost_models: CostModels::default(),
            execution_costs: ExUnitPrices {
                mem_price: Rational {
                    numerator: 577,
                    denominator: 10000,
                },
                step_price: Rational {
                    numerator: 721,
                    denominator: 10000000,
                },
            },
            max_tx_ex_units: ExUnits {
                mem: 14_000_000,
                steps: 10_000_000_000,
            },
            max_block_ex_units: ExUnits {
                mem: 62_000_000,
                steps: 40_000_000_000,
            },
            max_val_size: 5000,
            collateral_percentage: 150,
            max_collateral_inputs: 3,
            min_fee_ref_script_cost_per_byte: Rational {
                numerator: 15,
                denominator: 1,
            },
            drep_deposit: Lovelace(500_000_000),
            drep_activity: 20,
            gov_action_deposit: Lovelace(100_000_000_000),
            gov_action_lifetime: 6,
            committee_min_size: 7,
            committee_max_term_length: 146,
            dvt_pp_network_group: Rational {
                numerator: 67,
                denominator: 100,
            },
            dvt_pp_economic_group: Rational {
                numerator: 67,
                denominator: 100,
            },
            dvt_pp_technical_group: Rational {
                numerator: 67,
                denominator: 100,
            },
            dvt_pp_gov_group: Rational {
                numerator: 67,
                denominator: 100,
            },
            dvt_hard_fork: Rational {
                numerator: 60,
                denominator: 100,
            },
            dvt_no_confidence: Rational {
                numerator: 67,
                denominator: 100,
            },
            dvt_committee_normal: Rational {
                numerator: 67,
                denominator: 100,
            },
            dvt_committee_no_confidence: Rational {
                numerator: 60,
                denominator: 100,
            },
            dvt_constitution: Rational {
                numerator: 75,
                denominator: 100,
            },
            dvt_treasury_withdrawal: Rational {
                numerator: 67,
                denominator: 100,
            },
            pvt_motion_no_confidence: Rational {
                numerator: 51,
                denominator: 100,
            },
            pvt_committee_normal: Rational {
                numerator: 51,
                denominator: 100,
            },
            pvt_committee_no_confidence: Rational {
                numerator: 51,
                denominator: 100,
            },
            pvt_hard_fork: Rational {
                numerator: 51,
                denominator: 100,
            },
            pvt_pp_security_group: Rational {
                numerator: 51,
                denominator: 100,
            },
            protocol_version_major: 9,
            protocol_version_minor: 0,
            active_slots_coeff: 0.05,
            d: Rational {
                numerator: 0,
                denominator: 1,
            },
            // Dijkstra PParams (keys 34-37): None until a Dijkstra genesis or
            // governance ParameterChange populates them.
            max_ref_script_size_per_block: None,
            max_ref_script_size_per_tx: None,
            ref_script_cost_stride: None,
            ref_script_cost_multiplier: None,
        }
    }
}

/// Convert an f64 to a rational numerator/denominator pair.
///
/// Searches for the smallest denominator (up to common Cardano denominators,
/// then up to 1,000,000) that reconstructs the float exactly. For Cardano
/// protocol parameters this always finds a clean fraction (e.g., 0.05 = 1/20).
pub fn f64_to_rational(value: f64) -> (u64, u64) {
    if value == 0.0 {
        return (0, 1);
    }
    if value == 1.0 {
        return (1, 1);
    }
    // Try common denominators first (covers all known Cardano active_slot_coeff values)
    for den in [1, 2, 4, 5, 10, 20, 25, 50, 100, 200, 1000, 10000] {
        let num = (value * den as f64).round() as u64;
        let reconstructed = num as f64 / den as f64;
        if (reconstructed - value).abs() < 1e-15 {
            let g = gcd(num, den);
            return (num / g, den / g);
        }
    }
    // Fallback: use 1_000_000 as denominator
    let den = 1_000_000u64;
    let num = (value * den as f64).round() as u64;
    let g = gcd(num, den);
    (num / g, den / g)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Compute `ceil(numerator * k / f)` using integer arithmetic, where `f = f_num / f_den`.
///
/// This computes `ceil(numerator * k * f_den / f_num)` without floating-point.
/// Used for stability window (3k/f) and randomness stabilisation window (4k/f).
pub fn ceiling_div_by_rational(multiplier: u64, k: u64, f_num: u64, f_den: u64) -> u64 {
    assert!(f_num > 0, "active_slot_coeff numerator must be > 0");
    let numerator = multiplier as u128 * k as u128 * f_den as u128;
    let denominator = f_num as u128;
    numerator.div_ceil(denominator) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f64_to_rational_mainnet() {
        // Mainnet: active_slot_coeff = 0.05 = 1/20
        assert_eq!(f64_to_rational(0.05), (1, 20));
    }

    #[test]
    fn test_f64_to_rational_common_values() {
        assert_eq!(f64_to_rational(0.0), (0, 1));
        assert_eq!(f64_to_rational(1.0), (1, 1));
        assert_eq!(f64_to_rational(0.5), (1, 2));
        assert_eq!(f64_to_rational(0.1), (1, 10));
        assert_eq!(f64_to_rational(0.25), (1, 4));
    }

    #[test]
    fn test_active_slot_coeff_rational() {
        let params = ProtocolParameters::mainnet_defaults();
        assert_eq!(params.active_slot_coeff_rational(), (1, 20));
    }

    #[test]
    fn test_ceiling_div_by_rational_mainnet_randomness_window() {
        // Mainnet: 4 * 2160 / 0.05 = 4 * 2160 * 20 / 1 = 172800
        assert_eq!(ceiling_div_by_rational(4, 2160, 1, 20), 172800);
    }

    #[test]
    fn test_ceiling_div_by_rational_mainnet_stability_window() {
        // Mainnet: 3 * 2160 / 0.05 = 3 * 2160 * 20 / 1 = 129600
        assert_eq!(ceiling_div_by_rational(3, 2160, 1, 20), 129600);
    }

    #[test]
    fn test_ceiling_div_by_rational_preview() {
        // Preview: k=432, f=0.05 → 4 * 432 / 0.05 = 34560
        assert_eq!(ceiling_div_by_rational(4, 432, 1, 20), 34560);
        // 3k/f = 3 * 432 / 0.05 = 25920
        assert_eq!(ceiling_div_by_rational(3, 432, 1, 20), 25920);
    }

    #[test]
    fn test_ceiling_div_rounds_up() {
        // 4 * 1 / (1/3) = 12 exactly
        assert_eq!(ceiling_div_by_rational(4, 1, 1, 3), 12);
        // 4 * 1 / (2/3) = 6 exactly
        assert_eq!(ceiling_div_by_rational(4, 1, 2, 3), 6);
        // 4 * 1 / (1/7) = 28 exactly
        assert_eq!(ceiling_div_by_rational(4, 1, 1, 7), 28);
        // 3 * 1 / (2/5) = 7.5 → ceil = 8
        assert_eq!(ceiling_div_by_rational(3, 1, 2, 5), 8);
    }

    #[test]
    fn test_ceiling_div_exact_division() {
        // When the result is exact, no rounding should occur
        // 4 * 100 * 20 / 1 = 8000
        assert_eq!(ceiling_div_by_rational(4, 100, 1, 20), 8000);
    }

    // ── #462 Phase 4.3: CoinPerByte / txFeePerByte ───────────────────────────

    #[test]
    fn coin_per_byte_construction_and_accessor() {
        let c = CoinPerByte::new(44);
        assert_eq!(c.lovelace(), 44);
        assert_eq!(u64::from(c), 44);
        assert_eq!(CoinPerByte::from(155_381u64).lovelace(), 155_381);
        assert_eq!(format!("{c}"), "44");
    }

    /// `CoinPerByte` must serialise transparently as a bare `uint` in JSON to
    /// match Haskell's `deriving newtype (ToJSON, FromJSON)` semantics on
    /// `cardano-ledger`'s `CoinPerByte`. The JSON shape MUST NOT become an
    /// object — that would diverge from `cardano-cli protocol-parameters`.
    #[test]
    fn coin_per_byte_serde_is_transparent_uint() {
        let c = CoinPerByte::new(44);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "44");
        let parsed: CoinPerByte = serde_json::from_str("44").unwrap();
        assert_eq!(parsed, c);
    }

    /// `ProtocolParameters::tx_fee_per_byte()` is the Dijkstra-named accessor;
    /// it must surface exactly the same lovelace coefficient that
    /// `min_fee()` uses.
    #[test]
    fn tx_fee_per_byte_matches_min_fee_formula() {
        let mut pp = ProtocolParameters::mainnet_defaults();
        pp.min_fee_a = 44;
        pp.min_fee_b = 155_381;
        assert_eq!(pp.tx_fee_per_byte(), CoinPerByte::new(44));
        // fee = 44 * 200 + 155_381 = 164_181
        assert_eq!(pp.min_fee(200).0, 164_181);
        assert_eq!(
            pp.min_fee(200).0,
            pp.tx_fee_per_byte().lovelace() * 200 + pp.min_fee_b
        );
    }

    /// Setter round-trip via the typed `CoinPerByte` newtype.
    #[test]
    fn set_tx_fee_per_byte_round_trip() {
        let mut pp = ProtocolParameters::mainnet_defaults();
        pp.set_tx_fee_per_byte(CoinPerByte::new(99));
        assert_eq!(pp.min_fee_a, 99);
        assert_eq!(pp.tx_fee_per_byte(), CoinPerByte::new(99));
    }

    /// Phase 4.3 documents the Haskell rename `minFeeA` → `txFeePerByte`. The
    /// in-house JSON deserialiser must accept BOTH names so config files
    /// produced by upstream tooling (and our own historical fixtures) round
    /// trip without manual rewrite.
    #[test]
    fn protocol_params_accepts_tx_fee_per_byte_json_alias() {
        let base = ProtocolParameters::mainnet_defaults();
        let mut json = serde_json::to_value(&base).unwrap();
        // Replace the snake_case key with the Haskell/upstream camelCase name.
        let map = json.as_object_mut().unwrap();
        let v = map.remove("min_fee_a").unwrap();
        map.insert("txFeePerByte".to_string(), v);
        let parsed: ProtocolParameters = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.min_fee_a, base.min_fee_a);
    }

    /// And the legacy `minFeeA` Haskell-pre-Dijkstra alias must also parse.
    #[test]
    fn protocol_params_accepts_min_fee_a_camel_alias() {
        let base = ProtocolParameters::mainnet_defaults();
        let mut json = serde_json::to_value(&base).unwrap();
        let map = json.as_object_mut().unwrap();
        let v = map.remove("min_fee_a").unwrap();
        map.insert("minFeeA".to_string(), v);
        let parsed: ProtocolParameters = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.min_fee_a, base.min_fee_a);
    }

    // ── #919: per-era `min_coin_for_output` dispatch ─────────────────────────

    fn params_with_pv(pv_major: u64) -> ProtocolParameters {
        let mut p = ProtocolParameters::mainnet_defaults();
        p.protocol_version_major = pv_major;
        p.min_utxo_value = Lovelace(1_000_000);
        p.coins_per_utxo_word = Lovelace(34_482);
        p
    }

    #[test]
    fn min_coin_shelley_flat_regardless_of_value() {
        let p = params_with_pv(2);
        let ada_only = Value::lovelace(1);
        assert_eq!(
            p.min_coin_for_output(&ada_only, false, 84).0,
            1_000_000,
            "Shelley must return the flat minUTxOValue, not a size-scaled amount (#919 repro)"
        );
    }

    #[test]
    fn min_coin_allegra_flat() {
        let p = params_with_pv(3);
        let v = Value::lovelace(1);
        assert_eq!(p.min_coin_for_output(&v, false, 84).0, 1_000_000);
    }

    #[test]
    fn min_coin_mary_ada_only_flat() {
        let p = params_with_pv(4);
        let v = Value::lovelace(1);
        assert_eq!(p.min_coin_for_output(&v, false, 84).0, 1_000_000);
    }

    #[test]
    fn min_coin_mary_multi_asset_scaled_deposit_golden() {
        // Oracle golden: mv=1_000_000, 1 policy + one 0-byte asset name.
        // coinsPerUTxOWord = quot(1_000_000, 27) = 37_037.
        // size(v) = 11 (see mary_value_size_one_policy_one_zero_byte_asset_name).
        // min = max(1_000_000, 37_037 * (27 + 11)) = 1_407_406.
        let p = params_with_pv(4);
        let mut v = Value::lovelace(0);
        let policy = crate::hash::PolicyId::from_bytes([0x11u8; 28]);
        let mut assets = std::collections::BTreeMap::new();
        assets.insert(crate::value::AssetName::empty(), 1u64);
        v.multi_asset.insert(policy, assets);
        assert_eq!(p.min_coin_for_output(&v, false, 0).0, 1_407_406);
    }

    #[test]
    fn min_coin_alonzo_ada_only_no_datum_golden() {
        // Oracle golden: coinsPerUTxOWord=34_482, ada-only, no datum:
        // (27 + 2 + 0) * 34_482 = 29 * 34_482 = 999_978.
        let p = params_with_pv(5);
        let v = Value::lovelace(1);
        assert_eq!(p.min_coin_for_output(&v, false, 0).0, 999_978);
    }

    #[test]
    fn min_coin_alonzo_ada_only_with_datum_hash_golden() {
        // Oracle golden: (27 + 2 + 10) * 34_482 = 39 * 34_482 = 1_344_798.
        let p = params_with_pv(6);
        let v = Value::lovelace(1);
        assert_eq!(p.min_coin_for_output(&v, true, 0).0, 1_344_798);
    }

    #[test]
    fn min_coin_babbage_unchanged_serialized_size_formula() {
        let p = params_with_pv(7);
        let v = Value::lovelace(1);
        // 4310 * (160 + 84) = 4310 * 244 = 1_051_640 (the exact #919 repro
        // minimum that was WRONGLY applied to Shelley outputs pre-fix).
        assert_eq!(p.min_coin_for_output(&v, false, 84).0, 1_051_640);
    }

    #[test]
    fn min_coin_conway_unchanged_serialized_size_formula() {
        let p = params_with_pv(9);
        let v = Value::lovelace(1);
        assert_eq!(p.min_coin_for_output(&v, false, 84).0, 1_051_640);
    }
}
