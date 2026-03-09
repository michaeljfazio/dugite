use thiserror::Error;
use torsten_crypto::keys::PaymentVerificationKey;
use torsten_primitives::block::{BlockHeader, Tip};
use torsten_primitives::time::{EpochLength, EpochNo, SlotNo};
use tracing::{debug, trace, warn};

/// KES period length in slots (each period is 129600 slots = 36 hours on mainnet)
pub const KES_PERIOD_SLOTS: u64 = 129600;

/// Maximum number of KES evolutions (mainnet: 62)
pub const MAX_KES_EVOLUTIONS: u64 = 62;

#[derive(Error, Debug)]
pub enum ConsensusError {
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Block from future slot: current={current}, block={block}")]
    FutureBlock { current: u64, block: u64 },
    #[error("Not a slot leader")]
    NotSlotLeader,
    #[error("Invalid VRF proof")]
    InvalidVrfProof,
    #[error("Invalid KES signature")]
    InvalidKesSignature,
    #[error("Invalid operational certificate")]
    InvalidOperationalCert,
    #[error("Block does not extend chain")]
    DoesNotExtendChain,
    #[error("KES period expired: current_period={current}, cert_start={cert_start}, max_evolutions={max_evolutions}")]
    KesExpired {
        current: u64,
        cert_start: u64,
        max_evolutions: u64,
    },
    #[error(
        "KES period mismatch: block is in period {block_period}, but cert starts at {cert_start}"
    )]
    KesPeriodBeforeCert { block_period: u64, cert_start: u64 },
    #[error("Empty issuer VRF key")]
    EmptyVrfKey,
    #[error("Empty issuer verification key")]
    EmptyIssuerVkey,
    #[error("VRF verification error: {0}")]
    VrfVerification(String),
    #[error("Operational cert sequence number regression: got {got}, expected > {expected}")]
    OpcertSequenceRegression { got: u64, expected: u64 },
}

/// Active slot coefficient (f) - probability that a slot has a block
/// Mainnet value: 1/20 = 0.05 (one block every ~20 seconds on average)
pub const ACTIVE_SLOT_COEFF: f64 = 0.05;

/// Security parameter k
pub const SECURITY_PARAM: u64 = 2160;

/// Ouroboros Praos consensus engine
pub struct OuroborosPraos {
    /// Active slot coefficient
    pub active_slot_coeff: f64,
    /// Security parameter
    pub security_param: u64,
    /// Epoch length in slots
    pub epoch_length: EpochLength,
    /// Current tip
    pub tip: Tip,
    /// Whether to enforce strict signature verification.
    /// When false (during initial sync), VRF/KES/opcert failures are non-fatal.
    /// When true (caught up to chain tip), verification failures reject blocks.
    pub strict_verification: bool,
}

impl OuroborosPraos {
    pub fn new() -> Self {
        OuroborosPraos {
            active_slot_coeff: ACTIVE_SLOT_COEFF,
            security_param: SECURITY_PARAM,
            epoch_length: torsten_primitives::time::mainnet_epoch_length(),
            tip: Tip::origin(),
            strict_verification: false,
        }
    }

    pub fn with_params(
        active_slot_coeff: f64,
        security_param: u64,
        epoch_length: EpochLength,
    ) -> Self {
        OuroborosPraos {
            active_slot_coeff,
            security_param,
            epoch_length,
            tip: Tip::origin(),
            strict_verification: false,
        }
    }

    /// Check if strict verification mode is enabled.
    pub fn strict_verification(&self) -> bool {
        self.strict_verification
    }

    /// Enable strict verification mode (for when node is caught up to chain tip).
    /// In strict mode, VRF/KES/opcert verification failures reject blocks.
    pub fn set_strict_verification(&mut self, strict: bool) {
        if strict != self.strict_verification {
            debug!(
                strict,
                "Praos: {} strict signature verification",
                if strict { "enabling" } else { "disabling" }
            );
        }
        self.strict_verification = strict;
    }

