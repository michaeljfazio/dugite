#!/usr/bin/env bash
# Capture step for the ouroboros-consensus area.
#
# Clones IntersectMBO/ouroboros-consensus at the SHA from sources.toml
# (shallow clone, full working tree — no blob filter), copies the
# conformance golden files, and packages them as ouroboros-consensus.tar.gz.
#
# Golden files live under ouroboros-consensus-cardano/golden/ with no
# file extension (raw binary). We capture:
#   golden/cardano/CardanoNodeToNodeVersion*/Block_*
#   golden/cardano/CardanoNodeToNodeVersion*/Header_*
#   golden/cardano/CardanoNodeToNodeVersion*/GenTx_*
#   golden/cardano/CardanoNodeToNodeVersion*/GenTxId_*
#   golden/byron/*/Block_*
#   golden/byron/*/Header_*
#   golden/byron/*/GenTx*
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

log() { echo "[capture-ouroboros-consensus] $*"; }

SHA="$(python3 - "${SOURCES_TOML}" ouroboros-consensus sha <<'EOF'
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
)"
log "SHA: ${SHA}"

CLONE_DIR="${WORK_DIR}/clone"
CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CLONE_DIR}" "${CONTENT_DIR}"

# Use depth=1 shallow clone (full working tree, no blob filter).
# We cannot use --filter=blob:none because we need the actual file content.
log "Cloning ouroboros-consensus (shallow)..."
git clone --depth=1 \
    "https://github.com/IntersectMBO/ouroboros-consensus.git" "${CLONE_DIR}"

# The SHA might not be on the default branch tip.
# If the shallow clone doesn't have the exact SHA, fetch it.
if ! git -C "${CLONE_DIR}" rev-parse --verify "${SHA}^{commit}" >/dev/null 2>&1; then
    log "SHA not at tip, fetching..."
    git -C "${CLONE_DIR}" fetch --depth=1 origin "${SHA}"
    git -C "${CLONE_DIR}" checkout "${SHA}"
fi

GOLDEN_DIR="${CLONE_DIR}/ouroboros-consensus-cardano/golden"
[[ -d "${GOLDEN_DIR}" ]] || { log "ERROR: golden dir not found at ${GOLDEN_DIR}"; exit 1; }

# Copy golden files matching our conformance patterns (no file extension).
# Patterns: Block_*, Header_*, GenTx_*, GenTxId_* under golden/cardano/ and golden/byron/
find "${GOLDEN_DIR}" -type f \( \
    -name "Block_*" \
    -o -name "Header_*" \
    -o -name "GenTx_*" \
    -o -name "GenTxId_*" \
    -o -name "GenTx" \
    -o -name "GenTxId" \
    \) | while read -r f; do
    rel="${f#${GOLDEN_DIR}/}"
    dest="${CONTENT_DIR}/${rel}"
    mkdir -p "$(dirname "${dest}")"
    cp "${f}" "${dest}"
done

COUNT="$(find "${CONTENT_DIR}" -type f | wc -l | tr -d ' ')"
log "Collected ${COUNT} golden files"

[[ "${COUNT}" -gt 0 ]] || { log "ERROR: No golden files collected — check path patterns"; exit 1; }

# Generate hashes.json
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
