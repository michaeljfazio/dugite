//! One-shot offline capture tool for Conway ratification fixtures.
//!
//! Queries the public preview Koios endpoint for every input the Haskell
//! `ratifyTransition` rule reads, and writes a JSON fixture under
//! `fixtures/conway-ratification/` consumed by
//! `crates/dugite-ledger/tests/conway_ratification.rs`.
//!
//! The capture is deliberately heavyweight — it walks `drep_list`, fans
//! out `drep_voting_power_history` per registered DRep, sums always-abstain
//! / always-no-confidence delegators from `account_list`, captures every
//! voting pool's reward-account DRep delegation, and transforms the live
//! committee into typed-Hash32 form.  The first run for a given proposal
//! takes minutes; the result is committed and re-used offline at test time.
//!
//! Not a CI dependency — never runs in the test suite.

use bech32::primitives::decode::CheckedHrpstring;
use bech32::Bech32;
use clap::Parser;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::Semaphore;

const KOIOS_PREVIEW: &str = "https://preview.koios.rest/api/v1";

/// Capture a Conway ratification fixture from Koios preview.
#[derive(Parser, Debug)]
#[command(name = "capture-ratification-fixture", version, about)]
struct Args {
    /// Network (only "preview" is supported for this slice).
    #[arg(long, default_value = "preview")]
    network: String,

    /// Governance action id in the form `<tx_hex>#<cert_index>`.
    #[arg(long)]
    proposal_id: String,

    /// Output path (parent directory must exist).
    #[arg(long)]
    output: PathBuf,

    /// Concurrency cap for the per-DRep voting-power fan-out.  Conservative
    /// default against Koios free tier rate limits — the free endpoint
    /// enforces a short-window burst budget that 5+ concurrent workers
    /// reliably exceed; 2 keeps us well below.
    #[arg(long, default_value_t = 2)]
    drep_concurrency: usize,

    /// Inter-request delay (milliseconds) per DRep fan-out worker.  Combined
    /// with `--drep-concurrency`, the effective Koios request rate is
    /// `concurrency * 1000 / inter_request_ms`.  Default 250ms × 2 workers
    /// ≈ 8 req/s, comfortably under the free-tier ceiling.
    #[arg(long, default_value_t = 250)]
    inter_request_ms: u64,

    /// Skip the per-DRep snapshot (writes empty `drep_power`).  Useful for
    /// bootstrap-era fixtures where DRep thresholds auto-pass and the
    /// snapshot is unread.
    #[arg(long, default_value_t = false)]
    skip_drep_snapshot: bool,

    /// Use aggregate-mode DRep capture: a single `proposal_voting_summary`
    /// call returns the aggregate Yes / No / Abstain DRep stake for this
    /// proposal.  The fixture's `drep_aggregates` field is populated
    /// instead of `drep_power`, and the loader synthesizes an equivalent
    /// snapshot (one Yes-cred, one No-cred, one Abstain-cred + a
    /// no-vote-cred for non-voting registered DReps) that reproduces the
    /// same `drep_yes / drep_total` ratio.  One Koios request instead of
    /// thousands — required for post-bootstrap (PV ≥ 10) captures that
    /// would otherwise exhaust Koios's 5000 req/day free-tier cap.
    ///
    /// Mutually exclusive with `--skip-drep-snapshot` (skip wins if both
    /// set, since aggregate mode still needs one network call).
    #[arg(long, default_value_t = false)]
    aggregate_drep: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();
    if args.network != "preview" {
        eprintln!("only --network=preview is supported (exit 2 = bad args)");
        std::process::exit(2);
    }

    let client = reqwest::Client::builder()
        .user_agent("dugite-capture-ratification-fixture/1.0")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");

    let (tx_hex, idx_str) = match args.proposal_id.split_once('#') {
        Some(parts) => parts,
        None => panic!("malformed --proposal-id: {}", args.proposal_id),
    };
    let idx: u32 = idx_str.parse().expect("--proposal-id index not u32");

