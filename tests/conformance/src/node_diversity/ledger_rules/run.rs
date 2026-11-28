//! Running one fixture through dugite's own validation path and reading the result off the wire.
//!
//! The path is the node's LocalTxSubmission path, step for step:
//!
//! 1. decode the transaction (`dugite_serialization::decode_transaction`, Conway);
//! 2. the ledger-derived context, from a real [`LedgerState`] (`mempool_validation_context()`);
//! 3. `validate_transaction_with_context`;
//! 4. on a rejection, `dugite_tx_admission::rejection_from_validation_errors` at the fixture's
//!    protocol version, then `encode_apply_tx_err`;
//! 5. decode THOSE BYTES through the tag table and name what they carry.
//!
//! Nothing below step 5 looks at dugite's own error type.

use dugite_ledger::utxo::CompositeUtxoView;
use dugite_ledger::validation::{validate_transaction_with_context, ValidationContext};
use dugite_network::protocol::local_tx_submission::encode::encode_apply_tx_err;
use dugite_primitives::time::SlotNo;

use super::decode::{decode_apply_tx_err, Wire};
use super::fixture::{Expected, Fixture};
use super::names::{name_failure, Named, Vocabulary};
use super::state::{build, Section};

/// The Conway HFC era index, which both the node's wire-era check and the encoder use.
const CONWAY_ERA: u16 = 6;

/// What dugite did with a fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The transaction was accepted.
    Pass,
    /// The transaction did not decode.
    DecodingFailure(String),
    /// Rejected; the failures the bytes carry, in wire order.
    Rejected(Vec<Named>),
    /// The bytes dugite emitted could not be read as an `ApplyTxErr` for Conway.
    BadWire(String),
    /// The HFC wrong-era answer.
    WrongEra,
    /// Validation panicked.
    Panicked(String),
}

/// A fixture's run.
#[derive(Debug, Clone)]
pub struct Run {
    pub id: String,
    pub expected: Expected,
    pub verdict: Verdict,
    /// Names of the `ValidationContext` fields that were `None` for this run.
    pub context_none: Vec<&'static str>,
    /// The hex of the `ApplyTxErr` bytes, when there were any (evidence for divergence issues).
    pub wire_hex: Option<String>,
    /// Debug text of the ledger's own `ValidationError`s: evidence for issues, never read to name a failure.
    pub ledger_errors: Vec<String>,
    /// When the transaction did not decode only because an output map is indefinite-length, the
    /// run of the rules on the transaction with that one shape undone.  Diagnostic only: it is
    /// what the rules would say if the decoder accepted what cardano-node accepts, so a divergence
    /// hidden behind the decoder's refusal is still seen.
    pub shadow: Option<Box<Run>>,
}

/// How the result is reported when dugite rejected the transaction but nothing it raised has a
/// scenario name.  The schema can only carry a scenario name, so the report uses a name that is
/// not the fixture's expectation, which scores as a miss exactly as the truth would.
pub const UNREPRESENTABLE_PLACEHOLDERS: [&str; 2] =
    ["ByronTxOutInContext", "ConflictingCommitteeUpdate"];

impl Run {
    /// The scenario-level result: `pass`, a scenario name, or `unmapped:<why>` when the
    /// transaction was rejected but nothing raised has a scenario name.  Anything outside the
    /// vocabulary is prefixed so it can never equal an expectation.
    pub fn name(&self) -> String {
        match &self.verdict {
            Verdict::Pass => "pass".into(),
            Verdict::DecodingFailure(_) => "DecodingFailure".into(),
            Verdict::WrongEra => "unmapped:HardForkApplyTxErrWrongEra".into(),
            Verdict::BadWire(why) => format!("unmapped:malformed-wire ({why})"),
            Verdict::Panicked(why) => format!("unmapped:panic ({why})"),
            Verdict::Rejected(raised) => {
                // The scenario's rule: if the expected failure is among the raised ones, report
                // it; otherwise report the first.
                let candidates: Vec<&str> = raised
                    .iter()
                    .flat_map(|n| n.ndc.iter().map(String::as_str))
                    .collect();
                if let Expected::Predicate { name, .. } = &self.expected {
                    if candidates.contains(&name.as_str()) {
                        return name.clone();
                    }
                }
                if let Some(first) = candidates.first() {
                    return (*first).to_string();
                }
                match raised.first() {
                    Some(n) if n.generic => {
                        "unmapped:ConwayMempoolFailure (generic fallback)".into()
                    }
                    Some(n) => format!("unmapped:{}", n.haskell),
                    None => "unmapped:no failure on the wire".into(),
                }
            }
        }
    }