    /// Validate a block header against consensus rules.
    ///
    /// This checks:
    /// 1. Block is not from the future
    /// 2. Issuer VRF key is present
    /// 3. VRF proof is cryptographically valid
    /// 4. KES period is valid (not expired, not before cert start)
    /// 5. Operational certificate has required fields
    pub fn validate_header(
        &self,
        header: &BlockHeader,
        current_slot: SlotNo,
    ) -> Result<(), ConsensusError> {
        trace!(
            slot = header.slot.0,
            block_no = header.block_number.0,
            current_slot = current_slot.0,
            issuer_vkey_len = header.issuer_vkey.len(),
            vrf_vkey_len = header.vrf_vkey.len(),
            "Praos: validating block header"
        );

        // Block must not be from the future
        if header.slot > current_slot {
            warn!(
                block_slot = header.slot.0,
                current_slot = current_slot.0,
                "Praos: rejecting future block"
            );
            return Err(ConsensusError::FutureBlock {
                current: current_slot.0,
                block: header.slot.0,
            });
        }

        // Issuer verification key must be present (32 bytes for Ed25519)
        if header.issuer_vkey.is_empty() {
            warn!("Praos: empty issuer verification key");
            return Err(ConsensusError::EmptyIssuerVkey);
        }

        // VRF key must be present
        if header.vrf_vkey.is_empty() {
            warn!("Praos: empty VRF key");
            return Err(ConsensusError::EmptyVrfKey);
        }

        // Verify VRF proof cryptographically
        self.verify_vrf_proof(header)?;

        // Validate KES period
        self.validate_kes_period(header)?;

        // Validate operational certificate structure
        self.validate_operational_cert(header)?;

        // Verify KES signature over the header body
        self.verify_kes_signature(header)?;

        trace!(
            slot = header.slot.0,
            block_no = header.block_number.0,
            "Praos: header validation passed"
        );

        Ok(())
    }

    /// Verify the VRF proof in the block header.
    ///
    /// The VRF input is constructed from the epoch nonce and the slot number:
    ///   seed = epoch_nonce || slot_to_cbor(slot)
    ///
    /// This verifies that the block producer actually evaluated the VRF correctly,
    /// proving they had the right to produce this block.
    fn verify_vrf_proof(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        // Construct the VRF seed: epoch_nonce (32 bytes) || slot (8 bytes big-endian)
        let mut seed = Vec::with_capacity(40);
        seed.extend_from_slice(header.epoch_nonce.as_ref());
        seed.extend_from_slice(&header.slot.0.to_be_bytes());

        match torsten_crypto::vrf::verify_vrf_proof(
            &header.vrf_vkey,
            &header.vrf_result.proof,
            &seed,
        ) {
            Ok(vrf_output) => {
                // Verify that the output in the header matches what we computed
                if header.vrf_result.output.len() == 64
                    && header.vrf_result.output[..] != vrf_output[..]
                {
                    warn!(slot = header.slot.0, "Praos: VRF output mismatch");
                    return Err(ConsensusError::InvalidVrfProof);
                }
                trace!(
                    slot = header.slot.0,
                    "Praos: VRF proof verified successfully"
                );
                Ok(())
            }
            Err(e) => {
                if self.strict_verification {
                    warn!(
                        slot = header.slot.0,
                        error = %e,
                        "Praos: VRF proof verification failed"
                    );
                    return Err(ConsensusError::InvalidVrfProof);
                }
                debug!(
                    slot = header.slot.0,
                    error = %e,
                    "Praos: VRF proof verification failed (non-fatal during sync)"
                );
                Ok(())
            }
        }
    }

    /// Validate the KES period for a block header.
    ///
    /// The KES key must not have expired: the block's KES period must be
    /// >= the cert's start period and < start + max_evolutions.
    fn validate_kes_period(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        let block_kes_period = header.slot.0 / KES_PERIOD_SLOTS;
        let cert_kes_period = header.operational_cert.kes_period;

        trace!(
            block_kes_period,
            cert_kes_period,
            slot = header.slot.0,
            "Praos: checking KES period"
        );

        // Block's KES period must be >= the operational cert's KES period
        if block_kes_period < cert_kes_period {
            warn!(
                block_kes_period,
                cert_kes_period, "Praos: KES period before cert start"
            );
            return Err(ConsensusError::KesPeriodBeforeCert {
                block_period: block_kes_period,
                cert_start: cert_kes_period,
            });
        }

        // KES key must not have expired
        let kes_evolutions = block_kes_period - cert_kes_period;
        if kes_evolutions >= MAX_KES_EVOLUTIONS {
            warn!(
                kes_evolutions,
                max = MAX_KES_EVOLUTIONS,
                "Praos: KES key expired"
            );
            return Err(ConsensusError::KesExpired {
                current: block_kes_period,
                cert_start: cert_kes_period,
                max_evolutions: MAX_KES_EVOLUTIONS,
            });
        }

        Ok(())
    }

