//! Dugite xtask dispatcher.
//!
//! Usage (via cargo alias):
//!   cargo xtask download-upstream-fixtures [--area <name>]
//!
//! This binary is the entry point when the cargo alias `xtask` is used.
//! It dispatches to the appropriate sub-binary after parsing the subcommand name.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Dugite workspace task runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download the upstream conformance corpus fixtures.
    DownloadUpstreamFixtures {
        /// Download only a single area (e.g. `plutus`, `cardano-ledger`).
        #[arg(long)]
        area: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::DownloadUpstreamFixtures { area } => {
            download_upstream_fixtures::run(area)?;
        }
    }
    Ok(())
}

// ── Download logic (shared with the standalone binary) ───────────────────────

mod download_upstream_fixtures {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use serde::Deserialize;
    use sha2::{Digest, Sha256};

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

    pub fn run(single_area: Option<String>) -> Result<()> {
        let workspace_root = workspace_root();
        let manifest_path = workspace_root.join("tests/conformance/upstream/manifest.toml");
        let fixtures_root = std::env::var("DUGITE_UPSTREAM_FIXTURES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| workspace_root.join("tests/conformance/upstream/fixtures"));

        let manifest_text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("Cannot read {}", manifest_path.display()))?;
        let manifest: Manifest = toml::from_str(&manifest_text)
            .with_context(|| format!("Cannot parse {}", manifest_path.display()))?;

        let areas: Vec<(&String, &AreaEntry)> = if let Some(ref name) = single_area {
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

        let sentinel = fixtures_root.join("MANIFEST_SHA256");
        let hash = hex_sha256(manifest_text.as_bytes());
        fs::create_dir_all(&fixtures_root).context("Cannot create fixtures directory")?;
        fs::write(&sentinel, &hash)
            .with_context(|| format!("Cannot write sentinel {}", sentinel.display()))?;
        eprintln!("==> Wrote sentinel: {} → {hash}", sentinel.display());

        Ok(())
    }

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
        if target_dir.exists() {
            fs::remove_dir_all(target_dir)
                .with_context(|| format!("Cannot wipe {}", target_dir.display()))?;
        }
        fs::create_dir_all(target_dir)
            .with_context(|| format!("Cannot create {}", target_dir.display()))?;

        // Stream to a temp file (not an in-memory Vec) with HTTP-Range resume —
        // some assets are hundreds of MB (ledger-rules ~875MB) and a whole-body
        // `.bytes()` read aborts on any mid-stream blip with "error decoding
        // response body". 5 attempts, resuming from the last byte written.
        let tmp = download_to_temp_with_resume(url, token, 5)?;
        let file = fs::File::open(&tmp).with_context(|| format!("open {}", tmp.display()))?;
        let gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
        let mut archive = tar::Archive::new(gz);
        archive.set_overwrite(true);
        let unpack = archive
            .unpack(target_dir)
            .with_context(|| format!("Cannot extract to {}", target_dir.display()));
        let _ = fs::remove_file(&tmp);
        unpack?;

        let count = count_files(target_dir);
        eprintln!("    extracted {count} files");
        Ok(())
    }

    /// Download `url` to a temp file, retrying with an HTTP-Range resume from the
    /// last durably-written byte on transient stream errors. Returns the temp
    /// file path. Falls back to a fresh restart if the server ignores Range
    /// (responds 200 instead of 206).
    fn download_to_temp_with_resume(
        url: &str,
        token: Option<&str>,
        max_retries: u32,
    ) -> Result<PathBuf> {
        use std::io::{Read, Write};

        let client = reqwest::blocking::Client::builder()
            .user_agent("dugite-xtask/1.0")
            .build()
            .context("Cannot build HTTP client")?;

        let asset = url.rsplit('/').next().unwrap_or("fixture");
        let tmp = std::env::temp_dir().join(format!("dugite-xtask-{asset}.download.tmp"));
        let _ = fs::remove_file(&tmp);

        let mut written: u64 = 0;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = Duration::from_secs(1u64 << (attempt - 1).min(4));
                eprintln!(
                    "    retry {attempt}/{max_retries} (resume from {written} bytes) after {}s…",
                    delay.as_secs()
                );
                thread::sleep(delay);
            }
            let mut req = client.get(url);
            if let Some(tok) = token {
                req = req.header("Authorization", format!("Bearer {tok}"));
            }
            if written > 0 {
                req = req.header(reqwest::header::RANGE, format!("bytes={written}-"));
            }
            let resp = match req.send().and_then(|r| r.error_for_status()) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("    attempt {attempt} request failed: {e}");
                    last_err = Some(e.into());
                    continue;
                }
            };
            // 206 = the server honoured Range → append; otherwise (200) it sent
            // the whole body → restart the file from scratch.
            let resuming = written > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            let mut file = if resuming {
                match fs::OpenOptions::new().append(true).open(&tmp) {
                    Ok(f) => f,
                    Err(e) => {
                        last_err = Some(e.into());
                        written = 0;
                        continue;
                    }
                }
            } else {
                written = 0;
                fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?
            };

            let mut buf = vec![0u8; 1 << 20];
            let stream_result: Result<()> = (|| {
                let mut r = resp;
                loop {
                    let n = r.read(&mut buf).context("error reading response body")?;
                    if n == 0 {
                        break;
                    }
                    file.write_all(&buf[..n]).context("write to temp file")?;
                    written += n as u64;
                }
                file.flush().context("flush temp file")?;
                Ok(())
            })();
            match stream_result {
                Ok(()) => return Ok(tmp),
                Err(e) => {
                    eprintln!("    attempt {attempt} stream failed at {written} bytes: {e}");
                    last_err = Some(e);
                }
            }
        }
        let _ = fs::remove_file(&tmp);
        bail!(
            "Download failed after {max_retries} retries: {}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".into())
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
}
