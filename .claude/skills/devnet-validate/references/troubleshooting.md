# Troubleshooting — known gotchas

## Genesis is stale

**Symptom**: `run.sh` exits with `Genesis is XXX seconds old (>300s)`.

**Cause**: Genesis `systemStart` is more than 5 minutes from now. Either you ran `setup.sh` long ago or your clock skewed.

**Fix**: `./setup.sh` again. It's idempotent.

## Ports in use

**Symptom**: `run.sh` exits with `Port already bound` or socket creation fails.

**Cause**: A previous run left processes alive, or another devnet is up.

**Fix**:
```bash
./stop.sh
for p in 3000 3001 3002 12798 12799 12800; do
  pid=$(lsof -tiTCP:$p -sTCP:LISTEN 2>/dev/null); [ -n "$pid" ] && kill -9 $pid
done
rm -f testnet/local-devnet/state/*.sock testnet/local-devnet/state/*.pid
```

## cardano-node too old (PV mismatch)

**Symptom**: cardano-bp dies at boot with `Version number ... not supported` or refuses to read Conway genesis.

**Cause**: Devnet genesis is Conway PV10; cardano-node < 11.0.1 refuses it (memory: `project_preview_pv11_requires_cn11`).

**Fix**: Upgrade cardano-node. On macOS arm64, the bundled binary needs ad-hoc codesign + bundled dylibs.

## Stale intersection bug (Round 3 hazard)

**Symptom**: After restarting dugite-bp, its tip stays frozen at the pre-restart slot even though the relay's tip advances. `tip_age_seconds` climbs monotonically.

**Cause**: When dugite-bp opens ChainSync to the relay, if the relay's tip is past dugite-bp's known tip BUT the intersection lands at `origin` for any reason, dugite never re-intersects (memory: `project_stale_intersection_when_peer_behind`).

**`run.sh` workaround**: Staggered start ensures relay has blocks before dugite-bp connects. But Round 3 restarts dugite-bp manually — make sure the relay is still advancing before re-launch.

**Detection**: `dugite_chain_sync_intersect_state` metric stays at `origin` after restart.

## KES expiry mid-run

**Symptom**: `dugite-bp.log` shows `KES sign failure` and forging stops.

**Cause**: KES period exceeded `maxKesEvolutions`. With f=0.5 and slotLength=1s, KES rolls roughly every `slotsPerKESPeriod` seconds; the default genesis is sized for >24h, so this shouldn't fire in a 20-min playbook.

**Fix if it does**: re-run `setup.sh`. Regenerates operational certs.

## macOS App Nap freezes the node

**Symptom**: Process state `SN` in `ps`; no log output for minutes; `tip_age_seconds` climbs; restart fixes it (memory: `project_macos_appnap_freeze_2026_05_08`).

**Fix**: `run.sh` already wraps `dugite-node` in `caffeinate -dimsu` on macOS. If you launched it manually for debugging, wrap it yourself.

## Tx-zoo V3 spend fails

**Symptom**: `03e-spend-v3.sh` (or similar) fails with a Plutus decoding error.

**Cause**: Vendored V3 always-true binary is stale — the V3 wire shape changed multiple times during Conway development.

**Fix**:
```bash
brew install aiken-lang/tap/aiken
testnet/local-devnet/tx-zoo/lib/build-plutus.sh   # regenerates with aiken
```

## `cardano-cli` against dugite socket hangs

**Symptom**: `cardano-cli query tip --socket-path state/dugite-bp.sock` blocks forever.

**Cause**: dugite's N2C server doesn't have the requested query handler, OR the LocalStateQuery negotiation failed silently.

**Diagnostic**:
```bash
strace -e trace=read -p $(lsof -t state/dugite-bp.sock) 2>&1 | head -20  # Linux
# macOS: use dtruss with sudo
```

If the N2C handshake fails, look in `dugite-bp.log` for `local_state_query` lines. Compat path direction is fixed by memory: `feedback_n2c_compat_test_direction` — always cardano-cli → dugite, never the reverse.

## DHCP rotation breaks inbound (lab gotcha)

**Symptom**: After hours of running, peers can no longer connect to dugite from outside loopback.

**Cause**: macOS dev box LAN IP rotated; router NAT port-forward invalidated (memory: `project_dhcp_lan_ip_rotation`). Irrelevant for loopback devnet but worth noting if you forward 3000/3001 externally.

**Fix**: pin DHCP reservation OR use a manual IP.

## Build artifact mtime is stale

**Symptom**: A "fixed" bug still reproduces after pulling the fix.

**Cause**: `./target/release/dugite-node` predates the commit holding the fix (memory: `feedback_rebuild_before_declaring_unfixed`).

**Fix**: `cargo build --release -p dugite-node` before declaring anything unfixed. Check `stat -f%m target/release/dugite-node` vs `git log -1 --format=%ct <fix-commit>`.

## Worktree confusion

**Symptom**: Run scripts can't find binaries; paths look weird.

**Cause**: Running from `.claude/worktrees/<name>/` instead of repo root. `testnet/local-devnet/run.sh` resolves paths relative to its own directory, but the dugite-node binary it launches is the repo's `target/release/dugite-node`. In a worktree, that path is the worktree's own `target/`, which may not be built.

**Fix**: Either run from the main checkout, or `cargo build --release -p dugite-node` inside the worktree first.

## Round failed — what to capture

Always bundle:
1. `git rev-parse HEAD` and `git status --short`
2. `cardano-node --version` and `cardano-cli --version`
3. `testnet/local-devnet/evidence/<ts>/` (entire directory)
4. `testnet/local-devnet/logs/*.log` (current snapshot)
5. `testnet/local-devnet/tx-zoo/state/results.csv` + `state/logs/`
6. Output of `verify.sh` (the failing line — quote verbatim)
7. Output of `analyze-evidence.sh` (the anomaly list)

Do NOT speculate on the root cause without these. Quote evidence, then form hypotheses.
