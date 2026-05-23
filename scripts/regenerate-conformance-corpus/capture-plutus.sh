#!/usr/bin/env bash
# Capture step for the plutus area.
#
# Downloads the plutus-conformance.tar.gz release asset from IntersectMBO/plutus
# at the tag specified in sources.toml and repackages it as plutus.tar.gz.
# Upstream naming: plutus-conformance.tar.gz → republished as: plutus.tar.gz
#
# Produces:
#   <work-dir>/hashes.json
#   <tarball>

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
[[ -n "${SOURCES_TOML}" && -n "${WORK_DIR}" && -n "${TARBALL}" ]] || { echo "Missing args" >&2; exit 1; }

log() { echo "[capture-plutus] $*"; }

parse_val() {
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
        in_section = (line.strip('[]').strip() == section)
        continue
    if in_section and re.match(rf'^{key}\s*=', line):
        m = re.search(r'"([^"]+)"', line)
        if m: print(m.group(1)); sys.exit(0)
sys.exit(1)
EOF
}

TAG="$(parse_val plutus tag)"
UPSTREAM_ASSET="$(parse_val plutus release_asset)"
log "Tag: ${TAG}, asset: ${UPSTREAM_ASSET}"

DOWNLOAD_URL="https://github.com/IntersectMBO/plutus/releases/download/${TAG}/${UPSTREAM_ASSET}"
CONTENT_DIR="${WORK_DIR}/content"
UPSTREAM_TARBALL="${WORK_DIR}/upstream.tar.gz"
mkdir -p "${CONTENT_DIR}"

log "Downloading ${DOWNLOAD_URL}..."
CURL_ARGS=(-L -o "${UPSTREAM_TARBALL}")
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    CURL_ARGS+=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
fi
curl "${CURL_ARGS[@]}" "${DOWNLOAD_URL}"

log "Extracting..."
tar -xzf "${UPSTREAM_TARBALL}" -C "${CONTENT_DIR}"

COUNT="$(find "${CONTENT_DIR}" -type f | wc -l | tr -d ' ')"
log "Extracted ${COUNT} files"

python3 - "${CONTENT_DIR}" > "${WORK_DIR}/hashes.json" <<'EOF'
import sys, os, hashlib, json
base = sys.argv[1]
hashes = {}
for root, _, files in os.walk(base):
    for fn in files:
        path = os.path.join(root, fn)
        rel = os.path.relpath(path, base)
        sha = hashlib.sha256(open(path, 'rb').read()).hexdigest()
        hashes[rel] = f'sha256:{sha}'
print(json.dumps(hashes, indent=2))
EOF

tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Tarball written: ${TARBALL}"