    // 1. proposal_list — find this specific proposal.
    let proposal_list = koios_get(
        &client,
        &format!("/proposal_list?proposal_tx_hash=eq.{tx_hex}&proposal_index=eq.{idx}"),
    )
    .await;
    let proposal = proposal_list
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| panic!("proposal {} not found on Koios", args.proposal_id));

    let proposal_id_bech32 = proposal
        .get("proposal_id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("proposal_list row missing proposal_id (bech32)"))
        .to_string();

    // 2. proposal_votes — individual vote records (RPC).
    let votes_blob = koios_get(
        &client,
        &format!("/proposal_votes?_proposal_id={proposal_id_bech32}"),
    )
    .await;

    // 3. Determine ratification epoch.
    let ratification_epoch: u64 = ["ratified_epoch", "enacted_epoch", "dropped_epoch"]
        .iter()
        .find_map(|k| proposal.get(k).and_then(|v| v.as_u64()))
        .unwrap_or_else(|| panic!("no ratification/dropped epoch in proposal row"));
    let was_ratified = proposal
        .get("ratified_epoch")
        .and_then(|v| v.as_u64())
        .is_some()
        || proposal
            .get("enacted_epoch")
            .and_then(|v| v.as_u64())
            .is_some();
    let snapshot_epoch = ratification_epoch.saturating_sub(1);

    // 4. epoch_params @ ratification_epoch — pparams subset.
    let epoch_params_arr = koios_get(
        &client,
        &format!("/epoch_params?_epoch_no={ratification_epoch}"),
    )
    .await;
    let epoch_params = epoch_params_arr
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| panic!("epoch_params returned empty for epoch {ratification_epoch}"));

    // 5. pool_voting_power_history @ snapshot_epoch — full set.
    let pool_power_full = koios_get(
        &client,
        &format!("/pool_voting_power_history?_epoch_no={snapshot_epoch}&limit=1000"),
    )
    .await;

    // 6. committee_info — current committee.
    let committee = koios_get(&client, "/committee_info").await;

    // 7. Capture spo_stake + pool_reward_accounts.
    //    For the fixture to be tractable we capture the voting pools
    //    explicitly (their stakes drive the SPO ratio); other pools are
    //    irrelevant to the ratio because non-voters during bootstrap are
    //    Abstain and post-bootstrap default to No (never enter spo_yes).
    let voting_pool_hashes: Vec<String> = votes_blob
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    if v.get("voter_role").and_then(|x| x.as_str()) == Some("SPO") {
                        v.get("voter_hex")
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut spo_stake: BTreeMap<String, u64> = BTreeMap::new();
    if let Some(arr) = pool_power_full.as_array() {
        for row in arr {
            let pool_bech32 = match row.get("pool_id_bech32").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let amount = match row.get("amount").and_then(|v| v.as_str()) {
                Some(s) => s.parse::<u64>().unwrap_or(0),
                None => continue,
            };
            let hex = pool_bech32_to_hex(pool_bech32);
            // Include all pools so post-bootstrap default-vote logic sees a
            // realistic distribution; bootstrap fixtures pay a tiny size
            // cost in exchange for fidelity.
            if voting_pool_hashes.contains(&hex) {
                spo_stake.insert(hex, amount);
            }
        }
    }

    // pool_reward_accounts — for every voting pool we capture its
    // reward_account so default_spo_vote_from has the data it needs (post-
    // bootstrap fixtures will rely on this).
    let mut pool_reward_accounts: BTreeMap<String, String> = BTreeMap::new();
    for hex in &voting_pool_hashes {
        let bech32 = pool_hex_to_bech32(hex);
        let info_arr = koios_get(&client, &format!("/pool_info?_pool_bech32_ids={bech32}")).await;
        let info = match info_arr.as_array().and_then(|a| a.first()) {
            Some(o) => o.clone(),
            None => continue,
        };
        // `reward_addr` from Koios is a stake address (bech32 stake1...).
        let reward_bech32 = match info
            .get("reward_addr")
            .and_then(|v| v.as_str())
            .or_else(|| info.get("reward_account").and_then(|v| v.as_str()))
        {
            Some(s) => s.to_string(),
            None => continue,
        };
        let reward_hex29 = stake_addr_bech32_to_hex(&reward_bech32);
        pool_reward_accounts.insert(hex.clone(), reward_hex29);
    }

    // 8. DRep snapshot — three modes:
    //   * --skip-drep-snapshot: empty drep_power (bootstrap fixtures only)
    //   * --aggregate-drep:     1 call to proposal_voting_summary, populates
    //                            drep_aggregates instead of per-DRep map
    //   * default:              ~8800 per-DRep drep_voting_power_history calls
    let mut drep_power: BTreeMap<String, u64> = BTreeMap::new();
    let mut drep_aggregates_blob: Option<Value> = None;
    if args.aggregate_drep && !args.skip_drep_snapshot {
        eprintln!("DRep snapshot: aggregate mode (1 Koios request)");
        let summary_resp = koios_get(
            &client,
            &format!("/proposal_voting_summary?_proposal_id={proposal_id_bech32}"),
        )
        .await;
        let row = summary_resp
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or_else(|| panic!("proposal_voting_summary returned empty"));
        // Koios fields (lovelace, returned as numeric strings):
        //   drep_yes_votes_assigned_power, drep_no_votes_assigned_power,
        //   drep_abstain_votes_assigned_power,
        //   drep_active_no_vote_power,
        //   drep_always_no_confidence_vote_power,
        //   drep_always_abstain_vote_power
        let read_amount = |key: &str| -> u64 {
            row.get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or_else(|| row.get(key).and_then(|v| v.as_u64()).unwrap_or(0))
        };
        drep_aggregates_blob = Some(serde_json::json!({
            "yes_stake":                read_amount("drep_yes_votes_assigned_power"),
            "no_stake":                 read_amount("drep_no_votes_assigned_power"),
            "abstain_stake":            read_amount("drep_abstain_votes_assigned_power"),
            "no_vote_stake":            read_amount("drep_active_no_vote_power"),
            "always_no_confidence_stake": read_amount("drep_always_no_confidence_vote_power"),
            "always_abstain_stake":     read_amount("drep_always_abstain_vote_power"),
        }));
        eprintln!(
            "DRep snapshot: aggregates captured (yes={}, no={}, abstain={})",
            read_amount("drep_yes_votes_assigned_power"),
            read_amount("drep_no_votes_assigned_power"),
            read_amount("drep_abstain_votes_assigned_power")
        );
    } else if !args.skip_drep_snapshot {
        let drep_list = koios_paged(&client, "/drep_list").await;
        let registered: Vec<(String, String, bool)> = drep_list
            .into_iter()
            .filter_map(|row| {
                let registered = row
                    .get("registered")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !registered {
                    return None;
                }
                let drep_id = row.get("drep_id").and_then(|v| v.as_str())?.to_string();
                let hex = row.get("hex").and_then(|v| v.as_str())?.to_string();
                let has_script = row
                    .get("has_script")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some((drep_id, hex, has_script))
            })
            .collect();
        eprintln!(
            "DRep snapshot: querying voting power for {} registered DReps at epoch {snapshot_epoch} (concurrency={})",
            registered.len(),
            args.drep_concurrency
        );

        let sem = std::sync::Arc::new(Semaphore::new(args.drep_concurrency));
        let inter_request = args.inter_request_ms;
        let total = registered.len();
        let progress_interval = (total / 20).max(50); // log every ~5%
        let mut handles = Vec::new();
        for (idx, (drep_id, hex, has_script)) in registered.into_iter().enumerate() {
            let permit = sem.clone().acquire_owned().await.unwrap();
            let client = client.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                let url = format!(
                    "/drep_voting_power_history?_drep_id={drep_id}&_epoch_no={snapshot_epoch}"
                );
                let resp = koios_get(&client, &url).await;
                // Per-task throttle to keep the aggregate request rate well
                // under Koios's burst budget — applied AFTER the request so
                // the inter-arrival gap for the same worker is honoured.
                if inter_request > 0 {
                    tokio::time::sleep(Duration::from_millis(inter_request)).await;
                }
                let amount: u64 = resp
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|row| row.get("amount").and_then(|v| v.as_str()))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if amount == 0 {
                    return (idx, None);
                }
                (
                    idx,
                    Some((typed_hash32_from_hex28(&hex, has_script), amount)),
                )
            }));
        }
        for h in handles {
            let (idx, payload) = h.await.unwrap();
            if (idx + 1).is_multiple_of(progress_interval) || idx + 1 == total {
                eprintln!(
                    "DRep snapshot: {}/{} ({:.0}%)",
                    idx + 1,
                    total,
                    100.0 * (idx + 1) as f64 / total as f64
                );
            }
            if let Some((typed_hex, amount)) = payload {
                drep_power.insert(typed_hex, amount);
            }
        }
    }

    // 9. Pseudo-DRep aggregation (per-DRep mode only).
    //    Koios doesn't expose drep_always_abstain / drep_always_no_confidence
    //    via drep_voting_power_history directly — capture by paging
    //    `/account_list?_drep_id=...` and summing each account's total.
    //
    //    In aggregate mode, the always-* totals already come back from the
    //    single proposal_voting_summary call, so we skip the extra paged
    //    queries and leave the top-level scalar fields at zero (the loader
    //    reads from `drep_aggregates` instead).
    let in_aggregate_mode = drep_aggregates_blob.is_some();
    let drep_no_confidence = if args.skip_drep_snapshot || in_aggregate_mode {
        0
    } else {
        sum_pseudo_drep(&client, "drep_always_no_confidence")
            .await
            .unwrap_or(0)
    };
    let drep_abstain = if args.skip_drep_snapshot || in_aggregate_mode {
        0
    } else {
        sum_pseudo_drep(&client, "drep_always_abstain")
            .await
            .unwrap_or(0)
    };

    // ----------------------------------------------------------------------
    // Transform proposal → canonical FixtureProposal.
    // ----------------------------------------------------------------------
    let proposed_epoch = proposal
        .get("proposed_epoch")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("proposal row missing proposed_epoch"));
    let expiration = proposal
        .get("expiration")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("proposal row missing expiration"));
    let deposit: u64 = proposal
        .get("deposit")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("proposal row missing/non-numeric deposit"));
    let proposal_type_str = proposal
        .get("proposal_type")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("proposal row missing proposal_type"))
        .to_string();
    let enacted_bucket = match proposal_type_str.as_str() {
        "ParameterChange" => "PParamUpdate",
        "HardForkInitiation" => "HardFork",
        "NewCommittee" => "Committee",
        "NewConstitution" => "Constitution",
        other => panic!(
            "proposal_type {other:?} is out of scope for this slice (PParamUpdate / HardFork / Committee / Constitution only)"
        ),
    };

    let fixture_proposal = serde_json::json!({
        "gov_action_id": format!("{tx_hex}#{idx}"),
        "action": proposal.get("proposal_description").cloned().unwrap_or(Value::Null),
        "deposit": deposit,
        "return_addr_hex": "e0000000000000000000000000000000000000000000000000000000000000",
        "expiration": expiration,
        "anchor": null,
    });

    // Transform Koios votes → canonical FixtureVote list.
    let mut canonical_votes: Vec<Value> = Vec::new();
    if let Some(arr) = votes_blob.as_array() {
        for v in arr {
            let role = v.get("voter_role").and_then(|x| x.as_str()).unwrap_or("");
            let has_script = v
                .get("voter_has_script")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let voter_hex = v
                .get("voter_hex")
                .and_then(|x| x.as_str())
                .unwrap_or_else(|| panic!("vote row missing voter_hex: {v}"));
            let vote_str = v
                .get("vote")
                .and_then(|x| x.as_str())
                .unwrap_or_else(|| panic!("vote row missing vote: {v}"));
            let voter_type = match (role, has_script) {
                ("DRep", false) => "DRepKeyHash",
                ("DRep", true) => "DRepScriptHash",
                ("SPO", _) => "StakePoolKeyHash",
                ("ConstitutionalCommittee", false) => "ConstitutionalCommitteeHotKeyHash",
                ("ConstitutionalCommittee", true) => "ConstitutionalCommitteeHotScriptHash",
                _ => panic!("unknown voter_role/has_script: {role}/{has_script}"),
            };
            canonical_votes.push(serde_json::json!({
                "voter_type": voter_type,
                "voter_id": voter_hex,
                "vote": vote_str,
            }));
        }
    }

    // Committee — typed Hash32 (cold + hot) + real threshold.
    let committee_obj = transform_committee(&committee);

    // pparams_subset — projection of epoch_params onto the fields RATIFY reads.
    let pparams_subset = transform_pparams(&epoch_params);

    // parent_enacted — fill the slot for this proposal's purpose only.
    // Other slots stay null; for single-proposal fixtures the rule only
    // consults the matching purpose.
    let prev = proposal.get("prev_action_index").cloned();
    let prev_tx = proposal.get("prev_action_tx_hash").cloned();
    let parent_id_str = match (prev_tx, prev) {
        (Some(Value::String(tx)), Some(idx_v)) => idx_v
            .as_u64()
            .map(|i| Value::String(format!("{tx}#{i}")))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let parent_enacted = match enacted_bucket {
        "PParamUpdate" => serde_json::json!({
            "PParamUpdate": parent_id_str,
            "HardFork": null,
            "Committee": null,
            "Constitution": null,
        }),
        "HardFork" => serde_json::json!({
            "PParamUpdate": null,
            "HardFork": parent_id_str,
            "Committee": null,
            "Constitution": null,
        }),
        "Committee" => serde_json::json!({
            "PParamUpdate": null,
            "HardFork": null,
            "Committee": parent_id_str,
            "Constitution": null,
        }),
        "Constitution" => serde_json::json!({
            "PParamUpdate": null,
            "HardFork": null,
            "Committee": null,
            "Constitution": parent_id_str,
        }),
        _ => unreachable!("validated above"),
    };

    let mut fixture = serde_json::json!({
        "proposal": fixture_proposal,
        "proposed_epoch": proposed_epoch,
        "votes": canonical_votes,
        "drep_power": drep_power,
        "drep_no_confidence": drep_no_confidence,
        "drep_abstain": drep_abstain,
        "spo_stake": spo_stake,
        "pool_reward_accounts": pool_reward_accounts,
        "vote_delegations": serde_json::Value::Object(serde_json::Map::new()),
        "no_confidence": false,
        "committee": committee_obj,
        "pparams_epoch": ratification_epoch,
        "pparams": pparams_subset,
        "expected_outcome": {
            "ratified": was_ratified,
            "enacted_bucket": enacted_bucket,
            "enacted_epoch": ratification_epoch,
            "enacted_id": if was_ratified {
                Value::String(format!("{tx_hex}#{idx}"))
            } else {
                Value::Null
            },
        },
        "parent_enacted": parent_enacted,
        "provenance": {
            "captured_at": chrono_now(),
            "source": KOIOS_PREVIEW,
            "snapshot_epoch": snapshot_epoch,
            "ratification_epoch": ratification_epoch,
            "drep_mode": if in_aggregate_mode {
                "aggregate"
            } else if args.skip_drep_snapshot {
                "skip"
            } else {
                "per-drep"
            },
        }
    });
    // Splice `drep_aggregates` into the top-level object only when
    // aggregate mode produced one — keeping per-DRep captures unchanged.
    if let Some(blob) = drep_aggregates_blob {
        fixture
            .as_object_mut()
            .expect("fixture is an object")
            .insert("drep_aggregates".to_string(), blob);
    }

    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent).expect("create output parent dir");
    }
    let pretty = serde_json::to_string_pretty(&fixture).expect("serialize");
    std::fs::write(&args.output, pretty + "\n").expect("write output");
    eprintln!("wrote {}", args.output.display());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn koios_get(client: &reqwest::Client, path: &str) -> Value {
    koios_get_retry(client, path, 8).await
}

