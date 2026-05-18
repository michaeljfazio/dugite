/// Performs a full serialization roundtrip of a governance state (including
/// `committee_expiration`) to detect data loss or corruption in encode/decode
/// paths.  Intended for use in `governance_test.rs` and related unit tests.
///
/// # Errors
///
/// Returns an error if the encoder fails to serialize `state` or if the
/// resulting buffer cannot be decoded back to the same type.
pub fn roundtrip_test(
    state: &GovernanceState,
    encoder: &SnapshotEncoder,
) -> anyhow::Result<()> {
    let mut buffer = Vec::new();

    encoder
        .encode(state, &mut buffer)
        .with_context(|| "serialization of governance state failed")?;

    log::info!(
        "snapshot roundtrip: serialized {} bytes for governance state",
        buffer.len()
    );

    let decoded: GovernanceState = encoder
        .decode(&buffer[..])
        .with_context(|| "deserialization of governance state failed")?;

    // Deep comparison – the encoded/decoded type must implement `PartialEq`.
    assert_eq!(
        *state, decoded,
        "governance state changed after serialization roundtrip"
    );

    log::info!("snapshot roundtrip: governance state preserved intact");
    Ok(())
}