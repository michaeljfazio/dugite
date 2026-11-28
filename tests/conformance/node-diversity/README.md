# Node-diversity conformance harness

Shared infrastructure for the four scenarios published at
<https://pragma-org.github.io/node-diversity-conformance/> (NDC): `ledger-rules`,
`cbor-decoding`, `uplc-conformance`, `uplc-turbo`. Tracking issue: #1114; this is Phase 1 (#1116).
The runners themselves are Phases 2a-2d.

| Piece | Where |
|---|---|
| Every pin (commits, SHA-256) | `corpora.toml` (this directory) |
| Fetch and verify | `scripts/node-diversity/fetch-corpora.sh` |
| Report envelopes, writer, schema validation | `tests/conformance/src/node_diversity/` |
| Runners and harness tests | `tests/conformance/tests/ndc_reports.rs` |
| Score with NDC's own builder | `scripts/node-diversity/score-reports.sh` |
| Recipes | `just ndc-fetch`, `ndc-report <scenario>`, `ndc-reports`, `ndc-score`, `ndc-selftest` |

```sh
just ndc-reports   # fetch + verify the corpora, write and schema-validate all reports
just ndc-score     # score them with the site's builder (--preview)
just ndc-selftest  # offline: a corrupted pin must fail the fetch, for the right reason
```

Corpora go to `target/node-diversity/corpora/` and reports to `target/node-diversity/reports/`
(both gitignored). `DUGITE_NDC_DIR` and `DUGITE_NDC_REPORTS_DIR` override them.

## Rules the harness keeps

* **Reporting never fails on a score.** Only a harness failure fails: a pin that does not verify, a
  report that does not match its scenario's `schema.json`, an unwritable path.
* **A runner is an ordinary test.** `DUGITE_NDC_REPORT=<path>` names its report file
  (`DUGITE_NDC_CBOR_ERA=conway|dijkstra` picks the era of a `cbor-decoding` run). Unset, the runner
  does nothing, so a plain `cargo nextest run` needs no corpus.
* **A crashed generator writes an error report** (`write_generated`), so every scenario still has a report.
* **A generator that fails fails the run, and still leaves a report.** A crash or an unprepared
  input writes an `error` report first (so the scenario can still be published), then the runner
  test panics: `ndc-report` and `ndc-reports` exit non-zero (`ndc-reports` finishes the other
  scenarios first), as does `score-reports.sh`.
* **A report that carries no result says so in `<report>.status.json`** (`ok`, `stub` or `error`).
  Only `ledger-rules`' schema has room for an `error` field; the other three schemas forbid extra
  fields, so for them the empty payload is the schema-valid form and the sidecar is the record. A
  publish step (Phase 3) must refuse to publish anything whose status is not `ok`: the site would
  score a stub as 0, not as "no report". The sidecar also records the SHA-256 of `corpora.toml` and
  the dugite commit; `score-reports.sh` marks a report that does not match the current pins and
  checkout `STALE` (no sidecar: `UNBOUND`).
* **`just ndc-reports` also runs the whole `ndc_reports` test binary with `DUGITE_NDC_REQUIRE=1`**
  (`just ndc-harness-test`), so the red-proofs against NDC's real `schema.json` files cannot skip.
* **The envelope differs per scenario.** `cbor-decoding` is format version 2 (no `scenario`, integer
  milliseconds, flat `implementation_commit`, one report per era); the other three are version 1.
  See the table in `report.rs`.

## The cbor-decoding runner