/// Retry with exponential backoff, with extra patience on 429 (Koios burst
/// rate limits).  The free tier enforces both a per-second rate limit and a
/// short-window burst limit; 429 responses commonly arrive in clusters when
/// concurrent fan-out (per-DRep voting power) exceeds the burst budget.
///
/// Backoff strategy: base 500ms exponential, but on 429 we wait the full
/// backoff window — 0.5s, 1s, 2s, 4s, 8s, 16s, 32s, 60s — to give the
/// burst counter time to drain.
async fn koios_get_retry(client: &reqwest::Client, path: &str, retries: u32) -> Value {
    let url = format!("{KOIOS_PREVIEW}{path}");
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let outcome: Result<Result<Value, String>, String> = match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(v) => return v,
                Err(e) => Ok(Err(format!("body not JSON: {e}"))),
            },
            Ok(resp) => {
                let status = resp.status();
                let is_rate_limited = status.as_u16() == 429;
                let body = resp.text().await.unwrap_or_default();
                if is_rate_limited {
                    Err(format!("rate-limited (429): {body}"))
                } else {
                    Ok(Err(format!("status {status}: {body}")))
                }
            }
            Err(e) => Ok(Err(format!("transport error: {e}"))),
        };
        match outcome {
            Err(rate_limit_msg) => {
                if attempt >= retries {
                    panic!("koios {url} rate-limited after {retries} retries: {rate_limit_msg}");
                }
                // Aggressive backoff on 429: 0.5s × 2^(attempt-1), capped at 60s.
                let wait_ms = (500u64 << attempt.min(7)).min(60_000);
                eprintln!(
                    "koios {url} -> 429, backing off {wait_ms}ms (attempt {attempt}/{retries})"
                );
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            Ok(Err(other_err)) => {
                if attempt >= retries {
                    panic!("koios {url} failed after {retries} retries: {other_err}");
                }
                let wait_ms = 500u64 * attempt as u64;
                eprintln!(
                    "koios {url} -> {other_err}, retrying in {wait_ms}ms ({attempt}/{retries})"
                );
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            Ok(Ok(_)) => unreachable!("success returns from match arm"),
        }
    }
}

