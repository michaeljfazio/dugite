# Conformance Test Suite

Dugite's correctness story rests on three layers:

1. **Conformance** — byte-exact alignment vs upstream Cardano artefacts, verified by replay. This page.
2. **Feature compatibility** — what protocol features the node implements. See the wiki [Protocol Compliance](https://github.com/michaeljfazio/dugite/wiki/Protocol-Compliance) page.
3. **Operational soak testing** — sustained behaviour on live testnets (preview, preprod) and the local devnet.

This page documents the conformance suite: where the upstream fixtures come from, what each area validates, and how to replay any of them locally.

## The corpus model

```
upstream repos (SHA-pinned in sources.toml)
       │
       ▼
regenerate-conformance-corpus workflow
       │  produces 7 tarballs
       ▼
dugite GitHub release (tag pinned in manifest.toml)
       │
       ▼
just download-upstream-fixtures
       │
       ▼
dugite-conformance test harness (DUGITE_REQUIRE_UPSTREAM=1)
```

Upstream sources are pinned by commit SHA (or tag) in `tests/conformance/upstream/sources.toml`. The `regenerate-conformance-corpus` workflow consumes those pins, materialises the seven fixture areas into tarballs, and publishes them as assets of a single dugite GitHub release. Consumers (CI and local developers alike) then pin to that *release tag* via `tests/conformance/upstream/manifest.toml`, so a single fetch lands every fixture area at a known good combination. Tarballs are cached by content hash of `manifest.toml`, so bumping the tag invalidates the cache automatically.

This two-level pinning separates "what upstream version we test against" (`sources.toml`, only changes when we want to bump) from "what corpus the test run consumed" (`manifest.toml`, deterministic and cacheable).

## Status

| Area | Source | Coverage | Status |
|---|---|---|---|
| UPLC (Plutus) | IntersectMBO/plutus | 999 evaluation cases | 100% — skip list empty |
| ouroboros-consensus | IntersectMBO/ouroboros-consensus | Block / header golden files per era | passing |
| cardano-ledger | IntersectMBO/cardano-ledger | Genesis JSON, CDDL schema, golden transactions | passing |
| cardano-node | IntersectMBO/cardano-node | Genesis spec files | passing |
| ledger-rules (ImpSpec) | IntersectMBO/cardano-ledger | CBOR NEWEPOCH + LEDGER vectors from ImpSpec | passing — `SKIP_LIST` empty |
| cardano-base | IntersectMBO/cardano-base | VRF v03 crypto test vectors | passing |
| mithril | input-output-hk/mithril | Certificate fixture JSON | passing |