`tests/conformance/src/node_diversity/cbor_decoding/` (Phase 2c, #1126) runs the pinned
cardano-cbor-dataset, once per era (`DUGITE_NDC_CBOR_ERA=conway|dijkstra`). Each sample goes to
dugite's production decoder for its rule (`dugite_serialization::decode::rules`); a sample that must
decode is re-encoded with dugite's own encoder (`encode::rules`) and compared with `.expected.cbor`
(after the dataset's normalisation, exactly for the byte-exact rules); a sample that must be refused
must fail to decode. `registry.rs` is the rule to decoder table. A rule with no entry FAILS every one
of its samples (never skipped), and a rule is left out when dugite has no decoder at that
granularity, rather than given a decoder written for the harness.

The report's per-sample value is only `true`, so the harness guards itself, and publishes the
numbers next to the report as `<report>.guards.json` (and in the status sidecar's note):

* **No input-byte reuse.** `scrub.rs` clears every preserved-bytes field between decode and encode
  and panics (a harness failure, never a sample failure) if one is left. Every decoded type a codec
  does not scrub must be declared `NoPreservedBytes`.
  `reuse_of_preserved_bytes_would_make_a_wrong_encoding_look_right` shows the encoder echoing an
  input from preserved bytes and the harness path not doing so.
* **What the audit covers.** A scan PARSES (with `syn`) every non-`#[cfg(test)]` item of
  `dugite-primitives` and `dugite-serialization` and finds every struct field, tuple-struct field and
  enum-variant field (named or positional, the type on any line) whose type carries bytes
  (`Vec<u8>`, `Bytes`, any `[u8]` slice such as `Arc<[u8]>`, `Box<[u8]>`, `Cow<'_, [u8]>`, fixed
  `[u8; N]`, `KeepRaw<..>`; also inside `Option`, tuples and maps, and through type aliases), or whose
  name looks like preserved bytes (`raw_*`, `*_raw`, `original_*`), whatever its type. Each is either
  scrubbed or exempted by owner type AND field with a reason (`the_scan_sees_every_field_shape`
  proves the shapes). It does not look at bytes held behind a generic parameter or a type from
  another crate. After a scrub, and for every `plain` codec, the value is also formatted with `Debug`
  and checked for a populated byte `Option` or a `raw_*`/`original_*` field, independently of the
  scrub's own list, so a traversal gap is caught; a handful of content fields (`ipv4`, `ipv6`, cost
  vectors, `tx_fee_policy`) are allowed by name.
* **Opaque input that is re-emitted.** A Byron address keeps its payload as the bytes it was read
  from and writes them back, so an echo and a rebuild are the same thing for it. The harness
  recognises that for the `address` rule only (`samples_with_opaque_reemitted_input`; those samples
  are scored, but left out of the discriminating counts). No other rule is checked for it. The
  encoder also honours a decoded form hint, `TransactionOutput.is_legacy` (array or map output): not
  input bytes, but input information that survives the re-encoding. It is listed in the guards
  (`decoded_form_hints_the_encoder_honours`) and in the status note.
* **Entries use the reader production uses for the field, or the rule is unregistered, with these
  exceptions, which are adapters written for the harness:**
  * `script` is read through the `#6.24(bytes .cbor script)` wrapper the production script-reference
    decoder expects, by wrapping the item;
  * `pool_metadata` refuses `null` (the production reader accepts it for the optional field): that
    refusal is a harness check;
  * `account_balance_interval` and `account_balance_intervals` call the production interval readers
    directly, not through the sub-transaction body;
  * `alonzo_transaction_output` calls `read_legacy_tx_output` directly and skips the production
    array-or-map dispatch; `babbage_transaction_output` and `transaction_output` use the union
    reader (`read_babbage_tx_output`), which dispatches on array or map, so neither rule is checked
    for refusing the other form;
  * `redeemer` is read as the one element of the array form of `redeemers` (Conway's
    `read_redeemers_raw`, with `read_redeemer_tag`);
  * `unit_interval` and `nonnegative_interval` use `read_rational` (dugite checks no bounds there);
  * the `credential` family, `kes_period`, `epoch`, `slot`, `block_number` and
    `major_protocol_version` use one reader each for what are several rules.

  Keys and signatures (`vkey`, `vrf_vkey`, `kes_vkey`, `signature`) are read with
  `read_bytes_owned`, which checks no length; `big_int`, `bounded_bytes` and `%constr<plutus_data>`
  have no decoder at that granularity and fail as "no decoder registered"; `data` and `network_id`
  read exactly what the inline-datum arm and the transaction body read. Those readers accept some
  samples the dataset says must be refused (an inline datum followed by a trailing byte: #1181; a
  `network_id` other than 0 or 1 decoded as `None`: #1182); they show up in the
  invalid-samples-accepted counts, not as harness exceptions. `native_script`, `plutus_data`,
  `block`, `header` and `transaction` have no whole-input check of their own: they call the
  production entry point as it is.
* **"Refused" is any `Err`.** The reason of every refusal is written to `<report>.refusals.json`,
  summarised in `refusal_reasons`, and the `manual-invalid` refusals whose reason does not mention a
  defect their name states (`indefinite`, `trailing`, `duplicate`) are counted.
* **The rule-level entry points** (`dugite_serialization::decode::rules` / `encode::rules`) are
  behind the `conformance-rules` feature and hidden from the docs. The feature is not enabled by the
  node, but `tests/conformance` enables it and is a default workspace member, so a workspace-wide
  build turns it on. They are not all production
  encoders: `vote`, `ex_units`, `ex_unit_prices`, `constitution`, `account_balance_interval(s)`,
  `data`, `bytes` and `network_id` are written from CBOR primitives.
* **Discriminating samples.** `discriminating_valid_samples` counts valid samples whose input
  differs from `.expected.cbor` (and how many pass). Because the comparison normalises first, a
  stricter count is published too, `echo_proof_valid_samples`: the samples that echoing the input
  cannot pass (`normalize(input) != expected`).
* **Invalid samples count only if the rule's valid samples all decode.** The ungated number is
  published as `invalid_samples_refused_by_decoder`.
* **The dataset is checked against the harness**: every valid sample must equal `normalize(input)`,
  or agree once map entries and set elements are ordered, `Map`/`Set` re-encodings being what the
  reference does for some rules. Anything else is a harness defect and fails the run.
* The protocol version is stated in the report (`11.0` Conway, `12.0` Dijkstra).

`DUGITE_NDC_CBOR_EXPLAIN=<substring of a sample key>` prints input, expected and actual bytes for
every mismatching sample whose key contains it.

## The ledger-rules fixtures

NDC copied them from Amaru, then edited them. `fetch-corpora.sh` checks, at the pinned Amaru commit:

* NDC's 368 fixtures are Amaru's, compared type-strictly (`true`, `1` and `1.0` differ; a duplicate
  JSON key is an error), after undoing six named rewrites NDC made (rules R1-R6 in
  `ledger_rules_check.py`: an added `category` key, and the schema's null/empty-means-absent spellings
  at specific `initial_state` paths). Whatever else differs is listed in
  `[ledger_rules.known_divergences]` by LEAF PATH and must match exactly; an unlisted path fails, and
  so does a listed one that no longer differs. Every file under Amaru's `common/` (including
  `test-credentials/`) must be in NDC byte for byte, and NDC-only files there are listed. Ten fixtures are listed: six differ in wording only, four
  (reference-script size limit) differ in transaction bytes, because NDC replaced Amaru's
  `max_reference_scripts_size = 50` override with the real 204,800-byte limit.
* Three protocol-parameter files that 32 fixtures reference are not in NDC. They are added from
  Amaru, by SHA-256. The fetch **fails once NDC ships any of them**, so the workaround gets deleted.

## The uplc-conformance runner (Phase 2a, #1124)

`tests/conformance/src/node_diversity/uplc_conformance.rs` (printer: `uplc_print.rs`).

**Input paths.** 888 of the 1,064 vectors have a `.flat` (hex text); they run through
`dugite_uplc::eval_redeemer::eval_script_bytes_gated`, i.e. the production flat decoder. The other 176
have no `.flat` and run from their `.uplc` text through the TEXT parser (`dugite_uplc::syn`, behind the
`parser` feature), which is not the production decoder, then through `eval_program_gated`. All are
protocol version 11, V3: 131 BLS12-381 vectors (flat cannot carry BLS element constants), 20
`builtin/parser/value`, and 25 other constants, ill-typed inputs and similar. A `parse/decode error` for
one of those is the text parser rejecting the input, not the flat decoder.

Both entry points run the same per-script steps, through the same helpers, as the redeemer path
(`eval_resolved_redeemer`): cost model, language gate, availability rules, scope check, semantics variant,
CEK. They omit only what needs a transaction (building and applying the script context). The cost model
is resolved first and handed to the machine already resolved; a missing or unusable one is a harness
failure, never a fall back to the reference model. The corpus' cost model for the vector's protocol
version and Plutus language is used, with the scenario's 10B CPU / 14M MEM.

* The vector list, ids, titles and groups are NDC's `expectations.json`; the runner reads no expected
  result. A vector it cannot find, or a corpus vector no expectation names, fails the run.
* Error classes: `parse/decode error` (text parse, CBOR/flat decode, availability rules),
  `out of budget`, `evaluation failure` (everything else that stops a run).
* **Non-scoring outcomes fail the run.** An evaluator panic, a dugite-internal or encoder error, and a
  decoder arm that says it is "not yet wired" are reported as `evaluator panic`,
  `evaluator internal error` and `decoder not yet wired`; no expectation names them, so they never score,
  and a run that produces one writes an `error` report and fails. (A decoder that simply lacks a case
  and says "unknown ..." is a `parse/decode error`; #1127 tracks 62 of those.)

**What the 990 mean** (as pinned; `just ndc-score`): 703 vectors pass on the full textual result and
budget; 287 pass on the class alone (218 `evaluation failure`, 62 `parse/decode error`, 7
`out of budget`), where nothing but the class is compared. Of the 62 `parse/decode error` passes, 58 are
the text parser rejecting a `.uplc` and 4 are the flat decoder. The 74 failures are pinned in
`uplc-conformance-known-failures.json` with the observed outcome and the issue tracking each (#1127,
#1128, #1129, #1130); `ndc_uplc_conformance.rs` fails on a new failure, on a listed vector that now
passes, and on a changed outcome. Reporting itself never fails on a score.

**What cannot go red.** Mutation checks against this corpus show the protocol version handed to the
cost-model applier is not exercised (a wrong one changes no vector), and the cost-model source and the
semantics variant move only 11 vectors each. The exact-budget guard for those lives in
`crates/dugite-uplc/tests/phase2_onchain_budget.rs` (ten Babbage redeemers pinned to their declared
ExUnits) and a unit test of the variant selection in `eval_redeemer.rs`, not in this scenario.

## Changing a pin

A pin change is a corpus upgrade: edit `corpora.toml`, run `just ndc-fetch` (it names every
difference to resolve), `just ndc-selftest`, `just ndc-reports`, and compare `just ndc-score`.