async fn koios_paged(client: &reqwest::Client, path: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let page_size = 1000usize;
    let mut offset = 0usize;
    loop {
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = format!("{path}{sep}limit={page_size}&offset={offset}");
        let resp = koios_get(client, &url).await;
        let arr = match resp.as_array() {
            Some(a) => a.clone(),
            None => break,
        };
        let len = arr.len();
        out.extend(arr);
        if len < page_size {
            break;
        }
        offset += page_size;
    }
    out
}

async fn sum_pseudo_drep(client: &reqwest::Client, drep_id: &str) -> Option<u64> {
    let path = format!("/drep_delegators?_drep_id={drep_id}");
    let rows = koios_paged(client, &path).await;
    let mut total = 0u64;
    for row in rows {
        let amount: u64 = row
            .get("amount")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        total = total.saturating_add(amount);
    }
    Some(total)
}

fn pool_bech32_to_hex(s: &str) -> String {
    let parsed = CheckedHrpstring::new::<Bech32>(s)
        .unwrap_or_else(|e| panic!("invalid pool bech32 {s}: {e}"));
    let bytes: Vec<u8> = parsed.byte_iter().collect();
    hex::encode(bytes)
}

fn pool_hex_to_bech32(hex_str: &str) -> String {
    let bytes = hex::decode(hex_str).unwrap_or_else(|e| panic!("invalid pool hex {hex_str}: {e}"));
    let hrp = bech32::Hrp::parse("pool").unwrap();
    bech32::encode::<Bech32>(hrp, &bytes).unwrap_or_else(|e| panic!("encode pool bech32: {e}"))
}

