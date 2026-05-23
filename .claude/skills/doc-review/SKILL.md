---
name: doc-review
description: Review the Dugite project documentation (mdBook, published as GitHub Pages at https://michaeljfazio.github.io/dugite/) section by section for accuracy, relevance, and currency. Use when asked to review docs, update documentation, audit the wiki, check for stale content, or verify docs match the code. Works through every page in docs/src/ cross-referencing against actual source code.
---

# Doc Review

Dugite docs are an **mdBook** at `docs/src/`, published via GitHub Pages. Entry point: `docs/src/SUMMARY.md`.

**Goal**: For each page — read it, cross-check it against the code, and report every issue found with a concrete fix.

## Process

Work through every page in SUMMARY.md order. Do NOT skip pages.

### For each page

1. **Read** the doc file (`docs/src/<path>`)
2. **Identify cross-check targets** — see `references/doc-map.md` for which source files apply
3. **Flag issues** with the tags below
4. **Apply fixes** directly — edit the doc file inline, don't just report

### Issue tags

| Tag | Meaning |
|-----|---------|
| `[STALE]` | Was true, has since changed (version numbers, port, crate counts, feature flags) |
| `[WRONG]` | Factually incorrect against current code |
| `[MISSING]` | Feature/option/command exists in code but isn't documented |
| `[DEAD]` | Documents something removed from the codebase |
| `[VAGUE]` | Correct but too imprecise to be useful |

## Output

After reviewing all pages, produce a summary table:

```
| Page | Issue | Tag | Fix Applied |
|------|-------|-----|-------------|
| docs/src/introduction.md:32 | Port listed as 12798 | OK | — |
| docs/src/architecture/overview.md:3 | Says 14 crates, Cargo.toml has 15 | [STALE] | Updated count |
...
```

List every page — `OK` if clean, otherwise describe the fix.

## Key facts (high-signal cross-checks)

These are the most likely sources of stale content — check every doc that touches these topics:

- **Crate count**: Count actual `[workspace.members]` in `Cargo.toml` — docs say "14" in some places, "15" in others
- **Metrics port**: 12798 (restored in commit 9921bc577 after brief change to 12796)
- **Pallas / aiken**: Must NOT appear anywhere (removed in 9921bc577)
- **dugite-uplc**: In-house UPLC CEK machine; fully conformant as of 2026-05-23; must appear in architecture pages
- **Current version**: v1.7.0 — verify against `Cargo.toml` workspace version if docs mention versions
- **Preview testnet**: Requires cardano-node 11.0.1+ peers (PV11); worth noting in Networks page
- **Third-party licenses page**: Must not list pallas or aiken; `dugite-uplc` and its deps should appear

## Efficiency

Grep rather than reading whole files when checking a single claim:

```bash
grep -rn "pallas\|aiken" docs/src/
grep -rn "12796\|12798" docs/src/
grep -rn "14-crate\|15-crate\|14 crate\|15 crate" docs/src/
```

For code cross-checks, search the relevant crate:

```bash
grep -rn "TargetNumberOfActivePeers" crates/dugite-node/src/ --include="*.rs"
```

See `references/doc-map.md` for the full section-by-section map of which source files to verify against.
