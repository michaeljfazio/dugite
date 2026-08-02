---
name: release-lead
description: Manages Dugite releases — version bumps, CI verification, changelog, GitHub release, and devnet-validate QA integration. Use when preparing a release tag, publishing crates, or checking release readiness.
---

# release-lead

Manages the Dugite release lifecycle from version bump through GitHub release publication. Integrates with the `devnet-validate` harness to gate releases on QA evidence.

## Pre-release checklist

Before tagging a new version:

1. **Version bump** — update `Cargo.toml` workspace version; verify all crate `[dependencies]` on sibling crates reference the same version.
2. **CI gate** — `just check` must pass (fmt + clippy + build + test).
3. **devnet-validate** — run the **standard** preset, all rounds, and attach the report.

   **The tag gate is the standard preset, run strict.** This is the single
   authoritative statement of that fact; `devnet-validate/SKILL.md` defers to
   it. (Until #953 the two skills disagreed — devnet-validate's capability
   matrix nominated the ~75-minute extended preset as the tag gate while both
   skills' actual commands hardcoded `--preset standard`, so every release
   v2.4.3–v2.4.5 shipped on standard and nothing extended-only ever gated a
   tag. The docs now describe what runs.)

   Do **not** hand-roll the round sequence here. Follow the Round 1–3 workflow
   in `.claude/skills/devnet-validate/SKILL.md`, which runs every suite the
   standard preset's evidence manifest requires — tx-zoo, cli-parity,
   adversarial N2N, the bidirectional parity oracle, and chaos. An earlier
   version of this checklist inlined an abbreviated recipe that ran none of the
   last four; the resulting report recorded them as zeros. The generator now
   refuses to produce a passing report from that evidence:

   ```
   GATE INTEGRITY: 4 violation(s)
     - cli-parity.csv absent in EVERY round (preset 'standard' requires it in at least one)
     - n2n-trace.csv absent in EVERY round (preset 'standard' requires it in at least one)
     - parity-matrix.csv absent in EVERY round (preset 'standard' requires it in at least one)
     - chaos-events.csv absent in EVERY round (preset 'standard' requires it in at least one)
   Refusing to report a PASS over evidence that was never produced.
   ```

   Then generate the report (strict is the default — never pass `--no-strict`
   for a tag):

   ```bash
   .claude/skills/devnet-validate/scripts/generate-release-report.sh \
     --preset standard \
     --tag <VERSION> \
     --round-names "baseline,epoch-boundary,restart" \
     --tx-zoo-state testnet/local-devnet/tx-zoo/state \
     --previous-report reports/devnet-validate/<PREVIOUS_TAG>.json \
     --output-dir reports/devnet-validate \
     "$EVD_ROUND1" "$EVD_ROUND2" "$EVD_ROUND3"
   ```

   Exit 3 means gate integrity failed — required evidence was absent, short of
   its pinned denominator, or borrowed from another round. Fix the run; do not
   work around it.

   **Before trusting the report**, confirm `gate_integrity.admissible` is
   `true`:
   ```bash
   jq '.gate_integrity' reports/devnet-validate/report.json
   ```
   `admissible: false` means the report is a partial run and must not gate a tag.

   The extended preset (`just devnet-validate-extended`, ~75 min) remains
   available as a deeper pre-major-release pass. It is not the tag gate.

4. **Commit the report** — `git add reports/devnet-validate/<tag>.json` and commit.
5. **Tag** — `git tag -s <VERSION> -m "Release <VERSION>"` then push.

## GitHub release body

When creating the GitHub release with `gh release create`:
- **Prepend the devnet-validate report.md** to the release body.
- Attach `reports/devnet-validate/<tag>.json` as a release asset.

Template:
```bash
gh release create <TAG> \
  --title "Dugite <TAG>" \
  --notes "$(cat reports/devnet-validate/<tag>.md
echo
echo '---'
echo '<hand-written changelog here>')" \
  reports/devnet-validate/<tag>.json \
  target/release/dugite-node \
  target/release/dugite-cli \
  target/release/dugite-monitor
```

## Crate publishing order

Publish in dependency order (primitives first, node last):
```
dugite-primitives → dugite-crypto → dugite-serialization
  → dugite-storage → dugite-ledger → dugite-consensus → dugite-network
  → dugite-mempool → dugite-node → dugite-cli → dugite-monitor → dugite-config
```

## Report storage

- `reports/devnet-validate/<tag>.json` — checked into `main`, provides trend baseline for next release.
- Released as a GitHub release asset — provides a stable URL for CI trend comparison.

## Hard rules

- Never tag if `just check` fails — not even for "emergency" releases.
- Never tag without a devnet-validate standard run PASS.
- Never force-push `main`.
- If the devnet-validate run fails: file a bug, fix it, re-run before tagging.

## Version scheme

Dugite uses semantic versioning (`vMAJOR.MINOR.PATCH`). Increment:
- `PATCH` — bug fixes only, no new wire-format changes.
- `MINOR` — new features, new protocol support, new CLI commands.
- `MAJOR` — breaking ledger/network incompatibility (rare).