fn stake_addr_bech32_to_hex(s: &str) -> String {
    let parsed = CheckedHrpstring::new::<Bech32>(s)
        .unwrap_or_else(|e| panic!("invalid stake bech32 {s}: {e}"));
    let bytes: Vec<u8> = parsed.byte_iter().collect();
    if bytes.len() != 29 {
        panic!(
            "stake address {s} decoded to {} bytes (expected 29)",
            bytes.len()
        );
    }
    hex::encode(bytes)
}

/// Encode a 28-byte hash + type byte (0x00 for key / 0x01 for script)
/// into the 32-byte typed-Hash32 hex form expected by the loader.
fn typed_hash32_from_hex28(hex28: &str, is_script: bool) -> String {
    let bytes = hex::decode(hex28).unwrap_or_else(|e| panic!("typed hash hex {hex28}: {e}"));
    if bytes.len() != 28 {
        panic!("typed hash input must be 28 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out[..28].copy_from_slice(&bytes);
    if is_script {
        out[28] = 0x01;
    }
    hex::encode(out)
}

fn transform_committee(committee_resp: &Value) -> Value {
    let row = committee_resp
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| panic!("committee_info returned empty"));
    let quorum_n = row
        .get("quorum_numerator")
        .and_then(|v| v.as_u64())
        .unwrap_or(2);
    let quorum_d = row
        .get("quorum_denominator")
        .and_then(|v| v.as_u64())
        .unwrap_or(3);
    let mut members: Vec<Value> = Vec::new();
    let mut resigned: Vec<String> = Vec::new();
    if let Some(arr) = row.get("members").and_then(|v| v.as_array()) {
        for m in arr {
            let cold_hex = m
                .get("cc_cold_hex")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("committee member missing cc_cold_hex: {m}"));
            let cold_is_script = m
                .get("cc_cold_has_script")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let cold_typed = typed_hash32_from_hex28(cold_hex, cold_is_script);

            let status = m.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "resigned" {
                resigned.push(cold_typed.clone());
                continue;
            }
            let expiration = m
                .get("expiration_epoch")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| panic!("committee member missing expiration_epoch: {m}"));
            let hot_typed = match (
                m.get("cc_hot_hex").and_then(|v| v.as_str()),
                m.get("cc_hot_has_script").and_then(|v| v.as_bool()),
            ) {
                (Some(hex_str), Some(is_script)) if !hex_str.is_empty() => {
                    Value::String(typed_hash32_from_hex28(hex_str, is_script))
                }
                _ => Value::Null,
            };
            members.push(serde_json::json!({
                "cold_key": cold_typed,
                "hot_key": hot_typed,
                "expiration": expiration,
            }));
        }
    }
    serde_json::json!({
        "members": members,
        "threshold": { "numerator": quorum_n, "denominator": quorum_d },
        "resigned": resigned,
    })
}

