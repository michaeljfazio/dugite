use torsten_primitives::hash::Hash32;
use torsten_primitives::time::SlotNo;

// Slot leader check
//
// A stake pool is elected as slot leader if its VRF output for the slot
// satisfies: vrf_output < threshold(stake, f)
//
// The threshold function is: phi_f(sigma) = 1 - (1-f)^sigma
// where f is the active slot coefficient and sigma is relative stake.

/// Check if a pool is the slot leader for a given slot.
pub fn is_slot_leader(vrf_output: &[u8], relative_stake: f64, active_slot_coeff: f64) -> bool {
    torsten_crypto::vrf::check_leader_value(vrf_output, relative_stake, active_slot_coeff)
}

/// Compute the VRF input for a given slot
///
/// VRF input = epoch_nonce || slot_number (40 bytes total)
pub fn vrf_input(epoch_nonce: &Hash32, slot: SlotNo) -> Vec<u8> {
    let mut data = Vec::with_capacity(40);
    data.extend_from_slice(epoch_nonce.as_bytes());
    data.extend_from_slice(&slot.0.to_be_bytes());
    data
}

/// Expected number of blocks per epoch
pub fn expected_blocks_per_epoch(epoch_length: u64, active_slot_coeff: f64) -> f64 {
    epoch_length as f64 * active_slot_coeff
}

/// A slot where the pool is elected as leader
#[derive(Debug, Clone)]
pub struct LeaderSlot {
    pub slot: SlotNo,
    pub vrf_output: [u8; 64],
    pub vrf_proof: [u8; 80],
}

/// Compute the leader schedule for a given epoch.
///
/// Returns all slots within the epoch where the pool (identified by its VRF secret key)
/// is elected as slot leader.
///
/// - `vrf_skey`: 32-byte VRF secret key
/// - `epoch_nonce`: the epoch's nonce
/// - `epoch_start_slot`: first slot of the epoch
/// - `epoch_length`: number of slots in the epoch
/// - `relative_stake`: pool's stake fraction (0.0 to 1.0)
/// - `active_slot_coeff`: protocol parameter f (typically 0.05)
pub fn compute_leader_schedule(
    vrf_skey: &[u8; 32],
    epoch_nonce: &Hash32,
    epoch_start_slot: u64,
    epoch_length: u64,
    relative_stake: f64,
    active_slot_coeff: f64,
) -> Vec<LeaderSlot> {
    let mut schedule = Vec::new();

    for offset in 0..epoch_length {
        let slot = SlotNo(epoch_start_slot + offset);
        let seed = vrf_input(epoch_nonce, slot);

        if let Ok((proof, output)) = torsten_crypto::vrf::generate_vrf_proof(vrf_skey, &seed) {
            if is_slot_leader(&output, relative_stake, active_slot_coeff) {
                schedule.push(LeaderSlot {
                    slot,
                    vrf_output: output,
                    vrf_proof: proof,
                });
            }
        }
    }

    schedule
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vrf_input() {
        let nonce = Hash32::from_bytes([1u8; 32]);
        let input = vrf_input(&nonce, SlotNo(100));
        assert_eq!(input.len(), 40); // 32 bytes nonce + 8 bytes slot
    }

    #[test]
    fn test_expected_blocks() {
        let expected = expected_blocks_per_epoch(432000, 0.05);
        assert!((expected - 21600.0).abs() < 0.1);
    }

    #[test]
    fn test_full_stake_leader() {
        // Pool with 100% stake and low VRF output should be leader
        assert!(is_slot_leader(&[0u8; 32], 1.0, 0.05));
    }

    #[test]
    fn test_zero_stake_not_leader() {
        assert!(!is_slot_leader(&[128u8; 32], 0.0, 0.05));
    }

    #[test]
    fn test_leader_schedule() {
        let kp = torsten_crypto::vrf::generate_vrf_keypair();
        let epoch_nonce = Hash32::from_bytes([42u8; 32]);

        // Compute schedule for 1000 slots with 100% stake
        let schedule = compute_leader_schedule(
            &kp.secret_key,
            &epoch_nonce,
            0,    // epoch start slot
            1000, // epoch length
            1.0,  // 100% stake
            0.05, // active slot coefficient
        );

        // With f=0.05 and 100% stake, expect ~50 leader slots out of 1000
        assert!(
            !schedule.is_empty(),
            "Should have some leader slots with 100% stake"
        );
        assert!(
            schedule.len() < 200,
            "Should not have too many slots with f=0.05"
        );

        // Verify each slot's VRF proof
        for leader in &schedule {
            let seed = vrf_input(&epoch_nonce, leader.slot);
            let verified =
                torsten_crypto::vrf::verify_vrf_proof(&kp.public_key, &leader.vrf_proof, &seed);
            assert!(verified.is_ok(), "VRF proof should verify");
            assert_eq!(verified.unwrap(), leader.vrf_output);
        }
    }

    #[test]
    fn test_leader_schedule_zero_stake() {
        let kp = torsten_crypto::vrf::generate_vrf_keypair();
        let epoch_nonce = Hash32::from_bytes([0u8; 32]);

        let schedule = compute_leader_schedule(&kp.secret_key, &epoch_nonce, 0, 1000, 0.0, 0.05);

        assert!(schedule.is_empty(), "Zero stake should never be leader");
    }
}