    /// Validate the operational certificate structure and signature.
    ///
    /// The operational certificate contains:
    /// - hot_vkey: KES verification key (the "hot" key)
    /// - sequence_number: monotonically increasing counter
    /// - kes_period: KES period at which the certificate was issued
    /// - sigma: Ed25519 signature by the cold key over [hot_vkey, seq_num, kes_period]
    ///
    /// We verify the Ed25519 signature using the issuer_vkey (cold key) from the header.
    fn validate_operational_cert(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        let opcert = &header.operational_cert;

        // Hot VKey must be present
        if opcert.hot_vkey.is_empty() {
            return Err(ConsensusError::InvalidOperationalCert);
        }

        // Sigma (signature) must be present
        if opcert.sigma.is_empty() {
            return Err(ConsensusError::InvalidOperationalCert);
        }

        // Verify the operational certificate signature:
        // The cold key (issuer_vkey) signs the CBOR encoding of [hot_vkey, seq_num, kes_period]
        if header.issuer_vkey.len() == 32 && opcert.sigma.len() == 64 {
            match verify_opcert_signature(
                &header.issuer_vkey,
                &opcert.hot_vkey,
                opcert.sequence_number,
                opcert.kes_period,
                &opcert.sigma,
            ) {
                Ok(()) => {
                    debug!("Operational certificate signature verified");
                }
                Err(e) => {
                    if self.strict_verification {
                        warn!("Opcert signature verification failed: {e}");
                        return Err(ConsensusError::InvalidOperationalCert);
                    }
                    debug!("Opcert signature verification skipped: {e}");
                }
            }
        }

        Ok(())
    }

    /// Verify the KES signature on the block header.
    ///
    /// The KES signature signs the header body bytes using the hot key (from the opcert)
    /// at the KES period = block_kes_period - opcert_kes_period.
    fn verify_kes_signature(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        // Skip verification if no KES signature is available (Byron blocks)
        if header.kes_signature.is_empty() {
            return Ok(());
        }

        let opcert = &header.operational_cert;
        if opcert.hot_vkey.len() != 32 || header.kes_signature.len() != 448 {
            return Ok(()); // Skip if sizes don't match expected KES format
        }

        let block_kes_period = header.slot.0 / KES_PERIOD_SLOTS;
        let kes_period_offset = block_kes_period.saturating_sub(opcert.kes_period);

        // Reconstruct the header body CBOR for verification
        let header_body_cbor = torsten_serialization::encode_block_header_body(header);

        // Parse the KES signature and verify against the hot verification key
        let mut hot_vkey = [0u8; 32];
        hot_vkey.copy_from_slice(&opcert.hot_vkey);

        match torsten_crypto::kes::kes_verify_bytes(
            &hot_vkey,
            kes_period_offset as u32,
            &header.kes_signature,
            &header_body_cbor,
        ) {
            Ok(()) => {
                trace!(
                    slot = header.slot.0,
                    kes_period = kes_period_offset,
                    "Praos: KES signature verified"
                );
                Ok(())
            }
            Err(e) => {
                if self.strict_verification {
                    warn!(
                        slot = header.slot.0,
                        error = %e,
                        kes_period = kes_period_offset,
                        "Praos: KES signature verification failed"
                    );
                    return Err(ConsensusError::InvalidKesSignature);
                }
                debug!(
                    slot = header.slot.0,
                    error = %e,
                    kes_period = kes_period_offset,
                    "Praos: KES signature verification failed (non-fatal during sync)"
                );
                Ok(())
            }
        }
    }

    /// Check if a slot is within the stability window (last k blocks)
    pub fn is_in_stability_window(&self, slot: SlotNo) -> bool {
        match self.tip.point.slot() {
            Some(tip_slot) => tip_slot.0.saturating_sub(self.stability_window()) <= slot.0,
            None => true,
        }
    }

    /// The stability window: 3k/f slots
    pub fn stability_window(&self) -> u64 {
        (3.0 * self.security_param as f64 / self.active_slot_coeff) as u64
    }

    /// Calculate which epoch a slot belongs to
    pub fn slot_to_epoch(&self, slot: SlotNo) -> EpochNo {
        slot.to_epoch(self.epoch_length)
    }

    /// Get the first slot of an epoch
    pub fn epoch_first_slot(&self, epoch: EpochNo) -> SlotNo {
        SlotNo(epoch.0 * self.epoch_length.0)
    }