fn transform_pparams(epoch_params: &Value) -> Value {
    let pv_major = epoch_params
        .get("protocol_major")
        .and_then(|v| v.as_u64())
        .unwrap_or(9);
    let cms = epoch_params
        .get("committee_min_size")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    let cmt = epoch_params
        .get("committee_max_term_length")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(365);

    let read_threshold = |key: &str| -> Value {
        let v = match epoch_params.get(key) {
            Some(x) => x,
            None => return serde_json::json!({ "numerator": 0, "denominator": 1 }),
        };
        if let Some(f) = v.as_f64() {
            // Convert float to /10000 rational (matches loader's read_rational
            // float fallback exactly).
            let denominator: u64 = 10_000;
            let numerator = (f * denominator as f64).round() as u64;
            serde_json::json!({ "numerator": numerator, "denominator": denominator })
        } else if let Some(s) = v.as_str() {
            // Some Koios endpoints return numbers as strings.
            let f: f64 = s.parse().unwrap_or(0.0);
            let denominator: u64 = 10_000;
            let numerator = (f * denominator as f64).round() as u64;
            serde_json::json!({ "numerator": numerator, "denominator": denominator })
        } else if let Some(obj) = v.as_object() {
            serde_json::json!({
                "numerator": obj.get("numerator").and_then(|x| x.as_u64()).unwrap_or(0),
                "denominator": obj.get("denominator").and_then(|x| x.as_u64()).unwrap_or(1),
            })
        } else {
            serde_json::json!({ "numerator": 0, "denominator": 1 })
        }
    };

    serde_json::json!({
        "protocol_version_major": pv_major,
        "committee_min_size": cms,
        "committee_max_term_length": cmt,
        "dvt_pp_network_group":         read_threshold("dvt_p_p_network_group"),
        "dvt_pp_economic_group":        read_threshold("dvt_p_p_economic_group"),
        "dvt_pp_technical_group":       read_threshold("dvt_p_p_technical_group"),
        "dvt_pp_gov_group":              read_threshold("dvt_p_p_gov_group"),
        "dvt_hard_fork":                 read_threshold("dvt_hard_fork_initiation"),
        "dvt_no_confidence":             read_threshold("dvt_motion_no_confidence"),
        "dvt_committee_normal":          read_threshold("dvt_committee_normal"),
        "dvt_committee_no_confidence":   read_threshold("dvt_committee_no_confidence"),
        "dvt_constitution":              read_threshold("dvt_update_to_constitution"),
        "dvt_treasury_withdrawal":       read_threshold("dvt_treasury_withdrawal"),
        "pvt_motion_no_confidence":      read_threshold("pvt_motion_no_confidence"),
        "pvt_committee_normal":          read_threshold("pvt_committee_normal"),
        "pvt_committee_no_confidence":   read_threshold("pvt_committee_no_confidence"),
        "pvt_hard_fork":                 read_threshold("pvt_hard_fork_initiation"),
        "pvt_pp_security_group":         read_threshold("pvt_p_p_security_group"),
    })
}

fn chrono_now() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}
