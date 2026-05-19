use dugite_primitives::hash::Hash32;
use dugite_primitives::time::{EpochLength, SlotNo};

// Epoch transition handling
//
// At each epoch boundary, the following happens:
// 1. Stake snapshot is taken (mark snapshot)
// 2. Rewards are calculated and distributed
// 3. Protocol parameters may be updated
// 4. New nonce is computed from VRF outputs
// 5. Leader schedule is derived for next epoch

/// Compute the epoch nonce from the previous nonce and VRF contributions.
///
/// The epoch nonce is: `hash(prev_nonce || eta_v)`
/// where eta_v is the hash of all VRF outputs from the first 2/3 of the epoch.
pub fn compute_epoch_nonce(prev_nonce: &Hash32, eta_v: &Hash32) -> Hash32 {
    let mut data = Vec::with_capacity(64);
    data.extend_from_slice(prev_nonce.as_bytes());
    data.extend_from_slice(eta_v.as_bytes());
    dugite_primitives::hash::blake2b_256(&data)
}

/// Determine if a slot is in the randomness stabilization window
/// (first 4k/f slots of the epoch, where VRF outputs contribute to nonce)
///
/// Issue #545 E9: epoch_length == 0 would cause a modulo-by-zero panic.
/// ShelleyGenesis::validate() rejects this at startup; checked_rem here
/// provides defense-in-depth and matches Haskell's genesis validation
/// (`check_genesis_parameters` rejects epochLength == 0).
pub fn in_nonce_contribution_window(
    slot: SlotNo,
    epoch_length: EpochLength,
    stability_window: u64,
) -> bool {
    let slot_in_epoch = match slot.0.checked_rem(epoch_length.0) {
        Some(r) => r,
        // epoch_length == 0 is a degenerate genesis; treat every slot as
        // outside the window so no nonce contributions are accepted.
        None => return false,
    };
    slot_in_epoch < stability_window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_nonce_deterministic() {
        let prev = Hash32::from_bytes([1u8; 32]);
        let eta = Hash32::from_bytes([2u8; 32]);

        let nonce1 = compute_epoch_nonce(&prev, &eta);
        let nonce2 = compute_epoch_nonce(&prev, &eta);
        assert_eq!(nonce1, nonce2);
    }

    #[test]
    fn test_epoch_nonce_different_inputs() {
        let prev = Hash32::from_bytes([1u8; 32]);
        let eta1 = Hash32::from_bytes([2u8; 32]);
        let eta2 = Hash32::from_bytes([3u8; 32]);

        let nonce1 = compute_epoch_nonce(&prev, &eta1);
        let nonce2 = compute_epoch_nonce(&prev, &eta2);
        assert_ne!(nonce1, nonce2);
    }

    #[test]
    fn test_nonce_contribution_window() {
        let epoch_length = EpochLength(432000);
        let stability_window = 129600; // 3k/f

        // First slot of epoch: in window
        assert!(in_nonce_contribution_window(
            SlotNo(0),
            epoch_length,
            stability_window
        ));

        // Just before window ends
        assert!(in_nonce_contribution_window(
            SlotNo(129599),
            epoch_length,
            stability_window
        ));

        // After window
        assert!(!in_nonce_contribution_window(
            SlotNo(129600),
            epoch_length,
            stability_window
        ));
    }

    /// Issue #545 E9: epoch_length == 0 must not panic (checked_rem defense-in-depth).
    /// A degenerate genesis with epochLength=0 is rejected by ShelleyGenesis::validate()
    /// at startup; but the function itself must not panic if called with 0.
    #[test]
    fn issue_545_e9_epoch_length_zero_does_not_panic() {
        let epoch_length = EpochLength(0);
        // Should return false without panicking.
        let result = in_nonce_contribution_window(SlotNo(0), epoch_length, 100);
        assert!(!result, "epoch_length=0 should return false (not panic)");

        let result2 = in_nonce_contribution_window(SlotNo(u64::MAX), epoch_length, 100);
        assert!(
            !result2,
            "epoch_length=0, slot=MAX should return false (not panic)"
        );
    }
}