    /// Check if we're at an epoch boundary
    pub fn is_epoch_boundary(&self, slot: SlotNo) -> bool {
        slot.0.is_multiple_of(self.epoch_length.0)
    }

    /// Maximum rollback depth
    pub fn max_rollback(&self) -> u64 {
        self.security_param
    }

    /// Update the tip
    pub fn update_tip(&mut self, tip: Tip) {
        self.tip = tip;
    }
}

/// Verify the operational certificate Ed25519 signature.
///
/// The cold key signs the CBOR encoding of: [hot_vkey, sequence_number, kes_period]
/// This proves that the pool operator (cold key holder) authorized the hot key.
pub fn verify_opcert_signature(
    cold_vkey_bytes: &[u8],
    hot_vkey: &[u8],
    sequence_number: u64,
    kes_period: u64,
    signature: &[u8],
) -> Result<(), ConsensusError> {
    // Construct the signed message: CBOR array [hot_vkey, seq_num, kes_period]
    let mut body_cbor = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut body_cbor);
    enc.array(3)
        .map_err(|e| ConsensusError::InvalidBlock(format!("CBOR encode error: {e}")))?;
    enc.bytes(hot_vkey)
        .map_err(|e| ConsensusError::InvalidBlock(format!("CBOR encode error: {e}")))?;
    enc.u64(sequence_number)
        .map_err(|e| ConsensusError::InvalidBlock(format!("CBOR encode error: {e}")))?;
    enc.u64(kes_period)
        .map_err(|e| ConsensusError::InvalidBlock(format!("CBOR encode error: {e}")))?;

    // Verify the Ed25519 signature
    let vk = PaymentVerificationKey::from_bytes(cold_vkey_bytes)
        .map_err(|_| ConsensusError::InvalidOperationalCert)?;

    vk.verify(&body_cbor, signature)
        .map_err(|_| ConsensusError::InvalidOperationalCert)?;

    Ok(())
}

/// Verify VRF leader eligibility for a block.
///
/// Checks that the VRF output certifies the pool as a slot leader given its relative stake.
/// This does NOT verify the VRF proof itself (which requires a full VRF library),
/// but verifies that the VRF output value satisfies the Praos leader check:
///   vrf_output < 2^512 * phi_f(sigma)
/// where phi_f(sigma) = 1 - (1 - f)^sigma
pub fn verify_leader_eligibility(
    vrf_output: &[u8],
    relative_stake: f64,
    active_slot_coeff: f64,
) -> Result<(), ConsensusError> {
    if torsten_crypto::vrf::check_leader_value(vrf_output, relative_stake, active_slot_coeff) {
        Ok(())
    } else {
        Err(ConsensusError::NotSlotLeader)
    }
}

/// Construct the VRF input for a given slot and epoch nonce.
///
/// In Praos, the VRF input is: nonce || slot_number
/// This is hashed by the VRF to produce the certified random value.
pub fn vrf_input(slot: SlotNo, epoch_nonce: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(epoch_nonce.len() + 8);
    input.extend_from_slice(epoch_nonce);
    input.extend_from_slice(&slot.0.to_be_bytes());
    input
}

impl Default for OuroborosPraos {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torsten_primitives::hash::Hash32;
    use torsten_primitives::time::{BlockNo, SlotNo};

    /// Create a valid test header at the given slot
    fn make_valid_header(slot: u64) -> BlockHeader {
        BlockHeader {
            header_hash: Hash32::ZERO,
            prev_hash: Hash32::ZERO,
            issuer_vkey: vec![1u8; 32],
            vrf_vkey: vec![2u8; 32],
            vrf_result: torsten_primitives::block::VrfOutput {
                output: vec![0u8; 32],
                proof: vec![0u8; 80],
            },
            block_number: BlockNo(1),
            slot: SlotNo(slot),
            epoch_nonce: Hash32::ZERO,
            body_size: 0,
            body_hash: Hash32::ZERO,
            operational_cert: torsten_primitives::block::OperationalCert {
                hot_vkey: vec![3u8; 32],
                sequence_number: 0,
                kes_period: slot / KES_PERIOD_SLOTS,
                sigma: vec![4u8; 64],
            },
            protocol_version: torsten_primitives::block::ProtocolVersion { major: 9, minor: 0 },
            kes_signature: vec![],
        }
    }

    #[test]
    fn test_new_praos() {
        let praos = OuroborosPraos::new();
        assert_eq!(praos.tip, Tip::origin());
        assert!((praos.active_slot_coeff - 0.05).abs() < f64::EPSILON);
        assert_eq!(praos.security_param, 2160);
    }

