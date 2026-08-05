---
name: issue-1006-cli-surface-parity-gate
description: Built the cardano-cli vs dugite-cli subcommand surface-enumeration gate; found 82 real gaps and two parser bugs in the checker itself
metadata:
  type: reference
---

Follow-up to [[issue-998-cip94-poll-commands-removed]]. #998's real finding
was structural: no suite in the gate enumerates `cardano-cli`'s subcommand
surface against `dugite-cli`'s. Built `scripts/validation/cli-surface-parity.sh`
(#1006, commit `ffdbfb16e4`) to close it — a recursive `--help`-tree walker,
NOT a hardcoded era list (the #969/#971 "absent from a hardcoded list never
runs" shape), diffed both directions with an allowlist mirroring the
ledger-rules `SKIP_LIST` discipline (`scripts/validation/cli-surface-known-gaps.txt`,
one tracking issue per entry).

**Two real parser bugs found by actually running it** (the #993 discipline —
run the check you just wrote):
1. bash associative arrays reject an empty-string key (`arr[""]=1` itself
   errors "bad array subscript", not just via variable expansion) — the
   root-level walk call hit this.
2. cardano-cli's `transaction build`/`build-raw`/`build-estimate` have a
   multi-PARAGRAPH description (blank line, then an ANSI-colored warning
   paragraph) that a naive "blank line ends the Available-commands: block"
   rule truncated on, silently losing sign/witness/assemble/submit/policyid/
   .../txid from the walk — the FIRST full run reported a plausible-looking
   but WRONG 79-gap count before this was caught by re-checking a suspicious
   entry (`transaction signed-transaction`) against raw --help output. The
   fix (never end on a bare blank line, only on a real header) then broke
   dugite-cli's clap output instead: clap's "Commands:" section is
   IMMEDIATELY followed by a real "Options:" section, and reading past it
   captured "-h," as a bogus child, which recursed combinatorially via
   clap's own error-recovery help text (exponential path blowup, "max depth
   exceeded" flood). Fixed by terminating explicitly on
   `Options:`/`Available options:`/`Usage:` headers, never on blank lines
   alone — verified directly against both frameworks' real --help output.

**Real finding**: cardano-cli 11.0.0.0 vs current dugite-cli — 69/151
matched, 82 real gaps (filed as #1008, grouped: Byron ceremony/legacy 14,
genesis ceremony 8, governance 11, hash 3, key conversion 10, node/BLS 3,
query 10, stake-address/pool 6, transaction 4, cip-format 4, debug 3, bare
utility `ping`/`version` 2), 20 dugite-only superset commands (informational,
covers #935's documented era-prefix leniency). #1008 explicitly flags that
the check's normalized-path comparison can false-positive "MISSING" for a
capability that exists under a DIFFERENT path/name on dugite's side (e.g.
`query tx-mempool info/next-tx/tx-exists` as subcommands vs dugite's single
`query tx-mempool` — same capability, different structure) — whoever picks
up an #1008 item should re-verify against the SUPERSET list first.

Selftest (`cli-surface-parity-selftest.sh`, 18/18) drives the walker against
fixture stub CLIs (`scripts/validation/fixtures/cli-surface/`) rather than
real binaries — proves MISSING/allowlist-coverage/stale-allowlist/superset/
INCONCLUSIVE all work, including a reproducible RED case, and the reference
fixture reproduces the exact multi-paragraph/ANSI shape that caused bug #2
so it can't regress silently. Wired into `ci.yml` as its own job (`cardano-cli
/ dugite-cli subcommand surface parity`): downloads+sha256-verifies the
pinned cardano-cli-11.0.0.0 Linux release tarball (asset unpacks to
`cardano-cli-x86_64-linux`, not `cardano-cli` — verified via `tar -tzf`, not
assumed). NOT verified end-to-end on an actual GH Actions runner (no push
from the worktree) — download URL/checksum/archive contents verified live,
execution on ubuntu-latest unconfirmed.