    /// What the gate compares: [`Self::name`], narrowed by the payload detail the report cannot
    /// carry (`ValidationTagMismatch(PassedUnexpectedly)`), so the gate is stricter than the
    /// scenario's own scoring wherever the fixture is.
    pub fn observed(&self) -> String {
        let name = self.name();
        if name == "ValidationTagMismatch" {
            if let Verdict::Rejected(raised) = &self.verdict {
                if let Some(detail) = raised
                    .iter()
                    .find(|n| n.haskell == "ValidationTagMismatch")
                    .and_then(|n| n.detail.as_deref())
                {
                    return format!("{name}({detail})");
                }
            }
        }
        name
    }

    /// Did the fixture match?
    pub fn matched(&self) -> bool {
        self.observed() == self.expected.gate_name()
    }

    /// The value written to the report.  Always one of the scenario's names.
    pub fn report_result(&self) -> String {
        let observed = self.name();
        if observed.starts_with("unmapped:") {
            let want = self.expected.result_name();
            return UNREPRESENTABLE_PLACEHOLDERS
                .iter()
                .find(|p| **p != want)
                .map(|p| (*p).to_string())
                .expect("two placeholders, at most one equals the expectation");
        }
        observed
    }

    /// Every raised failure as one line each, for the committed snapshot.
    pub fn raised_lines(&self) -> Vec<String> {
        match &self.verdict {
            Verdict::Rejected(raised) => raised
                .iter()
                .map(|n| {
                    let names = if n.generic {
                        "GENERIC-FALLBACK".to_string()
                    } else if n.ndc.is_empty() {
                        "no-scenario-name".to_string()
                    } else {
                        n.ndc.join("|")
                    };
                    let detail = n
                        .detail
                        .as_ref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default();
                    format!("{} {:?} -> {names}{detail}", n.path, n.tags)
                })
                .collect(),
            Verdict::Pass => vec!["pass".into()],
            Verdict::DecodingFailure(why) => {
                let mut lines = vec![format!("decoding-failure: {why}")];
                if let Some(shadow) = &self.shadow {
                    lines.push(format!(
                        "shadow (outputs made definite): {} [{}]",
                        shadow.observed(),
                        if shadow.matched() {
                            "match"
                        } else {
                            "MISMATCH"
                        }
                    ));
                    lines.extend(
                        shadow
                            .raised_lines()
                            .into_iter()
                            .map(|l| format!("  shadow: {l}")),
                    );
                }
                lines
            }
            Verdict::BadWire(why) => vec![format!("malformed-wire: {why}")],
            Verdict::WrongEra => vec!["wrong-era".into()],
            Verdict::Panicked(why) => vec![format!("panic: {why}")],
        }
    }
}

/// The names of the `ValidationContext` fields that are `None`.  The destructuring is exhaustive,
/// so adding a field to the context is a compile error here until it is classified.
pub fn none_fields(ctx: &ValidationContext) -> Vec<&'static str> {
    let ValidationContext {
        registered_pools,
        current_treasury,
        reward_accounts,
        current_epoch,
        registered_dreps,
        drep_deposits,
        registered_vrf_keys,
        node_network,
        committee_members,
        committee_resigned,
        stake_key_deposits,
        constitution_script_hash,
        vote_delegations,
        active_proposals,
        enacted_gov_roots,
        committee_authorized_hot_keys,
        committee_authorized_elected_hot_keys,
        treasury,
        reserves,
        epoch_length,
        security_param,
        accumulated_mir_balances,
        genesis_delegates,
        genesis_delegate_keys,
        update_quorum,
        ..
    } = ctx;
    let mut out = Vec::new();
    macro_rules! check {
        ($($f:ident),* $(,)?) => { $( if $f.is_none() { out.push(stringify!($f)); } )* };
    }
    check!(
        registered_pools,
        current_treasury,
        reward_accounts,
        current_epoch,
        registered_dreps,
        drep_deposits,
        registered_vrf_keys,
        node_network,
        committee_members,
        committee_resigned,
        stake_key_deposits,
        constitution_script_hash,
        vote_delegations,
        active_proposals,
        enacted_gov_roots,
        committee_authorized_hot_keys,
        committee_authorized_elected_hot_keys,
        treasury,
        reserves,
        epoch_length,
        security_param,
        accumulated_mir_balances,
        genesis_delegates,
        genesis_delegate_keys,
        update_quorum,
    );
    out
}

