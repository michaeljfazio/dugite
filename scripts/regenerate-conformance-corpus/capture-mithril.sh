#!/usr/bin/env bash
# Capture step for the mithril area — STUB (wired up in Phase 6).
#
# Phase 6 will replace this stub with a script that clones
# input-output-hk/mithril at the pinned SHA and copies certificate
# fixture files used for Mithril certificate chain verification tests.
#
# Until Phase 6, this produces a placeholder tarball.
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

log() { echo "[capture-mithril] $*"; }
log "STUB — Phase 6 not yet implemented. Producing placeholder tarball."

CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CONTENT_DIR}"

cat > "${CONTENT_DIR}/README.txt" <<'EOF'
mithril — stub placeholder

This area will be populated in Phase 6 of the upstream conformance testing
implementation (see docs/superpowers/specs/2026-05-23-upstream-conformance-testing-design.md).

Phase 6 copies Mithril certificate fixture files from input-output-hk/mithril,
replacing the current ad-hoc fixtures in crates/dugite-node/tests/fixtures/.
EOF

echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"

tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Placeholder tarball written: ${TARBALL}"
