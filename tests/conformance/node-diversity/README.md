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
| `ledger-rules` runner and strict gate (#1125) | `ledger-rules/README.md`, `just ndc-gate` |
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
  and says "unknown ..." is a `parse/decode error`.)

**What the 1059 mean** (as pinned; `just ndc-score`): 746 vectors pass on the full textual result and
budget; 313 pass on the class alone (237 `evaluation failure`, 69 `parse/decode error`, 7
`out of budget`), where nothing but the class is compared. Of the 69 `parse/decode error` passes, 58 are
the text parser rejecting a `.uplc`, 4 are the flat decoder and 7 are the program-version gate
(`plcVersionsAvailableIn`, #1128). The 5 failures are pinned in
`uplc-conformance-known-failures.json` with the observed outcome and the issue tracking each (#1129,
#1130); `ndc_uplc_conformance.rs` fails on a new failure, on a listed vector that now
passes, and on a changed outcome. Reporting itself never fails on a score.

**What cannot go red.** Mutation checks against this corpus show the protocol version handed to the
cost-model applier is not exercised (a wrong one changes no vector), and the cost-model source and the
semantics variant move only 11 vectors each. The exact-budget guard for those lives in
`crates/dugite-uplc/tests/phase2_onchain_budget.rs` (ten Babbage redeemers pinned to their declared
ExUnits) and a unit test of the variant selection in `eval_redeemer.rs`, not in this scenario.

## Changing a pin

A pin change is a corpus upgrade: edit `corpora.toml`, run `just ndc-fetch` (it names every
difference to resolve), `just ndc-selftest`, `just ndc-reports`, and compare `just ndc-score`.

## The uplc-turbo runner (Phase 2d, #1178)

The runner is `tests/conformance/src/node_diversity/uplc_turbo.rs`.

* `just ndc-report uplc-turbo` runs all 3,997 programs in the release profile. It takes about 2.5 minutes on an Apple M2 Max.
* `just ndc-turbo-quick` runs every 40th program and writes to `reports-quick/`. The result is always a `stub`.

**Path.** Each corpus file is the on-chain script: a raw CBOR byte string around flat. It is not hex text.
The runner calls `prepare_script_bytes_gated` once. This step is not timed. It does the cost model, the language gate, the decode, the availability rules and the scope check.
Then it calls `PreparedScript::run` 50 times. This step is timed. `eval_script_bytes_gated` is these two calls, so there is one evaluator.

**Measurement.** The runner keeps the 30 fastest of 50 sequential runs.
`cpu_us` and `mem_bytes` are means over those same runs.
`mem_bytes` is the number of bytes the evaluation asked the allocator for, on its own thread. A counting global allocator in the test binary does this.
A reallocation counts as a new allocation. A free does not subtract. This is the same as Go `TotalAlloc`, which Dingo reports. It is not a peak.

**Cost models.** Protocol versions 10 and 11 use the models of the `uplc-conformance` corpus. Version 9 uses the version 10 arrays. Version 8 uses `uplc-turbo-cost-models-pv8.json`.
The runner checks the SHA-256 of that file and the length of its two arrays (332).
The file has no pinned copy to compare with. These checks guard it instead:
* A test lists budgets from the cardano-node report for programs of each protocol version and language.
* `every_program_budget_equals_the_published_reports` compares the budget of all 3,997 programs with the cardano-node report and the Amaru report. `just ndc-report uplc-turbo` runs it in release.

**Who may publish.** Only a complete run with `DUGITE_NDC_PUBLISHING_RUNNER` set has status `ok`.
Every other run has status `stub`. This includes a run on a laptop and a sample.
A publish step must refuse a `stub`. `just ndc-score` still shows the score of a `stub`.
The `.status.json` note gives the machine, the rustc version, the load average and the wall time.

**Failures.** A corpus file without a valid outer CBOR wrapper stops the run.
A program that does not evaluate gets `null` budgets and `null` `mem_bytes`, and `cpu_us` 0.
On the publishing runner such a program stops the run, because every corpus program ran on mainnet.
A panic or an internal error always stops the run.
