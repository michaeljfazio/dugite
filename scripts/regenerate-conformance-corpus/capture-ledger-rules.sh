#!/usr/bin/env bash
# Capture step for the ledger-rules area — STUB (wired up in Phase 4).
#
# Phase 4 will replace this stub with a script that:
#   1. Builds cardano-ledger-conformance via the cardano-ledger Nix flake.
#   2. Runs `cabal test cardano-ledger-conformance` with
#      CONFORMANCE_CBOR_DUMP_PATH=./dumps to produce per-ImpSpec CBOR vectors.
#   3. Walks the dump tree and packages it as ledger-rules.tar.gz.
#
# Until Phase 4, this produces a placeholder tarball so the orchestrator
# can still publish a complete release with all 7 area slots present.
# corpus-manifest.json will show `"stub": true` for this area.

set -euo pipefail

SOURCES_TOML="" WORK_DIR="" TARBALL=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --sources-toml) SOURCES_TOML="$2"; shift 2 ;;
        --work-dir)     WORK_DIR="$2";     shift 2 ;;
        --tarball)      TARBALL="$2";      shift 2 ;;
        *)              echo "Unknown: $1" >&2; exit 1 ;;
    esac
done

log() { echo "[capture-ledger-rules] $*"; }
log "STUB — Phase 4 not yet implemented. Producing placeholder tarball."

CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CONTENT_DIR}"

cat > "${CONTENT_DIR}/README.txt" <<'EOF'
ledger-rules — stub placeholder

This area will be populated in Phase 4 of the upstream conformance testing
implementation (see docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md).

Phase 4 runs the cardano-ledger ImpSpec test suite at the pinned SHA with
CONFORMANCE_CBOR_DUMP_PATH set, captures per-era CBOR dump files, and
packages them here as ledger-rules.tar.gz.
EOF

# Emit the stub sentinel so the orchestrator sets "stub": true in the manifest.
echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"

tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Placeholder tarball written: ${TARBALL}"