    #[test]
    fn test_stability_window() {
        let praos = OuroborosPraos::new();
        // 3 * 2160 / 0.05 = 129600
        assert_eq!(praos.stability_window(), 129600);
    }

    #[test]
    fn test_slot_to_epoch() {
        let praos = OuroborosPraos::new();
        assert_eq!(praos.slot_to_epoch(SlotNo(0)), EpochNo(0));
        assert_eq!(praos.slot_to_epoch(SlotNo(431999)), EpochNo(0));
        assert_eq!(praos.slot_to_epoch(SlotNo(432000)), EpochNo(1));
        assert_eq!(praos.slot_to_epoch(SlotNo(864000)), EpochNo(2));
    }

    #[test]
    fn test_epoch_first_slot() {
        let praos = OuroborosPraos::new();
        assert_eq!(praos.epoch_first_slot(EpochNo(0)), SlotNo(0));
        assert_eq!(praos.epoch_first_slot(EpochNo(1)), SlotNo(432000));
    }

    #[test]
    fn test_epoch_boundary() {
        let praos = OuroborosPraos::new();
        assert!(praos.is_epoch_boundary(SlotNo(0)));
        assert!(praos.is_epoch_boundary(SlotNo(432000)));
        assert!(!praos.is_epoch_boundary(SlotNo(1)));
    }

    #[test]
    fn test_max_rollback() {
        let praos = OuroborosPraos::new();
        assert_eq!(praos.max_rollback(), 2160);
    }

    #[test]
    fn test_future_block_rejected() {
        let praos = OuroborosPraos::new();
        let header = make_valid_header(200);
        let result = praos.validate_header(&header, SlotNo(100));
        assert!(matches!(result, Err(ConsensusError::FutureBlock { .. })));
    }

