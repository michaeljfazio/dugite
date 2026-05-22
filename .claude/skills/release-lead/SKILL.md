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
3. **devnet-validate** — run the standard preset and attach the report:
   ```bash
   cd testnet/local-devnet
   ./setup.sh && ./run.sh
   sleep 30 && ./tx-zoo/run-all.sh --setup && ./tx-zoo/run-all.sh
   ./soak.sh 420   # 7 min — one epoch boundary
   ./stop.sh && ./setup.sh && ./run.sh
   sleep 30 && ./tx-zoo/run-all.sh --setup && ./tx-zoo/run-all.sh
   ./soak.sh 300   # restart round
   ./stop.sh
   # Generate release report
   .claude/skills/devnet-validate/scripts/generate-release-report.sh \
     --preset standard \
     --tag <VERSION> \
     --output-dir reports/devnet-validate \
     evidence/$(ls -t evidence | sed -n '2p') \
     evidence/$(ls -t evidence | head -1)
   ```
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
