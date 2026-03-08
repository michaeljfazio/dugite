/// VRF (Verifiable Random Function) support
///
/// In Cardano's Ouroboros Praos, VRF is used for:
/// 1. Leader election: determining if a stake pool can produce a block in a given slot
/// 2. Epoch nonce: contributing randomness to the epoch nonce
///
/// The VRF implementation uses ECVRF-ED25519-SHA512-Elligator2
/// (IETF draft-irtf-cfrg-vrf-03) as used by the Cardano reference node.
use thiserror::Error;
use vrf_dalek::vrf03::{PublicKey03, VrfProof03};

#[derive(Error, Debug)]
pub enum VrfError {
    #[error("Invalid VRF proof: {0}")]
    InvalidProof(String),
    #[error("Invalid VRF public key")]
    InvalidPublicKey,
    #[error("VRF verification failed")]
    VerificationFailed,
}

/// Verify a VRF proof and return the 64-byte VRF output.
///
/// - `vrf_vkey`: 32-byte VRF verification key from the block header
/// - `proof_bytes`: 80-byte VRF proof from the block header
/// - `seed`: the VRF input (eta_v || slot for leader, eta_v || epoch for nonce)
///
/// Returns the 64-byte VRF output on success.
pub fn verify_vrf_proof(
    vrf_vkey: &[u8],
    proof_bytes: &[u8],
    seed: &[u8],
) -> Result<[u8; 64], VrfError> {
    if vrf_vkey.len() != 32 {
        return Err(VrfError::InvalidPublicKey);
    }
    if proof_bytes.len() != 80 {
        return Err(VrfError::InvalidProof(format!(
            "expected 80 bytes, got {}",
            proof_bytes.len()
        )));
    }

    let mut pk_bytes = [0u8; 32];
    pk_bytes.copy_from_slice(vrf_vkey);
    let public_key = PublicKey03::from_bytes(&pk_bytes);

    let mut proof_arr = [0u8; 80];
    proof_arr.copy_from_slice(proof_bytes);
    let proof =
        VrfProof03::from_bytes(&proof_arr).map_err(|e| VrfError::InvalidProof(format!("{e:?}")))?;

    proof
        .verify(&public_key, seed)
        .map_err(|_| VrfError::VerificationFailed)
}

/// Extract the VRF output hash from a proof without verification.
///
/// This is used when you need the output value (e.g., for the leader
/// eligibility check) but have already verified the proof, or during
/// initial sync where verification may be deferred.
pub fn vrf_proof_to_hash(proof_bytes: &[u8]) -> Result<[u8; 64], VrfError> {
    if proof_bytes.len() != 80 {
        return Err(VrfError::InvalidProof(format!(
            "expected 80 bytes, got {}",
            proof_bytes.len()
        )));
    }

    let mut proof_arr = [0u8; 80];
    proof_arr.copy_from_slice(proof_bytes);
    let proof =
        VrfProof03::from_bytes(&proof_arr).map_err(|e| VrfError::InvalidProof(format!("{e:?}")))?;

    Ok(proof.proof_to_hash())
}

/// Check if a VRF output certifies leader election for a given slot
///
/// The leader check compares: VRF_output < 2^512 * phi_f(sigma)
/// where phi_f(sigma) = 1 - (1 - f)^sigma
///   f = active slot coefficient
///   sigma = relative stake of the pool
pub fn check_leader_value(vrf_output: &[u8], relative_stake: f64, active_slot_coeff: f64) -> bool {
    // Convert VRF output to a value in [0, 1)
    let vrf_value = vrf_output_to_fraction(vrf_output);

    // phi_f(sigma) = 1 - (1 - f)^sigma
    let threshold = 1.0 - (1.0 - active_slot_coeff).powf(relative_stake);

    vrf_value < threshold
}

