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
  and panics (a harness failure, never a sample failure) if one is left. A source scan of
  `dugite-primitives` fails the tests when a new field that could hold preserved bytes is neither
  scrubbed nor audited as harmless, and every decoded type a codec does not scrub must be declared
  `NoPreservedBytes`. `reuse_of_preserved_bytes_would_make_a_wrong_encoding_look_right` shows the
  encoder echoing an input from preserved bytes and the harness path not doing so.
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

## Changing a pin

A pin change is a corpus upgrade: edit `corpora.toml`, run `just ndc-fetch` (it names every
difference to resolve), `just ndc-selftest`, `just ndc-reports`, and compare `just ndc-score`.
