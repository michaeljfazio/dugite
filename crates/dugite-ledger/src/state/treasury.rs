use std::fmt;
use tracing::{debug, warn};

/// Type alias for Lovelace (1 ADA = 1,000,000 Lovelace).
pub type Lovelace = u64;

/// Type alias for epoch number.
pub type Epoch = u64;

/// Accumulated treasury amounts from the current ledger state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreasuryAccumulator {
    /// Total Lovelace locked in UTxOs.
    pub utxo_lovelace: Lovelace,
    /// Total Lovelace in reward accounts.
    pub reward_lovelace: Lovelace,
}

impl TreasuryAccumulator {
    /// Creates a new `TreasuryAccumulator` with validated values.
    ///
    /// # Arguments
    ///
    /// * `utxo_lovelace` - Total Lovelace in UTxOs. Must not be [`Lovelace::MAX`].
    /// * `reward_lovelace` - Total Lovelace in reward accounts. Must not be [`Lovelace::MAX`].
    ///
    /// # Returns
    ///
    /// `Ok(TreasuryAccumulator)` if values are valid, otherwise `Err(TreasuryError::InvalidInput)`.
    pub fn new(utxo_lovelace: Lovelace, reward_lovelace: Lovelace) -> TreasuryResult<Self> {
        if utxo_lovelace == Lovelace::MAX {
            return Err(TreasuryError::InvalidInput {
                details: "utxo_lovelace cannot be Lovelace::MAX",
                value: utxo_lovelace,
            });
        }
        if reward_lovelace == Lovelace::MAX {
            return Err(TreasuryError::InvalidInput {
                details: "reward_lovelace cannot be Lovelace::MAX",
                value: reward_lovelace,
            });
        }
        Ok(Self {
            utxo_lovelace,
            reward_lovelace,
        })
    }
}

/// Protocol parameters relevant to treasury calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolParameters {
    /// Epoch these parameters are valid for.
    pub epoch: Epoch,
    /// Reserve depletion rate (ρ) – amount of reserves to deplete per epoch.
    pub reserve_depletion_rate: Lovelace,
    /// Treasury fee rate (τ) – unclaimed fees added to treasury.
    pub treasury_fee_rate: Lovelace,
}

impl ProtocolParameters {
    /// Creates a new `ProtocolParameters` with validated values.
    ///
    /// # Arguments
    ///
    /// * `epoch` - Epoch number. Must not be [`Epoch::MAX`].
    /// * `reserve_depletion_rate` - Must not cause arithmetic overflow in calculations.
    /// * `treasury_fee_rate` - Must not cause arithmetic overflow in calculations.
    ///
    /// # Returns
    ///
    /// `Ok(ProtocolParameters)` if values are valid, otherwise `Err(TreasuryError::InvalidInput)`.
    pub fn new(
        epoch: Epoch,
        reserve_depletion_rate: Lovelace,
        treasury_fee_rate: Lovelace,
    ) -> TreasuryResult<Self> {
        if epoch == Epoch::MAX {
            return Err(TreasuryError::InvalidInput {
                details: "epoch cannot be Epoch::MAX",
                value: 0, // no meaningful value
            });
        }
        // Check that addition of fee rate won't overflow after subtraction of depletion.
        // We only check the extreme large values here; actual overflow is checked in compute.
        if reserve_depletion_rate > Lovelace::MAX.saturating_sub(treasury_fee_rate) {
            return Err(TreasuryError::InvalidInput {
                details: "reserve_depletion_rate + treasury_fee_rate would overflow",
                value: 0,
            });
        }
        Ok(Self {
            epoch,
            reserve_depletion_rate,
            treasury_fee_rate,
        })
    }
}

/// Treasury state output after computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreasuryState {
    /// Epoch for which the treasury was computed.
    pub epoch: Epoch,
    /// Computed treasury balance.
    pub balance: Lovelace,
    /// Sum of UTxO Lovelace.
    pub utxo_sum: Lovelace,
    /// Sum of reward Lovelace.
    pub reward_sum: Lovelace,
    /// Absolute difference between computed and actual treasury (if any).
    pub divergence: Lovelace,
}

impl TreasuryState {
    /// Creates a new `TreasuryState`.
    ///
    /// # Arguments
    ///
    /// * `epoch` - The epoch for which this state is computed.
    /// * `balance` - The computed treasury balance.
    /// * `utxo_sum` - Sum of UTxO Lovelace.
    /// * `reward_sum` - Sum of reward Lovelace.
    /// * `divergence` - Difference between computed and actual treasury.
    pub fn new(
        epoch: Epoch,
        balance: Lovelace,
        utxo_sum: Lovelace,
        reward_sum: Lovelace,
        divergence: Lovelace,
    ) -> Self {
        Self {
            epoch,
            balance,
            utxo_sum,
            reward_sum,
            divergence,
        }
    }
}

