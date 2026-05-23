#!/usr/bin/env bash
# Capture step for the cardano-base area — STUB (wired up in Phase 5).
#
# Phase 5 will replace this stub with a script that clones
# IntersectMBO/cardano-base at the pinned SHA and copies VRF/KES test
# vectors from paths such as:
#   cardano-crypto-tests/test_vectors/vrf_ver03_*
#   cardano-crypto-tests/test_vectors/kes_*
#   cardano-crypto-class/test/Test/Crypto/...
#
# Until Phase 5, this produces a placeholder tarball.
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

log() { echo "[capture-cardano-base] $*"; }
log "STUB — Phase 5 not yet implemented. Producing placeholder tarball."

CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CONTENT_DIR}"

cat > "${CONTENT_DIR}/README.txt" <<'EOF'
cardano-base — stub placeholder

This area will be populated in Phase 5 of the upstream conformance testing
implementation (see docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md).

Phase 5 copies VRF/KES test vectors from IntersectMBO/cardano-base and
cross-validates dugite-crypto against those vectors.
EOF

echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"

tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Placeholder tarball written: ${TARBALL}"
