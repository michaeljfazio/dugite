//! Download the dugite upstream conformance corpus from a pinned GitHub release.
//!
//! Reads `tests/conformance/upstream/manifest.toml` to discover which release
//! tag to pull from and which per-area assets to download. Extracts each asset
//! into `tests/conformance/upstream/fixtures/<area>/`. Writes a SHA-256
//! sentinel at `fixtures/MANIFEST_SHA256` once all areas succeed.
//!
//! Usage:
//!   cargo xtask download-upstream-fixtures              # all areas
//!   cargo xtask download-upstream-fixtures --area plutus  # single area

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(
    name = "download-upstream-fixtures",
    about = "Download dugite upstream conformance corpus"
)]
struct Cli {
    /// Download only a single area (e.g. `plutus`, `cardano-ledger`).
    /// Omit to download all areas.
    #[arg(long)]
    area: Option<String>,
}

// ── Manifest types ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Manifest {
    release: ReleasePin,
    area: std::collections::BTreeMap<String, AreaEntry>,
}

#[derive(Deserialize)]
struct ReleasePin {
    repo: String,
    tag: String,
}

#[derive(Deserialize)]
struct AreaEntry {
    asset: String,
    target: String,
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let workspace_root = workspace_root();
    let manifest_path = workspace_root.join("tests/conformance/upstream/manifest.toml");
    let fixtures_root = std::env::var("DUGITE_UPSTREAM_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root.join("tests/conformance/upstream/fixtures"));

    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Cannot read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest_text)
        .with_context(|| format!("Cannot parse {}", manifest_path.display()))?;

    let areas: Vec<(&String, &AreaEntry)> = if let Some(ref name) = cli.area {
        let entry = manifest
            .area
            .get(name)
            .with_context(|| format!("Area '{name}' not found in manifest.toml"))?;
        vec![(name, entry)]
    } else {
        manifest.area.iter().collect()
    };

    let github_token = std::env::var("GITHUB_TOKEN").ok();

    for (name, entry) in &areas {
        let url = format!(
            "https://github.com/{}/releases/download/{}/{}",
            manifest.release.repo, manifest.release.tag, entry.asset
        );
        let target_dir = fixtures_root.join(&entry.target);

        eprintln!("==> Downloading area '{name}': {url}");
        download_and_extract(&url, &target_dir, github_token.as_deref())
            .with_context(|| format!("Failed to fetch area '{name}'"))?;
        eprintln!("    {name}: extracted to {}", target_dir.display());
    }

    // Write the MANIFEST_SHA256 sentinel once all areas succeed.
    let sentinel = fixtures_root.join("MANIFEST_SHA256");
    let hash = hex_sha256(manifest_text.as_bytes());
    fs::create_dir_all(&fixtures_root).context("Cannot create fixtures directory")?;
    fs::write(&sentinel, &hash)
        .with_context(|| format!("Cannot write sentinel {}", sentinel.display()))?;
    eprintln!("==> Wrote sentinel: {} → {hash}", sentinel.display());

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cargo = dir.join("Cargo.toml");
        if cargo.exists()
            && fs::read_to_string(&cargo)
                .map(|s| s.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        if !dir.pop() {
            panic!(
                "workspace root not found from {}",
                env!("CARGO_MANIFEST_DIR")
            );
        }
    }
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn download_and_extract(url: &str, target_dir: &Path, token: Option<&str>) -> Result<()> {
    // Clear target dir to ensure no stale files remain.
    if target_dir.exists() {
        fs::remove_dir_all(target_dir)
            .with_context(|| format!("Cannot wipe {}", target_dir.display()))?;
    }
    fs::create_dir_all(target_dir)
        .with_context(|| format!("Cannot create {}", target_dir.display()))?;

    let bytes = download_with_retry(url, token, 3)?;

    let cursor = std::io::Cursor::new(bytes);
    let gz = flate2::read::GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);
    archive.set_overwrite(true);
    archive
        .unpack(target_dir)
        .with_context(|| format!("Cannot extract to {}", target_dir.display()))?;

    let count = count_files(target_dir);
    eprintln!("    extracted {count} files");
    Ok(())
}

fn download_with_retry(url: &str, token: Option<&str>, max_retries: u32) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("dugite-xtask/1.0")
        .build()
        .context("Cannot build HTTP client")?;

    let mut last_err = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let delay = Duration::from_secs(1u64 << (attempt - 1));
            eprintln!(
                "    retry {attempt}/{max_retries} after {}s…",
                delay.as_secs()
            );
            thread::sleep(delay);
        }
        let mut req = client.get(url);
        if let Some(tok) = token {
            req = req.header("Authorization", format!("Bearer {tok}"));
        }
        match req
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
        {
            Ok(bytes) => return Ok(bytes.to_vec()),
            Err(e) => {
                eprintln!("    attempt {attempt} failed: {e}");
                last_err = Some(e);
            }
        }
    }
    bail!(
        "Download failed after {max_retries} retries: {}",
        last_err.unwrap()
    );
}

fn count_files(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_file() {
                count += 1;
            } else if path.is_dir() {
                count += count_files(&path);
            }
        }
    }
    count
}