/// Errors that can occur during treasury calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TreasuryError {
    /// Arithmetic overflow occurred.
    Overflow { operation: &'static str },
    /// Invalid input data.
    InvalidInput { details: &'static str, value: u64 },
}

impl fmt::Display for TreasuryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow { operation } => {
                write!(f, "arithmetic overflow in {}", operation)
            }
            Self::InvalidInput { details, value } => {
                write!(f, "invalid input: {} (value = {})", details, value)
            }
        }
    }
}

impl std::error::Error for TreasuryError {}

/// Convenience type alias for results returned by treasury functions.
pub type TreasuryResult<T> = Result<T, TreasuryError>;

/// The threshold (in Lovelace) above which a treasury divergence triggers a warning log.
///
/// Set to 1000 ADA (1,000,000 Lovelace = 1 ADA, so 1000 ADA = 1,000,000,000 Lovelace).
pub const DIVERGENCE_WARN_THRESHOLD_LOVELACE: Lovelace = 1_000_000_000;

/// Compute the expected treasury balance using the Conway formula.
///
/// # Formula
///
/// `treasury = total_coins - utxo - reward - reserve_depletion + unclaimed_fees`
///
/// where `reserve_depletion` is taken from `prev_pparams.reserve_depletion_rate` and
/// `unclaimed_fees` from `prev_pparams.treasury_fee_rate`.
///
/// # Arguments
///
/// * `total_coins` - Total coin supply (Lovelace). Must be less than [`Lovelace::MAX`].
/// * `accum` - Accumulated UTxO and reward amounts from the current ledger.
/// * `prev_pparams` - Protocol parameters from the previous epoch (used for reserve/fee rates).
///
/// # Returns
///
/// The computed treasury balance as a `Lovelace` value.
///
/// # Errors
///
/// Returns `TreasuryError::InvalidInput` if `accum.utxo_lovelace > total_coins`
/// or `accum.reward_lovelace > total_coins - utxo`.
/// Returns `TreasuryError::Overflow` if any arithmetic operation overflows.
#[must_use = "expected treasury value should be used"]
pub fn calculate_expected_treasury(
    total_coins: Lovelace,
    accum: &TreasuryAccumulator,
    prev_pparams: &ProtocolParameters,
) -> TreasuryResult<Lovelace> {
    // Validate total_coins
    if total_coins == Lovelace::MAX {
        return Err(TreasuryError::InvalidInput {
            details: "total_coins cannot be Lovelace::MAX",
            value: total_coins,
        });
    }

    // Validate UTxO against total coins
    if accum.utxo_lovelace > total_coins {
        return Err(TreasuryError::InvalidInput {
            details: "utxo_lovelace exceeds total_coins",
            value: accum.utxo_lovelace,
        });
    }

    // total_coins - utxo
    let remaining_after_utxo = total_coins
        .checked_sub(accum.utxo_lovelace)
        .ok_or_else(|| TreasuryError::Overflow {
            operation: "total_coins - utxo_lovelace",
        })?;

    // Validate reward against remaining after UTxO subtraction
    if accum.reward_lovelace > remaining_after_utxo {
        return Err(TreasuryError::InvalidInput {
            details: "reward_lovelace exceeds total_coins - utxo_lovelace",
            value: accum.reward_lovelace,
        });
    }

    // Subtract reward
    let after_reward = remaining_after_utxo
        .checked_sub(accum.reward_lovelace)
        .ok_or_else(|| TreasuryError::Overflow {
            operation: "remaining_after_utxo - reward_lovelace",
        })?;

    // Apply reserve depletion
    let after_depletion = after_reward
        .checked_sub(prev_pparams.reserve_depletion_rate)
        .ok_or_else(|| TreasuryError::Overflow {
            operation: "after_reward - reserve_depletion",
        })?;

    // Add unclaimed fees
    let treasury = after_depletion
        .checked_add(prev_pparams.treasury_fee_rate)
        .ok_or_else(|| TreasuryError::Overflow {
            operation: "after_depletion + fee_adjustment",
        })?;

    debug!(
        total_coins = %total_coins,
        utxo = %accum.utxo_lovelace,
        reward = %accum.reward_lovelace,
        reserve_depletion = %prev_pparams.reserve_depletion_rate,
        fee_adjustment = %prev_pparams.treasury_fee_rate,
        result = %treasury,
        "Expected treasury calculated"
    );

    Ok(treasury)
}

