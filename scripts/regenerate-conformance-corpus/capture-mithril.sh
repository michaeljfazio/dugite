#!/usr/bin/env bash
# Capture step for the mithril area (Phase 6 — certificate test fixtures).
#
# Clones input-output-hk/mithril at the SHA pinned in sources.toml and
# copies static certificate test fixture JSON files from the mithril
# test directories.
#
# The fixture files are JSON API responses from the Mithril aggregator
# (certificate list + detail format). They are used by the Phase 6 test
# module to validate certificate structure without a live aggregator.
#
# Fixture format:
#   certificate_list.json — array of certificate summary objects; each
#     item contains `hash` or `certificate_hash` (v1 API) plus metadata.
#   certificate_<hash>.json — single certificate detail object.
#
# Usage (called by regenerate.sh):
#   capture-mithril.sh \
#     --sources-toml <path/to/sources.toml> \
#     --work-dir     <scratch-dir> \
#     --tarball      <output.tar.gz>

set -euo pipefail

SOURCES_TOML="" WORK_DIR="" TARBALL=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --sources-toml) SOURCES_TOML="$2"; shift 2 ;;
        --work-dir)     WORK_DIR="$2";     shift 2 ;;
        --tarball)      TARBALL="$2";      shift 2 ;;
        *)              echo "Unknown arg: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$SOURCES_TOML" && -n "$WORK_DIR" && -n "$TARBALL" ]] || {
    echo "Usage: $0 --sources-toml <f> --work-dir <d> --tarball <t>" >&2
    exit 1
}

log() { echo "[capture-mithril] $*"; }

# ── Parse SHA from sources.toml ───────────────────────────────────────────────
SHA=$(awk '
    /^\[mithril\]/ { in_section=1; next }
    /^\[/ { in_section=0 }
    in_section && /^sha/ { match($0, /\"([0-9a-f]+)\"/, arr); print arr[1]; exit }
' "${SOURCES_TOML}")

[[ -n "$SHA" ]] || { echo "[capture-mithril] ERROR: could not parse sha from ${SOURCES_TOML}" >&2; exit 1; }
log "Using mithril SHA: ${SHA}"

# ── Clone and checkout ────────────────────────────────────────────────────────
CLONE_DIR="${WORK_DIR}/mithril-src"
log "Cloning input-output-hk/mithril (shallow, then fetch pinned SHA)..."
git clone --quiet --depth=1 "https://github.com/input-output-hk/mithril.git" "${CLONE_DIR}"
git -C "${CLONE_DIR}" fetch --quiet --depth=1 origin "${SHA}"
git -C "${CLONE_DIR}" checkout --quiet "${SHA}"
log "Checked out ${SHA}"

CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CONTENT_DIR}"

# ── Copy certificate fixture files ────────────────────────────────────────────
# mithril-test-lab/mithril-aggregator-fake/default_data/ contains static API
# response fixtures used by the fake aggregator for integration testing.
# These are real mithril certificate/snapshot JSON files with all required fields.
#
# Certificate-bearing files (Level 2/3 validation):
#   certificates-list.json   — array of certificate summaries (has hash field)
#   certificates.json        — single certificate detail (has hash + beacon + created_at)
#
# Copy all files from default_data/ — non-certificate files are accepted by
# the Phase 6 validator's "no identifier" graceful path.

FIXTURE_COUNT=0
DEFAULT_DATA_DIR="${CLONE_DIR}/mithril-test-lab/mithril-aggregator-fake/default_data"

if [[ -d "${DEFAULT_DATA_DIR}" ]]; then
    while IFS= read -r -d '' f; do
        dest_name="$(basename "$f")"
        cp "$f" "${CONTENT_DIR}/${dest_name}"
        FIXTURE_COUNT=$((FIXTURE_COUNT + 1))
        log "Copied: ${dest_name}"
    done < <(find "${DEFAULT_DATA_DIR}" -maxdepth 1 -name "*.json" -type f -print0 2>/dev/null)
else
    log "WARN: default_data directory not found at ${DEFAULT_DATA_DIR}"
fi

# Limit to the first 20 files to keep the tarball small.
# If more are needed, raise this limit in a future corpus refresh.
if [[ $FIXTURE_COUNT -gt 20 ]]; then
    log "WARN: found ${FIXTURE_COUNT} fixture files; keeping only the first 20"
    # Remove excess files (sorted, keep first 20)
    mapfile -t all_files < <(ls "${CONTENT_DIR}"/*.json 2>/dev/null | sort)
    for (( i=20; i<${#all_files[@]}; i++ )); do
        rm -f "${all_files[$i]}"
    done
    FIXTURE_COUNT=20
fi

if [[ $FIXTURE_COUNT -eq 0 ]]; then
    log "WARN: no JSON fixture files found in mithril repo; producing placeholder tarball"
    cat > "${CONTENT_DIR}/README.txt" <<'EOF'
mithril — corpus fixture placeholder

No JSON fixture files were found in the mithril repository at the pinned SHA.
The Phase 6 test module falls back to the ad-hoc fixtures in
crates/dugite-node/tests/fixtures/mithril-*.json.

To activate corpus-based fixtures, update the capture script to locate the
correct fixture paths for the pinned mithril SHA.
EOF
    echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"
else
    # ── Emit hashes ───────────────────────────────────────────────────────────
    HASHES_FILE="${WORK_DIR}/hashes.json"
    echo "{" > "${HASHES_FILE}"
    first=1
    for f in "${CONTENT_DIR}"/*.json; do
        [[ -f "$f" ]] || continue
        hash=$(sha256sum "$f" | awk '{print $1}')
        name=$(basename "$f")
        [[ $first -eq 1 ]] && first=0 || echo "," >> "${HASHES_FILE}"
        printf '  "%s": "sha256:%s"' "$name" "$hash" >> "${HASHES_FILE}"
    done
    echo "" >> "${HASHES_FILE}"
    echo "}" >> "${HASHES_FILE}"
    log "Hashes written to ${HASHES_FILE}"
fi

# ── Package tarball ───────────────────────────────────────────────────────────
tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Tarball written: ${TARBALL} (${FIXTURE_COUNT} fixture file(s))"
