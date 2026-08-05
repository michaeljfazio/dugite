---
name: chap-dependency-pinning-methodology
description: How to resolve which exact cardano-ledger (or cardano-api) commit a cardano-node release actually ships, now that cardano-node no longer git-pins cardano-ledger directly
type: reference
---

As of cardano-node 11.x, `cabal.project` has **no `source-repository-package`
stanza for `cardano-ledger` or `cardano-api`** — both flow through CHaP
(Cardano Haskell Packages, `https://chap.intersectmbo.org/`, mirrored at
`github.com/IntersectMBO/cardano-haskell-packages`) as released package
versions, pinned only by `index-state: cardano-haskell-packages <timestamp>`.
The repo's own comment is explicit: *"Do NOT add more source-repository-package
stanzas here unless they are strictly temporary."* Do not assume an old-style
git pin exists — check `cabal.project` first, but expect this shape for any
recent release.

**Resolution procedure** (verified end-to-end for cardano-node 11.0.1):

1. Resolve the release tag to a commit: `gh api repos/IntersectMBO/cardano-node/git/refs/tags/<tag>` — for an *annotated* tag this returns a tag object, not a commit; dereference it with `gh api repos/IntersectMBO/cardano-node/git/tags/<tag_sha>` to get `.object.sha` (the real commit) plus GPG verification info.
2. Fetch `cabal.project` at that commit, note the `cardano-haskell-packages <index-state timestamp>` line.
3. Find which package actually carries the code you care about (e.g. PoolReap.hs lives under `cardano-ledger-shelley`, not `cardano-ledger-core`). Grep the relevant `.cabal` file (may be several hops away — `cardano-testnet.cabal`, not `cardano-node.cabal`, was the one with real `cardano-ledger-*` bounds for 11.0.1) for that package's version bound (e.g. `cardano-ledger-shelley >=1.16`, no upper bound is common).
4. In CHaP's repo, each package version has `_sources/<package>/<version>/meta.toml` with a **`timestamp`** (when published) and a **`github = { repo, rev }`** field — the exact upstream commit that version's tarball was cut from. This is the load-bearing provenance record; use it directly instead of trying to reverse-engineer commit SHAs from tag-naming conventions (cardano-ledger's own git tags, e.g. `cardano-ledger-shelley-test-1.9.0.0`, are for a *different* test package and are NOT reliable stand-ins).
5. Pick the **highest version whose `meta.toml` timestamp is ≤ the consuming project's index-state**, subject to the version bound found in step 3. `[[revisions]]` entries in `meta.toml` are metadata-only cabal-file patches (bounds/flags) — they do NOT change the pinned `github.rev`, so a revision published after your cutoff does not disqualify the package version itself if the *original* publish timestamp is within range.
6. Sanity-check robustness: if the architecture/mechanism you're verifying was already present several versions *before* the one you resolved to (check the file's own commit history via `gh api repos/.../commits?path=<file>`), your qualitative verdict is insensitive to being off by one patch/minor version in step 5 — valuable given step 5 is reasoned, not an actual `cabal build --dry-run` solve.

**Caveat**: this is dependency-bound reasoning, not an executed cabal solver
run. It's reliable for "which commit's *code* is in this release" questions:
adjacent CHaP releases of the same package are minutes-to-hours apart when
cut from a monorepo release cycle (e.g. `cardano-ledger-shelley` 1.18.1.0 and
`cardano-ledger-core` 1.20.0.0 both trace to commits on 2026-04-13, ~3h
apart) so getting the exact patch version right rarely changes which era of
the *architecture* you're looking at.

See [[poolreap-active-purge-verified-11-0-1]] for a worked example.