/// Full treasury computation, returning a [`TreasuryState`] with divergence.
///
/// # Arguments
///
/// * `total_coins` - Total coin supply (Lovelace).
/// * `accum` - Accumulated UTxO and reward amounts.
/// * `prev_pparams` - Protocol parameters from the previous epoch.
/// * `actual_treasury` - The actual treasury balance recorded in the ledger (for divergence).
///   Pass 0 if unknown or if divergence is not meaningful.
///
/// # Returns
///
/// [`TreasuryState`] with computed balance and divergence.
///
/// # Errors
///
/// Propagates any errors from [`calculate_expected_treasury`].
#[must_use = "treasury state should be used for further analysis"]
pub fn calculate_treasury_state(
    total_coins: Lovelace,
    accum: &TreasuryAccumulator,
    prev_pparams: &ProtocolParameters,
    actual_treasury: Lovelace,
) -> TreasuryResult<TreasuryState> {
    let balance = calculate_expected_treasury(total_coins, accum, prev_pparams)?;

    // Calculate absolute divergence
    let divergence = if balance >= actual_treasury {
        balance - actual_treasury
    } else {
        actual_treasury - balance
    };

    let state = TreasuryState {
        epoch: prev_pparams.epoch,
        balance,
        utxo_sum: accum.utxo_lovelace,
        reward_sum: accum.reward_lovelace,
        divergence,
    };

    validate_treasury_state(&state)?;

    Ok(state)
}

