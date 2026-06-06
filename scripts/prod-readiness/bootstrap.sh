#!/usr/bin/env bash
# bootstrap.sh — idempotent one-time setup so the engine runs unattended.
#   1) preflight (gh auth, https remote, caffeinate, disk)
#   2) extend .claude/settings.local.json allowlist with every command the engine
#      issues (the #1 autonomy invariant: an allowlist gap blocks the loop)
#   3) scaffold .jobs/, db-clones/, dump dirs (common.sh does this on source)
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"
SETTINGS="$REPO_ROOT/.claude/settings.local.json"

log "preflight…"
gh auth status >/dev/null 2>&1 || die "gh not authenticated (run: gh auth login)"
git -C "$REPO_ROOT" remote get-url origin | grep -q '^https://' \
  || die "origin is not HTTPS — engine must push via gh/HTTPS, never SSH"
command -v caffeinate >/dev/null || die "caffeinate missing"
[ -x "$REPO_ROOT/target/release/dugite-node" ] \
  || log "WARN: release dugite-node not built (build with --features epoch-state-debug before a replay)"
[ "$(free_disk_gb)" -ge "$MIN_FREE_GB" ] || log "WARN: free disk below MIN_FREE_GB=$MIN_FREE_GB"

# Commands the engine issues that may be missing from the allowlist.
NEEDED=(
  "Bash(cp:*)" "Bash(caffeinate:*)" "Bash(df:*)" "Bash(du:*)" "Bash(mv:*)"
  "Bash(mkdir:*)" "Bash(cat:*)" "Bash(date:*)" "Bash(vm_stat:*)" "Bash(tail:*)"
  "Bash(awk:*)" "Bash(sed:*)" "Bash(tr:*)" "Bash(pgrep:*)" "Bash(ps:*)"
  "Bash(scripts/prod-readiness/lib/heavyop-lock.sh:*)"
  "Bash(scripts/prod-readiness/lib/gc-disk.sh:*)"
  "Bash(scripts/prod-readiness/lib/health-sample.sh:*)"
  "Bash(scripts/prod-readiness/lib/clone-db.sh:*)"
  "Bash(scripts/prod-readiness/lib/launch-replay.sh:*)"
  "Bash(scripts/prod-readiness/lib/poll-job.sh:*)"
  "Bash(scripts/prod-readiness/lib/koios.sh:*)"
  "Bash(scripts/prod-readiness/lib/wake-lock.sh:*)"
  "Bash(bash scripts/prod-readiness/test/test-smoke-wake.sh)"
  "mcp__koios__koios_account_reward_history" "mcp__koios__koios_account_updates"
  "mcp__koios__koios_pool_history" "mcp__koios__koios_pool_delegators_history"
  "mcp__koios__koios_pool_stake_snapshot" "mcp__koios__koios_pool_info"
  "mcp__koios__koios_epoch_params" "mcp__koios__koios_epoch_info" "mcp__koios__koios_tip"
)

log "auditing allowlist ($SETTINGS)…"
python3 - "$SETTINGS" "${NEEDED[@]}" <<'PY'
import json, sys
path = sys.argv[1]; needed = sys.argv[2:]
with open(path) as f: cfg = json.load(f)
allow = cfg.setdefault("permissions", {}).setdefault("allow", [])
added = [n for n in needed if n not in allow]
allow.extend(added)
with open(path, "w") as f: json.dump(cfg, f, indent=2); f.write("\n")
print("added:", len(added))
for a in added: print("  +", a)
PY
log "bootstrap complete."
