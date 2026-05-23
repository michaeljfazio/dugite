#!/usr/bin/env bash
# Orchestrator for the dugite upstream conformance corpus regeneration pipeline.
#
# Usage:
#   regenerate.sh               # full run — clones, captures, publishes a GitHub release
#   regenerate.sh --local       # local run — produces tarballs in target/conformance-corpus/
#   regenerate.sh --area NAME   # only run one area's capture step (implies --local)
#
# Inputs:
#   tests/conformance/upstream/sources.toml  — upstream SHA/tag pins
#
# Outputs (full run):
#   A new dugite GitHub release tagged conformance-corpus-v<TIMESTAMP> with:
#     <area>.tar.gz × 7 + corpus-manifest.json
#
# Outputs (local run):
#   target/conformance-corpus/<area>.tar.gz × 7 + corpus-manifest.json
#
# Environment:
#   GITHUB_TOKEN  — required for full run (gh release create)
#   GH_REPO       — override repo for release (default: michaeljfazio/dugite)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCES_TOML="${REPO_ROOT}/tests/conformance/upstream/sources.toml"

LOCAL=false
SINGLE_AREA=""
GH_REPO="${GH_REPO:-michaeljfazio/dugite}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --local)    LOCAL=true; shift ;;
        --area)     SINGLE_AREA="$2"; LOCAL=true; shift 2 ;;
        *)          echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

TIMESTAMP="$(date -u +%Y%m%d-%H%M%S)"
RELEASE_TAG="conformance-corpus-v${TIMESTAMP}"
OUT_DIR="${REPO_ROOT}/target/conformance-corpus/${RELEASE_TAG}"
WORK_DIR="${OUT_DIR}/work"
TARBALLS_DIR="${OUT_DIR}/tarballs"

mkdir -p "${WORK_DIR}" "${TARBALLS_DIR}"

log() { echo "[regenerate] $*"; }
die() { echo "[regenerate] ERROR: $*" >&2; exit 1; }

# Parse a scalar value from sources.toml: parse_toml_value SECTION KEY
parse_toml_value() {
    local section="$1" key="$2"
    python3 - "${SOURCES_TOML}" "${section}" "${key}" <<'EOF'
import sys, re

toml_file, section, key = sys.argv[1], sys.argv[2], sys.argv[3]
with open(toml_file) as f:
    content = f.read()

in_section = False
for line in content.splitlines():
    line = line.strip()
    if line.startswith('[') and not line.startswith('[['):
        current = line.strip('[]').strip()
        in_section = (current == section)
        continue
    if in_section and line.startswith(key + ' ') or (in_section and line.startswith(key + '=')):
        m = re.match(r'[^=]+=\s*"([^"]+)"', line)
        if m:
            print(m.group(1))
            sys.exit(0)

sys.exit(1)
EOF
}

ALL_AREAS=(ouroboros-consensus cardano-ledger cardano-node plutus ledger-rules cardano-base mithril)

if [[ -n "${SINGLE_AREA}" ]]; then
    AREAS=("${SINGLE_AREA}")
else
    AREAS=("${ALL_AREAS[@]}")
fi

MANIFEST_AREAS_JSON=""

run_capture() {
    local area="$1"
    local capture_script="${SCRIPT_DIR}/capture-${area}.sh"

    [[ -f "${capture_script}" ]] || die "Missing capture script: ${capture_script}"

    local area_work="${WORK_DIR}/${area}"
    local area_tarball="${TARBALLS_DIR}/${area}.tar.gz"

    mkdir -p "${area_work}"

    log "=== Capturing area: ${area} ==="
    bash "${capture_script}" \
        --sources-toml "${SOURCES_TOML}" \
        --work-dir     "${area_work}" \
        --tarball      "${area_tarball}"

    log "Area ${area}: tarball written to ${area_tarball}"

    # Per-area hashes.json is produced by the capture script in area_work.
    local hashes_file="${area_work}/hashes.json"
    local area_json
    if [[ -f "${hashes_file}" ]]; then
        local file_hashes
        file_hashes="$(cat "${hashes_file}")"
        local file_count
        file_count="$(python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d))" <<< "${file_hashes}" 2>/dev/null || echo 0)"
        local is_stub
        is_stub="$(python3 -c "import json,sys; d=json.load(sys.stdin); print('true' if d.get('__stub__') else 'false')" <<< "${file_hashes}" 2>/dev/null || echo false)"
        area_json="\"${area}\": {\"asset\": \"${area}.tar.gz\", \"file_count\": ${file_count}, \"stub\": ${is_stub}, \"file_hashes\": ${file_hashes}}"
    else
        area_json="\"${area}\": {\"asset\": \"${area}.tar.gz\", \"file_count\": 0, \"stub\": true}"
    fi

    if [[ -z "${MANIFEST_AREAS_JSON}" ]]; then
        MANIFEST_AREAS_JSON="${area_json}"
    else
        MANIFEST_AREAS_JSON="${MANIFEST_AREAS_JSON}, ${area_json}"
    fi
}

for area in "${AREAS[@]}"; do
    run_capture "${area}"
done

# Write corpus-manifest.json
MANIFEST_JSON="{\"release_tag\": \"${RELEASE_TAG}\", \"generated_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\", \"areas\": {${MANIFEST_AREAS_JSON}}}"
echo "${MANIFEST_JSON}" | python3 -m json.tool > "${TARBALLS_DIR}/corpus-manifest.json"
log "corpus-manifest.json written"

if ${LOCAL}; then
    log "Local run complete. Artefacts in: ${TARBALLS_DIR}"
    exit 0
fi

# Full run: create GitHub release
[[ -n "${GITHUB_TOKEN:-}" ]] || die "GITHUB_TOKEN must be set for full run"

RELEASE_BODY="## Conformance corpus ${RELEASE_TAG}

Generated at $(date -u +%Y-%m-%dT%H:%M:%SZ) by the \`regenerate-conformance-corpus\` workflow.

### Upstream pins
$(python3 - "${SOURCES_TOML}" <<'PYEOF'
import re, sys
with open(sys.argv[1]) as f:
    content = f.read()
section = None
for line in content.splitlines():
    line = line.strip()
    if line.startswith('[') and not line.startswith('[['):
        section = line.strip('[]').strip()
        print(f'\n**[{section}]**')
    elif section and (line.startswith('sha') or line.startswith('tag')):
        print(f'  {line}')
PYEOF
)

### Assets
$(for a in "${ALL_AREAS[@]}"; do echo "- \`${a}.tar.gz\`"; done)
- \`corpus-manifest.json\`
"

GH_TOKEN="${GITHUB_TOKEN}" gh release create "${RELEASE_TAG}" \
    --repo "${GH_REPO}" \
    --title "Conformance corpus ${RELEASE_TAG}" \
    --notes "${RELEASE_BODY}" \
    "${TARBALLS_DIR}"/*.tar.gz \
    "${TARBALLS_DIR}/corpus-manifest.json"

log "GitHub release created: https://github.com/${GH_REPO}/releases/tag/${RELEASE_TAG}"