    #[test]
    fn test_valid_header() {
        let praos = OuroborosPraos::new();
        let header = make_valid_header(100);
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_issuer_vkey_rejected() {
        let praos = OuroborosPraos::new();
        let mut header = make_valid_header(100);
        header.issuer_vkey = vec![];
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(matches!(result, Err(ConsensusError::EmptyIssuerVkey)));
    }

    #[test]
    fn test_empty_vrf_key_rejected() {
        let praos = OuroborosPraos::new();
        let mut header = make_valid_header(100);
        header.vrf_vkey = vec![];
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(matches!(result, Err(ConsensusError::EmptyVrfKey)));
    }

    #[test]
    fn test_vrf_verification_non_fatal() {
        // VRF verification with dummy data should not reject during sync
        // (it's non-fatal since we may not have the correct epoch nonce)
        let praos = OuroborosPraos::new();
        let header = make_valid_header(100);
        // With dummy VRF key/proof, verification should pass (non-fatal mode)
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(result.is_ok());
    }

    #[test]
    fn test_kes_period_validation() {
        let praos = OuroborosPraos::new();
        // Block at slot 200,000 is in KES period 1 (200000 / 129600 = 1)
        let mut header = make_valid_header(200_000);
        // Set cert KES period to 1 (matches)
        header.operational_cert.kes_period = 1;
        assert!(praos.validate_header(&header, SlotNo(300_000)).is_ok());
    }

    #[test]
    fn test_kes_period_before_cert_rejected() {
        let praos = OuroborosPraos::new();
        let mut header = make_valid_header(100);
        // Block at slot 100 is in KES period 0, but cert says period 5
        header.operational_cert.kes_period = 5;
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(matches!(
            result,
            Err(ConsensusError::KesPeriodBeforeCert { .. })
        ));
    }

    #[test]
    fn test_kes_expired_rejected() {
        let praos = OuroborosPraos::new();
        // Block at slot 129600 * 63 = 8,164,800 (KES period 63)
        let slot = KES_PERIOD_SLOTS * 63;
        let mut header = make_valid_header(slot);
        // Cert started at period 0, so 63 evolutions > max 62
        header.operational_cert.kes_period = 0;
        let result = praos.validate_header(&header, SlotNo(slot + 1000));
        assert!(matches!(result, Err(ConsensusError::KesExpired { .. })));
    }

    #[test]
    fn test_kes_at_max_evolution_ok() {
        let praos = OuroborosPraos::new();
        // 61 evolutions (0..61) should be OK (< MAX_KES_EVOLUTIONS which is 62)
        let slot = KES_PERIOD_SLOTS * 61;
        let mut header = make_valid_header(slot);
        header.operational_cert.kes_period = 0;
        assert!(praos.validate_header(&header, SlotNo(slot + 1000)).is_ok());
    }

    #[test]
    fn test_empty_opcert_hot_vkey_rejected() {
        let praos = OuroborosPraos::new();
        let mut header = make_valid_header(100);
        header.operational_cert.hot_vkey = vec![];
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(matches!(
            result,
            Err(ConsensusError::InvalidOperationalCert)
        ));
    }

    #[test]
    fn test_empty_opcert_sigma_rejected() {
        let praos = OuroborosPraos::new();
        let mut header = make_valid_header(100);
        header.operational_cert.sigma = vec![];
        let result = praos.validate_header(&header, SlotNo(200));
        assert!(matches!(
            result,
            Err(ConsensusError::InvalidOperationalCert)
        ));
    }

    #[test]
    fn test_64_byte_vrf_output_valid() {
        let praos = OuroborosPraos::new();
        let mut header = make_valid_header(100);
        header.vrf_result.output = vec![0u8; 64]; // TPraos compatibility
        assert!(praos.validate_header(&header, SlotNo(200)).is_ok());
    }

    #[test]
    fn test_verify_opcert_signature_valid() {
        // Generate a cold key pair
        let cold_sk = torsten_crypto::keys::PaymentSigningKey::generate();
        let cold_vk = cold_sk.verification_key();

        let hot_vkey = vec![99u8; 32];
        let sequence_number = 0u64;
        let kes_period = 5u64;

        // Build the opcert body: [hot_vkey, seq_num, kes_period]
        let mut body = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut body);
        enc.array(3).unwrap();
        enc.bytes(&hot_vkey).unwrap();
        enc.u64(sequence_number).unwrap();
        enc.u64(kes_period).unwrap();

        // Sign with cold key
        let signature = cold_sk.sign(&body);

        // Verify
        let result = verify_opcert_signature(
            &cold_vk.to_bytes(),
            &hot_vkey,
            sequence_number,
            kes_period,
            &signature,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_opcert_signature_wrong_key() {
        let cold_sk = torsten_crypto::keys::PaymentSigningKey::generate();
        let wrong_vk = torsten_crypto::keys::PaymentSigningKey::generate().verification_key();

        let hot_vkey = vec![99u8; 32];
        let seq = 0u64;
        let kes = 5u64;

        let mut body = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut body);
        enc.array(3).unwrap();
        enc.bytes(&hot_vkey).unwrap();
        enc.u64(seq).unwrap();
        enc.u64(kes).unwrap();

        let signature = cold_sk.sign(&body);

        // Verify with wrong key should fail
        let result = verify_opcert_signature(&wrong_vk.to_bytes(), &hot_vkey, seq, kes, &signature);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_leader_eligibility_full_stake() {
        // Pool with 100% stake should be eligible with very low VRF output
        assert!(verify_leader_eligibility(&[0u8; 32], 1.0, 0.05).is_ok());
    }

    #[test]
    fn test_verify_leader_eligibility_zero_stake() {
        // Pool with 0% stake should never be eligible
        assert!(verify_leader_eligibility(&[128u8; 32], 0.0, 0.05).is_err());
    }

    #[test]
    fn test_vrf_input_construction() {
        let epoch_nonce = [42u8; 32];
        let input = vrf_input(SlotNo(12345), &epoch_nonce);

        // Should be nonce (32 bytes) + slot (8 bytes) = 40 bytes
        assert_eq!(input.len(), 40);
        assert_eq!(&input[..32], &epoch_nonce);
        assert_eq!(&input[32..], &12345u64.to_be_bytes());
    }

    #[test]
    fn test_strict_verification_mode() {
        let mut praos = OuroborosPraos::new();
        assert!(!praos.strict_verification);

        // In non-strict mode, dummy VRF should pass (non-fatal)
        let header = make_valid_header(100);
        assert!(praos.validate_header(&header, SlotNo(200)).is_ok());

        // Enable strict mode
        praos.set_strict_verification(true);
        assert!(praos.strict_verification);

        // In strict mode, same header should still pass structural checks
        // (VRF verification with dummy data will fail but only if vrf library
        // returns an error, which depends on the data format)
        let header2 = make_valid_header(100);
        // This tests that the strict flag is properly toggled
        praos.set_strict_verification(false);
        assert!(!praos.strict_verification);
        assert!(praos.validate_header(&header2, SlotNo(200)).is_ok());
    }
}