/// The context fields the node's own `mempool_validation_context()` leaves `None` in Conway, with
/// the reason each is acceptable or the issue that tracks it being a gap.  The harness test
/// asserts the set is exactly this, so a field that becomes `Some` (or a new `None`) is noticed.
pub const CONTEXT_NONE_EXPECTED: &[(&str, &str)] = &[
    (
        "accumulated_mir_balances",
        "Shelley-Babbage MIR only (`MIRProducesNegativeUpdate`); Conway has no MIR certificates",
    ),
    (
        "genesis_delegates",
        "pre-Conway `NonGenesisUpdatePPUP`; Conway has no update proposals",
    ),
    (
        "treasury",
        "MIR pot-transfer predicates (Shelley-Babbage); Conway's treasury check reads `current_treasury`",
    ),
    (
        "reserves",
        "MIR pot-transfer predicates (Shelley-Babbage)",
    ),
    (
        "epoch_length",
        "MIR `MIRCertificateTooLateInEpoch` (Shelley-Babbage)",
    ),
    (
        "security_param",
        "MIR `MIRCertificateTooLateInEpoch` (Shelley-Babbage)",
    ),
    (
        "drep_deposits",
        "NOT set by `mempool_validation_context()`: `ConwayDRepIncorrectRefund` is silently skipped on the \
         N2C admission path (see the expected-results entries that name it)",
    ),
];

/// Run one fixture.  `skip` leaves one `initial_state` section out (the negative control).
pub fn run_fixture(
    fixture: &Fixture,
    vocab: &Vocabulary,
    skip: Option<Section>,
) -> Result<Run, String> {
    let mut run = Run {
        id: fixture.id.clone(),
        expected: fixture.expected.clone(),
        verdict: Verdict::Pass,
        context_none: Vec::new(),
        wire_hex: None,
        ledger_errors: Vec::new(),
        shadow: None,
    };
    // 1. decode.  A transaction that does not decode never reaches a rule.
    match dugite_serialization::decode_transaction(CONWAY_ERA, &fixture.transaction) {
        Ok(tx) => {
            validate_into(&mut run, fixture, &tx, vocab, skip)?;
        }
        Err(e) => {
            run.verdict = Verdict::DecodingFailure(e.to_string());
            // The decoder's refusal hides every rule the transaction was written to exercise.
            // When the cause is one the harness can undo without touching a signed byte
            // (an indefinite-length output map), run the rules on the transaction as if it had
            // decoded.  The result is a diagnostic, never the fixture's result.
            if let Some(tx) = shadow_decode(&fixture.transaction) {
                let mut shadow = Run {
                    id: run.id.clone(),
                    expected: run.expected.clone(),
                    verdict: Verdict::Pass,
                    context_none: Vec::new(),
                    wire_hex: None,
                    ledger_errors: Vec::new(),
                    shadow: None,
                };
                validate_into(&mut shadow, fixture, &tx, vocab, skip)?;
                run.shadow = Some(Box::new(shadow));
            }
        }
    }
    Ok(run)
}

/// Decode `tx_bytes` with every output's outer indefinite-length map made definite, then restore
/// the ORIGINAL bytes, hash and body span so that every hash, signature and size is the one the
/// fixture signed.  `None` when the transaction does not decode even then.
fn shadow_decode(tx_bytes: &[u8]) -> Option<dugite_primitives::transaction::Transaction> {
    use minicbor::Decoder;
    let mut d = Decoder::new(tx_bytes);
    if d.array().ok()? != Some(4) {
        return None;
    }
    // Element 0 is the body, a map.
    let body_start = d.position();
    d.skip().ok()?;
    let body_end = d.position();
    let rest_start = body_end;
    let body = &tx_bytes[body_start..body_end];

    // Rebuild the body: copy every entry, rewriting the output arrays (keys 1 and 16).
    let mut b = Decoder::new(body);
    let entries = b.map().ok()?;
    let mut out_body = Vec::new();
    match entries {
        Some(n) => {
            minicbor::Encoder::new(&mut out_body).map(n).ok()?;
        }
        // An indefinite-length body map stays indefinite: only the outputs change.
        None => out_body.push(0xbf),
    }
    let mut left = entries;
    loop {
        match left.as_mut() {
            Some(0) => break,
            Some(n) => *n -= 1,
            None => {
                if b.datatype().ok()? == minicbor::data::Type::Break {
                    break;
                }
            }
        }
        let key_start = b.position();
        let key = b.u64().ok();
        let key_end = b.position();
        out_body.extend_from_slice(&body[key_start..key_end]);
        let val_start = b.position();
        match key {
            Some(1) => {
                let n = b.array().ok()?;
                let mut enc_buf = Vec::new();
                match n {
                    Some(n) => {
                        minicbor::Encoder::new(&mut enc_buf).array(n).ok()?;
                    }
                    None => enc_buf.push(0x9f),
                }
                let mut left = n;
                loop {
                    match left.as_mut() {
                        Some(0) => break,
                        Some(c) => *c -= 1,
                        None => {
                            if b.datatype().ok()? == minicbor::data::Type::Break {
                                break;
                            }
                        }
                    }
                    let s = b.position();
                    b.skip().ok()?;
                    let one = &body[s..b.position()];
                    enc_buf.extend_from_slice(&super::state::definite_outer_map(one).ok()?);
                }
                if n.is_none() {
                    enc_buf.push(0xff);
                    // Step over the break byte so the body walk continues after the array.
                    b.set_position(b.position() + 1);
                }
                out_body.extend_from_slice(&enc_buf);
            }
            Some(16) => {
                b.skip().ok()?;
                let one = &body[val_start..b.position()];
                // A legacy array return, or a map: only a map can be indefinite.
                out_body.extend_from_slice(&super::state::definite_outer_map(one).ok()?);
            }
            _ => {
                b.skip().ok()?;
                out_body.extend_from_slice(&body[val_start..b.position()]);
            }
        }
    }
    if entries.is_none() {
        out_body.push(0xff);
    }
    let mut rebuilt = Vec::new();
    minicbor::Encoder::new(&mut rebuilt).array(4).ok()?;
    rebuilt.extend_from_slice(&out_body);
    rebuilt.extend_from_slice(&tx_bytes[rest_start..]);

    let mut tx = dugite_serialization::decode_transaction(CONWAY_ERA, &rebuilt).ok()?;
    // The signed body is the ORIGINAL one.
    tx.hash = dugite_primitives::hash::blake2b_256(body);
    tx.raw_body_cbor = Some(body.to_vec());
    tx.raw_cbor = Some(tx_bytes.to_vec());
    Some(tx)
}

