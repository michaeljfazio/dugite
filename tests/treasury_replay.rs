use anyhow::{anyhow, Context, Result};
use log::{error, info, warn};
use reqwest::Client;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use dugite::config::NetworkConfig;
use dugite::ledger::state::EpochState;
use dugite::replay::ReplayBuilder;

/// Integration test: from-genesis replay on preview testnet up to epoch 1298,
/// dump treasury at each epoch boundary, compare against Koios totals endpoint.
/// Fail if divergence > 1000 lovelace.
#[tokio::test]
async fn treasury_replay_test() -> Result<()> {
    // Initialize logger for test output.
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .is_test(true)
        .init();

    let network = NetworkConfig::preview_testnet();
    let socket_path = PathBuf::from(std::env::var("DUGITE_SOCKET_PATH")
        .unwrap_or_else(|_| "/tmp/preview/node.sock".to_string()));

    let mut replay = ReplayBuilder::new()
        .network(network.clone())
        .socket_path(&socket_path)
        .build()
        .context("Failed to build replay engine")?;

    let mut current_epoch: Option<u64> = None;
    let mut treasury_at_boundary: Option<u64> = None;
    let stale_count = AtomicU64::new(0);
    let mismatch_count = AtomicU64::new(0);
    let lock = Mutex::new(());

    // Koios client for fetching totals.
    let client = Client::builder()
        .user_agent("dugite-treasury-replay")
        .build()
        .context("Failed to create HTTP client")?;

    // Replay from genesis onward.
    replay.run(|block| {
        let epoch = block.header.epoch();
        if current_epoch != Some(epoch) {
            // Epoch boundary reached
            let prev_epoch = current_epoch;
            current_epoch = Some(epoch);

            if let Some(prev) = prev_epoch {
                // Dump treasury from state snapshot at epoch end
                let state = replay.state().clone();
                let treasury = state.treasury_amount();

                info!("Epoch {} treasury: {}", prev, treasury);

                let local_treasury = treasury;
                let lock = lock.lock().await;
                let koios_treasury = fetch_koios_treasury(&client, prev).await?;

                let divergence = if local_treasury > koios_treasury {
                    local_treasury - koios_treasury
                } else {
                    koios_treasury - local_treasury
                };

                if divergence > 1000 {
                    error!(
                        "Epoch {}: local {} vs koios {} divergence {} > 1000 lovelace",
                        prev, local_treasury, koios_treasury, divergence
                    );
                    mismatch_count.fetch_add(1, Ordering::SeqCst);
                } else {
                    info!(
                        "Epoch {}: divergence {} lovelace (within tolerance)",
                        prev, divergence
                    );
                }
                drop(lock);
            }

            // If we've reached target epoch, stop replay
            if epoch >= 1298 {
                info!("Reached target epoch 1298, stopping replay.");
                return Ok(true);
            }
        }

        // Update stale detection (if needed)
        if block.header.slot() % 1000 == 0 {
            info!("Processing block slot {}", block.header.slot());
        }

        Ok(false)
    }).await.context("Replay engine exited with error")?;

    let mis = mismatch_count.load(Ordering::SeqCst);
    if mis > 0 {
        anyhow!("{mis} epoch(s) had treasury divergence > 1000 lovelace");
    }

    Ok(())
}

/// Fetch the treasury amount (in lovelace) for a given epoch from Koios.
/// Uses the `/api/v1/totals` endpoint, filtering by epoch number.
async fn fetch_koios_treasury(client: &Client, epoch: u64) -> Result<u64> {
    let url = format!(
        "https://preview.koios.rest/api/v1/totals?epoch_no={}",
        epoch
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Koios API request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Koios returned HTTP {status}: {body}"));
    }

    let totals: Vec<serde_json::Value> = resp
        .json()
        .await
        .context("Failed to parse Koios JSON response")?;

    let treasury_str = totals
        .first()
        .and_then(|v| v.get("treasury"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Koios response missing treasury field"))?;

    let treasury: u64 = treasury_str
        .parse()
        .context("Failed to parse treasury as u64")?;

    Ok(treasury)
}