#!/usr/bin/env bash
# Capture step for the ouroboros-consensus area.
#
# Clones IntersectMBO/ouroboros-consensus at the SHA from sources.toml,
# copies the conformance golden files, and packages them as ouroboros-consensus.tar.gz.
#
# Produces:
#   <work-dir>/hashes.json       — file_name → sha256 map (consumed by orchestrator)
#   <tarball>                    — the area tarball

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

# Clone at pinned SHA (shallow + deepen to reach the target commit)
log "Cloning ouroboros-consensus..."
git clone --no-checkout --filter=blob:none \
    "https://github.com/IntersectMBO/ouroboros-consensus.git" "${CLONE_DIR}"
git -C "${CLONE_DIR}" fetch --depth=1 origin "${SHA}"
git -C "${CLONE_DIR}" checkout "${SHA}"

# Locate conformance golden files.
# ouroboros-consensus ships block/header/gentx golden files under:
#   ouroboros-consensus-cardano/test/Test/Consensus/Cardano/Golden/
# and serialisation roundtrip fixtures under various test trees.
find "${CLONE_DIR}" \( \
    -path "*/Golden/Block_*" \
    -o -path "*/Golden/Header_*" \
    -o -path "*/Golden/GenTx_*" \
    -o -path "*/Golden/GenTxId_*" \
    -o -name "*.golden" \
    \) -type f | while read -r f; do
    # Preserve relative path from clone root
    rel="${f#${CLONE_DIR}/}"
    dest="${CONTENT_DIR}/${rel}"
    mkdir -p "$(dirname "${dest}")"
    cp "${f}" "${dest}"
done

COUNT="$(find "${CONTENT_DIR}" -type f | wc -l | tr -d ' ')"
log "Collected ${COUNT} golden files"

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

# Package
tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Tarball written: ${TARBALL}"