fn vrf_output_to_fraction(output: &[u8]) -> f64 {
    // Take first 8 bytes and convert to a fraction in [0, 1)
    if output.len() < 8 {
        return 0.0;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&output[..8]);
    let value = u64::from_be_bytes(bytes);
    value as f64 / u64::MAX as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leader_check() {
        // A pool with 100% stake and f=0.05 should almost always be elected
        assert!(check_leader_value(&[0u8; 32], 1.0, 0.05));

        // A pool with 0% stake should never be elected
        assert!(!check_leader_value(&[128u8; 32], 0.0, 0.05));
    }

    #[test]
    fn test_vrf_verify_known_vector() {
        // Test vector from IOG's VRF implementation (draft-03)
        // Secret key: 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
        // Public key: d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
        // Proof: b6b4699f87d56126c9117a7da55bd0085246f4c56dbc95d20172612e9d38e8d7
        //        ca65e573a126ed88d4e30a46f80a666854d675cf3ba81de0de043c3774f06156
        //        0f55edc256a787afe701677c0f602900
        // Output: 5b49b554d05c0cd5a5325376b3387de59d924fd1e13ded44648ab33c21349a60
        //         3f25b84ec5ed887995b33da5e3bfcb87cd2f64521c4c62cf825cffabbe5d31cc
        // Alpha (input): empty

        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        let proof = hex::decode(
            "b6b4699f87d56126c9117a7da55bd0085246f4c56dbc95d20172612e9d38e8d7\
             ca65e573a126ed88d4e30a46f80a666854d675cf3ba81de0de043c3774f06156\
             0f55edc256a787afe701677c0f602900",
        )
        .unwrap();
        let expected_output = hex::decode(
            "5b49b554d05c0cd5a5325376b3387de59d924fd1e13ded44648ab33c21349a60\
             3f25b84ec5ed887995b33da5e3bfcb87cd2f64521c4c62cf825cffabbe5d31cc",
        )
        .unwrap();

        let result = verify_vrf_proof(&pk, &proof, &[]).unwrap();
        assert_eq!(&result[..], &expected_output[..]);
    }

    #[test]
    fn test_vrf_verify_with_alpha() {
        // Test vector with alpha_string = 0x72
        let pk = hex::decode("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
            .unwrap();
        let proof = hex::decode(
            "ae5b66bdf04b4c010bfe32b2fc126ead2107b697634f6f7337b9bff8785ee111\
             200095ece87dde4dbe87343f6df3b107d91798c8a7eb1245d3bb9c5aafb09335\
             8c13e6ae1111a55717e895fd15f99f07",
        )
        .unwrap();
        let expected_output = hex::decode(
            "94f4487e1b2fec954309ef1289ecb2e15043a2461ecc7b2ae7d4470607ef82eb\
             1cfa97d84991fe4a7bfdfd715606bc27e2967a6c557cfb5875879b671740b7d8",
        )
        .unwrap();

        let result = verify_vrf_proof(&pk, &proof, &[0x72]).unwrap();
        assert_eq!(&result[..], &expected_output[..]);
    }

    #[test]
    fn test_vrf_verify_invalid_proof() {
        let pk = hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
            .unwrap();
        // Corrupted proof
        let proof = vec![0u8; 80];
        let result = verify_vrf_proof(&pk, &proof, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_vrf_proof_to_hash() {
        let proof = hex::decode(
            "b6b4699f87d56126c9117a7da55bd0085246f4c56dbc95d20172612e9d38e8d7\
             ca65e573a126ed88d4e30a46f80a666854d675cf3ba81de0de043c3774f06156\
             0f55edc256a787afe701677c0f602900",
        )
        .unwrap();
        let expected = hex::decode(
            "5b49b554d05c0cd5a5325376b3387de59d924fd1e13ded44648ab33c21349a60\
             3f25b84ec5ed887995b33da5e3bfcb87cd2f64521c4c62cf825cffabbe5d31cc",
        )
        .unwrap();

        let output = vrf_proof_to_hash(&proof).unwrap();
        assert_eq!(&output[..], &expected[..]);
    }

    #[test]
    fn test_vrf_wrong_key_size() {
        assert!(verify_vrf_proof(&[0u8; 16], &[0u8; 80], &[]).is_err());
    }

    #[test]
    fn test_vrf_wrong_proof_size() {
        assert!(verify_vrf_proof(&[0u8; 32], &[0u8; 40], &[]).is_err());
    }
}
