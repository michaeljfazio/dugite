---
name: v1.4.0 release
description: v1.4.0 release details — first canonical block milestone, forge pipeline hardening, Dijkstra era, 215+ new tests
type: project
---

Released 2026-05-09.

Bump commit: `ac56551e3f9c3e85a1502b4696fcf016c19666e0`
Tag SHA: `5de3fa999671043c670225bd98c72cb3365ae3e4`
Release URL: https://github.com/michaeljfazio/dugite/releases/tag/v1.4.0
CI run: https://github.com/michaeljfazio/dugite/actions/runs/25599907146 (all 6 jobs green)

**Why:** First release after dugite forged its first canonical block on-chain (block 4265661, slot 111661041, preview testnet, 2026-05-09). Preprod SAND pool active from epoch 289. Mainnet validation underway.

**Key changes in this release:**
- Forge: applyChainTick gap fix, slot-battle forging, PraosCannotForgeKeyNotUsableYet gap, BlockFromFuture strict semantics
- Network/Handshake: skip unknown version-data entries, elevate failure logs to warn, P2P bare-BP gaps
- Era: Dijkstra era support for preview testnet
- Metrics: slot-battle counter correctness, live RTT snapshot
- Ledger: treasury withdrawal to unregistered accounts
- Security: 19 new fuzz targets + Plutus remote-DoS catch_unwind guard
- Tests: 215+ new unit tests (ledger 68, CLI 76, node 70, network)
- Deps: clap 4.6.1, rayon 1.12.0, rustls-webpki 0.103.13

**Process notes:**
- Remote had 1 benchmark commit ahead; needed `git pull --rebase` before push (branch was 2 ahead, remote was 1 ahead after recent nightly)
- CI has 6 jobs: coverage, build-and-test, integration-offline, then 3 release-binaries in parallel (linux-aarch64, macos-aarch64, linux-x86_64); total CI time ~45 min
- Scheduled nightly failures (Fuzz Testing, Nightly Benchmarks) are pre-existing and do not block push-triggered CI
- Local nextest: 4443 passed, 4 skipped, 0 failed in ~53s