/// Steps 2-5 of the module doc for an already-decoded transaction; fills `run.verdict`,
/// `run.context_none` and `run.wire_hex`.
fn validate_into(
    run: &mut Run,
    fixture: &Fixture,
    tx: &dugite_primitives::transaction::Transaction,
    vocab: &Vocabulary,
    skip: Option<Section>,
) -> Result<(), String> {
    let prepared = build(fixture, skip)?;
    let ledger = &prepared.ledger;

    // 2. the ledger's own context, plus the network, as `LedgerTxValidator::validate_tx` does.
    let context = ledger
        .mempool_validation_context()
        .with_network(dugite_primitives::network::NetworkId::Testnet);
    run.context_none = none_fields(&context);

    // The per-tx safe-zone horizon, from the era history, exactly as the node injects it.
    let mut slot_config = prepared.slot_config;
    if let Some(h) = prepared
        .era_history
        .safe_zone_horizon_slot(SlotNo(prepared.current_slot))
    {
        slot_config.safe_zone_horizon_slot = Some(h);
    }

    let utxo_view = CompositeUtxoView::new(&ledger.utxo.utxo_set, Default::default());
    let tx_size = fixture.transaction.len() as u64;
    let params = &ledger.epochs.protocol_params;

    // 3. validate, shielded from a panic so one fixture cannot hide the rest.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        validate_transaction_with_context(
            tx,
            &utxo_view,
            params,
            prepared.current_slot,
            tx_size,
            Some(&slot_config),
            context,
        )
    }));
    let errors = match outcome {
        Ok(Ok(())) => {
            run.verdict = Verdict::Pass;
            return Ok(());
        }
        Ok(Err(errors)) => errors,
        Err(payload) => {
            run.verdict = Verdict::Panicked(
                payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic".into()),
            );
            return Ok(());
        }
    };

    run.ledger_errors = errors.iter().map(|e| format!("{e:?}")).collect();

    // 4. the conversion and the encoder the node uses, at the fixture's protocol version.
    let rejection = dugite_tx_admission::rejection_from_validation_errors(
        errors,
        tx,
        &utxo_view,
        prepared.pv_major,
    );
    let bytes = encode_apply_tx_err(&rejection, CONWAY_ERA);
    run.wire_hex = Some(hex::encode(&bytes));

    // 5. read the bytes.
    run.verdict = match decode_apply_tx_err(&bytes) {
        Wire::WrongEra => Verdict::WrongEra,
        Wire::Malformed(why) => Verdict::BadWire(why),
        Wire::Failures(failures) => {
            let mut raised = Vec::new();
            for f in &failures {
                match name_failure(f, tx, vocab) {
                    Ok(mut n) => raised.append(&mut n),
                    Err(why) => {
                        run.verdict = Verdict::BadWire(format!("{}: {why}", f.leaf));
                        return Ok(());
                    }
                }
            }
            Verdict::Rejected(raised)
        }
    };
    Ok(())
}
