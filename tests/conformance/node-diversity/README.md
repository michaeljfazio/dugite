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
* **A report that carries no result says so in `<report>.status.json`** (`ok`, `stub` or `error`).
  Only `ledger-rules`' schema has room for an `error` field; the other three schemas forbid extra
  fields, so for them the empty payload is the schema-valid form and the sidecar is the record. A
  publish step (Phase 3) must refuse to publish anything whose status is not `ok`: the site would
  score a stub as 0, not as "no report".
* **The envelope differs per scenario.** `cbor-decoding` is format version 2 (no `scenario`, integer
  milliseconds, flat `implementation_commit`, one report per era); the other three are version 1.
  See the table in `report.rs`.

## The ledger-rules fixtures

NDC copied them from Amaru, then edited them. `fetch-corpora.sh` checks, at the pinned Amaru commit:

* NDC's 368 fixtures are Amaru's after undoing what NDC rewrites mechanically (a `category` key,
  JSON nulls, default-empty `initial_state` sections). Whatever else differs is listed in
  `[ledger_rules.known_divergences]` and must match exactly; an unlisted difference fails, and so does
  a listed one that no longer differs. Ten fixtures are listed: six differ in wording only, four
  (reference-script size limit) differ in transaction bytes, because NDC replaced Amaru's
  `max_reference_scripts_size = 50` override with the real 204,800-byte limit.
* Three protocol-parameter files that 32 fixtures reference are not in NDC. They are added from
  Amaru, by SHA-256. The fetch **fails once NDC ships any of them**, so the workaround gets deleted.

## Changing a pin

A pin change is a corpus upgrade: edit `corpora.toml`, run `just ndc-fetch` (it names every
difference to resolve), `just ndc-selftest`, `just ndc-reports`, and compare `just ndc-score`.
