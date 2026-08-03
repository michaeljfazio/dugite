//! Offline re-evaluation of a captured phase-2 divergence dump.
//!
//! A `DUGITE_PHASE2_DUMP_DIR` dump (written by the live node when its parallel
//! UPLC eval disagrees with on-chain `is_valid`) is fully self-contained: tx
//! CBOR, resolved input UTxOs, on-chain cost models, the tx-level budget, the
//! protocol version, and the slot config. This bin feeds those straight back
//! into `dugite_uplc::phase_two::eval_phase_two_raw`, reproducing the exact
//! script evaluation deterministically and offline — the "re-eval tx from
//! chain" tool ISSUE-4 was missing.
//!
//! Usage:  replay_phase2 <dump.json>

use std::fs;

use dugite_uplc::phase_two::{eval_phase_two_raw, SlotConfig};

fn hexd(s: &str) -> Vec<u8> {
    hex::decode(s).expect("invalid hex in dump")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // `--flat <file>`: eval a single applied UPLC term (flat) alone with a huge
    // budget (so it always COMPLETES — no per-redeemer cap), and report full
    // step/builtin counts + total consumed. Used to localize a per-redeemer
    // divergence by diffing against the Haskell `uplc evaluate --counting`
    // per-step/per-builtin breakdown — the authoritative one. A third-party
    // evaluator can corroborate but cannot settle a disagreement (#970).
    if args.get(1).map(|s| s.as_str()) == Some("--flat") {
        let file = args
            .get(2)
            .expect("usage: replay_phase2 --flat <term.flat>");
        let bytes = fs::read(file).expect("read flat");
        let prog = dugite_uplc::program::Program::from_flat(&bytes).expect("from_flat");
        let huge = dugite_uplc::machine::cost::ExBudget {
            cpu: i64::MAX / 4,
            mem: i64::MAX / 4,
        };
        // Optional 3rd arg: a dump JSON whose on-chain V2 cost model is used
        // (so CPU costs match the live node). Without it, default costs.
        let mut tracker = if let Some(dump) = args.get(3) {
            let v: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(dump).unwrap()).unwrap();
            let cm_cbor = hexd(v["cost_models_cbor"].as_str().unwrap());
            let cm = dugite_uplc::cost_models::decode_cost_models_cbor(&cm_cbor).unwrap();
            let params = cm.plutus_v2.as_deref().unwrap();
            let applied = dugite_uplc::cost_apply::apply_v2(params, 11).unwrap();
            dugite_uplc::machine::cost::BudgetTracker::with_applied(huge, applied)
        } else {
            dugite_uplc::machine::cost::BudgetTracker::new(huge)
        };
        let variant = dugite_uplc::builtin::semantics::SemanticsVariant::for_script(
            dugite_uplc::redeemer_resolve::ScriptLanguage::PlutusV2,
            11,
        );
        let res = dugite_uplc::machine::step::evaluate_with_budget(
            prog.term,
            &mut tracker,
            None,
            variant,
        );
        let consumed = tracker.consumed();
        let steps = dugite_uplc::machine::cost::take_step_trace();
        let builtins = dugite_uplc::builtin::dispatch::take_builtin_trace();
        let step_mem: i64 = steps.iter().map(|(_, _, m, _)| *m).sum();
        let bi_mem: i64 = builtins.iter().map(|(_, _, m, _)| *m).sum();
        eprintln!(
            "[flat] {file} ok={} consumed: cpu={} mem={}",
            res.is_ok(),
            consumed.cpu,
            consumed.mem
        );
        eprintln!("[flat] step kinds:");
        for (k, c, m, n) in &steps {
            eprintln!("  {k:<14} cpu={c:<14} mem={m:<10} count={n}");
        }
        eprintln!(
            "[flat] sum step_mem={step_mem} sum builtin_mem={bi_mem}  startup_mem = consumed - steps - builtins = {}",
            consumed.mem - step_mem - bi_mem
        );
        eprintln!("[flat] per-builtin (cpu / mem / count), sorted by cpu:");
        let mut b2 = builtins.clone();
        b2.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (name, cpu, mem, n) in &b2 {
            eprintln!("  {name:<24} cpu={cpu:<14} mem={mem:<10} count={n}");
        }
        return;
    }

    let path = args
        .get(1)
        .cloned()
        .expect("usage: replay_phase2 <dump.json>");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("read dump")).expect("parse dump");

    let tx_cbor = hexd(v["tx_cbor"].as_str().unwrap());
    let cost_models_cbor = hexd(v["cost_models_cbor"].as_str().unwrap());
    let utxos: Vec<(Vec<u8>, Vec<u8>)> = v["utxo_pairs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                hexd(p["input"].as_str().unwrap()),
                hexd(p["output"].as_str().unwrap()),
            )
        })
        .collect();
    let budget = (
        v["max_ex_cpu"].as_u64().unwrap(),
        v["max_ex_mem"].as_u64().unwrap(),
    );
    let slot_config = SlotConfig {
        network_start_unix_seconds: v["sc_network_start_unix_seconds"].as_u64().unwrap(),
        slot_zero_offset: v["sc_slot_zero_offset"].as_u64().unwrap(),
        slot_length_ms: v["sc_slot_length_ms"].as_u64().unwrap() as u32,
        safe_zone_horizon_slot: v["sc_safe_zone_horizon_slot"].as_u64(),
    };
    let major_pv = v["protocol_major"].as_u64().unwrap() as u32;

    eprintln!(
        "[replay] {} | pv={} tx_idx={} utxos={} budget(cpu={}, mem={})",
        path,
        major_pv,
        v["tx_idx"],
        utxos.len(),
        budget.0,
        budget.1
    );

    // Declared exUnits (= the Haskell-computed budget the submitter put on the
    // wire). Comparing dugite's consumed to this gives the exact over-charge.
    match dugite_serialization::decode_transaction(6, &tx_cbor) {
        Ok(tx) => {
            for r in &tx.witness_set.redeemers {
                eprintln!(
                    "[declared] redeemer tag={:?} index={} ex_units: mem={} cpu={}",
                    r.tag, r.index, r.ex_units.mem, r.ex_units.steps
                );
            }
        }
        Err(e) => eprintln!("[declared] tx decode failed: {e}"),
    }

    let mut obs = ();
    match eval_phase_two_raw(
        &tx_cbor,
        &utxos,
        Some(&cost_models_cbor),
        budget,
        slot_config,
        major_pv,
        true,
        &mut obs,
    ) {
        Ok(results) => {
            eprintln!("[replay] OK — {} redeemer(s) evaluated", results.len());
            for r in &results {
                eprintln!(
                    "  redeemer tag={:?} idx={} consumed: cpu={} mem={}  logs={}",
                    r.tag,
                    r.index,
                    r.consumed.cpu,
                    r.consumed.mem,
                    r.logs.len()
                );
                for (i, l) in r.logs.iter().enumerate() {
                    eprintln!("    trace[{i}]: {l}");
                }
            }
        }
        Err(e) => {
            eprintln!("[replay] DIVERGENCE REPRODUCED — eval failed: {e}");
        }
    }

    // Per-builtin charge breakdown (only populated when
    // DUGITE_UPLC_BUILTIN_TRACE is set). Sorted by total mem desc — the
    // builtin at the top is where dugite spends the most mem, and the place to
    // cross-check the cost shape against Haskell when localizing a divergence.
    let steps = dugite_uplc::machine::cost::take_step_trace();
    let builtins = dugite_uplc::builtin::dispatch::take_builtin_trace();
    let mut gcpu = 0i64;
    let mut gmem = 0i64;
    if !steps.is_empty() {
        eprintln!("[replay] per-CEK-step-type charges (kind: total_cpu / total_mem / count):");
        for (name, cpu, mem, n) in &steps {
            eprintln!("  {name:<28} cpu={cpu:<16} mem={mem:<12} count={n}");
            gcpu += cpu;
            gmem += mem;
        }
    }
    if !builtins.is_empty() {
        eprintln!("[replay] per-builtin charges (name: total_cpu / total_mem / count):");
        let (mut bcpu, mut bmem) = (0i64, 0i64);
        for (name, cpu, mem, n) in &builtins {
            eprintln!("  {name:<28} cpu={cpu:<16} mem={mem:<12} count={n}");
            bcpu += cpu;
            bmem += mem;
        }
        eprintln!(
            "  {:<28} cpu={bcpu:<16} mem={bmem:<12}",
            "subtotal(builtins)"
        );
        gcpu += bcpu;
        gmem += bmem;
    }
    if !steps.is_empty() || !builtins.is_empty() {
        eprintln!(
            "  {:<28} cpu={gcpu:<16} mem={gmem:<12}  (+ startup; vs the redeemer's declared exUnits)",
            "GRAND TOTAL(steps+builtins)"
        );
    }
}