/// Validate treasury state for internal consistency.
///
/// Checks that:
/// - Balance does not exceed the sum of UTxO and rewards (soft warning).
/// - Divergence is within the configured warning threshold.
///
/// # Notes
///
/// This function issues warnings using the `tracing::warn!` macro. It does not return an error;
/// callers may escalate to errors if desired.
pub fn validate_treasury_state(state: &TreasuryState) -> TreasuryResult<()> {
    let utxo_reward_sum = state.utxo_sum.saturating_add(state.reward_sum);

    if state.balance > utxo_reward_sum {
        warn!(
            "Treasury balance exceeds utxo+reward sum: balance={}, utxo+reward={}",
            state.balance, utxo_reward_sum
        );
    }

    if state.divergence > DIVERGENCE_WARN_THRESHOLD_LOVELACE {
        warn!(
            "Large treasury divergence detected: divergence={} Lovelace (threshold={} Lovelace)",
            state.divergence, DIVERGENCE_WARN_THRESHOLD_LOVELACE
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helper functions for validation in constructors
// ---------------------------------------------------------------------------
fn validate_field_not_max(name: &'static str, value: u64) -> TreasuryResult<()> {
    if value == u64::MAX {
        return Err(TreasuryError::InvalidInput {
            details: name,
            value,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pparams(
        epoch: Epoch,
        reserve_depletion_rate: Lovelace,
        treasury_fee_rate: Lovelace,
    ) -> ProtocolParameters {
        ProtocolParameters::new(epoch, reserve_depletion_rate, treasury_fee_rate).unwrap()
    }

    #[test]
    fn test_accumulator_creation() {
        let acc = TreasuryAccumulator::new(100, 200).unwrap();
        assert_eq!(acc.utxo_lovelace, 100);
        assert_eq!(acc.reward_lovelace, 200);
    }

    #[test]
    fn test_accumulator_invalid_max() {
        assert!(TreasuryAccumulator::new(Lovelace::MAX, 0).is_err());
        assert!(TreasuryAccumulator::new(0, Lovelace::MAX).is_err());
    }

    #[test]
    fn test_protocol_params_valid() {
        let p = test_pparams(10, 500, 100);
        assert_eq!(p.epoch, 10);
        assert_eq!(p.reserve_depletion_rate, 500);
        assert_eq!(p.treasury_fee_rate, 100);
    }

    #[test]
    fn test_pparams_epoch_max() {
        assert!(ProtocolParameters::new(Epoch::MAX, 0, 0).is_err());
    }

    #[test]
    fn test_pparams_overflow_risk() {
        // reserve_depletion_rate + treasury_fee_rate would overflow u64
        assert!(ProtocolParameters::new(1, u64::MAX, 1).is_err());
        assert!(ProtocolParameters::new(1, 1, u64::MAX).is_err());
        // Fine if sum doesn't overflow
        assert!(ProtocolParameters::new(1, u64::MAX - 1, 1).is_ok());
    }

    #[test]
    fn test_calculate_expected_treasury_happy() {
        let total_coins = 1_000_000;
        let accum = TreasuryAccumulator::new(400_000, 300_000).unwrap();
        let pparams = test_pparams(5, 50_000, 10_000);

        // Expected: 1_000_000 - 400_000 - 300_000 - 50_000 + 10_000 = 260_000
        let result = calculate_expected_treasury(total_coins, &accum, &pparams).unwrap();
        assert_eq!(result, 260_000);
    }

    #[test]
    fn test_calculate_expected_treasury_utxo_exceeds_total() {
        let total_coins = 1000;
        let accum = TreasuryAccumulator::new(1500, 0).unwrap();
        let pparams = test_pparams(1, 0, 0);
        let result = calculate_expected_treasury(total_coins, &accum, &pparams);
        assert!(result.is_err());
        match result.unwrap_err() {
            TreasuryError::InvalidInput { details, .. } => {
                assert!(details.contains("utxo_lovelace exceeds total_coins"));
            }
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_calculate_expected_treasury_reward_exceeds_remaining() {
        let total_coins = 1000;
        let accum = TreasuryAccumulator::new(200, 900).unwrap(); // remaining after utxo = 800, reward > 800
        let pparams = test_pparams(1, 0, 0);
        let result = calculate_expected_treasury(total_coins, &accum, &pparams);
        assert!(result.is_err());
        match result.unwrap_err() {
            TreasuryError::InvalidInput { details, .. } => {
                assert!(details.contains("reward_lovelace exceeds"));
            }
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_calculate_expected_treasury_overflow_reserve_depletion() {
        let total_coins = 1000;
        let accum = TreasuryAccumulator::new(100, 100).unwrap();
        let pparams = test_pparams(1, 2000, 0); // after reward: 800, depletion 2000 -> underflow
        let result = calculate_expected_treasury(total_coins, &accum, &pparams);
        assert!(result.is_err());
        match result.unwrap_err() {
            TreasuryError::Overflow { operation } => {
                assert!(operation.contains("reserve_depletion"));
            }
            _ => panic!("Expected Overflow error"),
        }
    }

    #[test]
    fn test_calculate_expected_treasury_overflow_fee_addition() {
        // Make sure after_depletion + fee would overflow
        let total_coins = 1_000_000_000;
        let accum = TreasuryAccumulator::new(0, 0).unwrap();
        let pparams = test_pparams(1, 0, u64::MAX); // fee = u64::MAX
        let result = calculate_expected_treasury(total_coins, &accum, &pparams);
        // total_coins - 0 - 0 - 0 + u64::MAX -> overflow
        assert!(result.is_err());
        match result.unwrap_err() {
            TreasuryError::Overflow { operation } => {
                assert!(operation.contains("fee_adjustment") || operation.contains("after_depletion +"));
            }
            _ => panic!("Expected Overflow error"),
        }
    }

    #[test]
    fn test_calculate_treasury_state_with_divergence() {
        let total_coins = 10_000_000;
        let accum = TreasuryAccumulator::new(4_000_000, 3_000_000).unwrap();
        let pparams = test_pparams(10, 500_000, 100_000);
        // expected = 10M - 4M - 3M - 0.5M + 0.1M = 2.6M
        let actual_treasury = 2_500_000;
        let state = calculate_treasury_state(total_coins, &accum, &pparams, actual_treasury).unwrap();
        assert_eq!(state.epoch, 10);
        assert_eq!(state.balance, 2_600_000);
        assert_eq!(state.divergence, 100_000); // |2.6M - 2.5M|
    }

    #[test]
    fn test_validate_treasury_state_warns_on_large_divergence() {
        // We can't easily test log output, but we check no error returned.
        let state = TreasuryState::new(1, 100, 50, 30, DIVERGENCE_WARN_THRESHOLD_LOVELACE + 1);
        let result = validate_treasury_state(&state);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_treasury_state_balance_exceeds_utxo_reward() {
        let state = TreasuryState::new(1, 1000, 200, 300, 0);
        // balance 1000 > utxo+reward 500, should warn but not error
        let result = validate_treasury_state(&state);
        assert!(result.is_ok());
    }

    #[test]
    fn test_total_coins_max_is_invalid() {
        let accum = TreasuryAccumulator::new(0, 0).unwrap();
        let pparams = test_pparams(1, 0, 0);
        let result = calculate_expected_treasury(Lovelace::MAX, &accum, &pparams);
        assert!(result.is_err());
        match result.unwrap_err() {
            TreasuryError::InvalidInput { details, .. } => {
                assert!(details.contains("total_coins cannot be Lovelace::MAX"));
            }
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_treasury_state_new() {
        let ts = TreasuryState::new(5, 1_000, 400, 300, 50);
        assert_eq!(ts.epoch, 5);
        assert_eq!(ts.balance, 1_000);
        assert_eq!(ts.utxo_sum, 400);
        assert_eq!(ts.reward_sum, 300);
        assert_eq!(ts.divergence, 50);
    }
}