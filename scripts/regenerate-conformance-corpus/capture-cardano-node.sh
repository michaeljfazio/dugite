#!/usr/bin/env bash
# Capture step for the cardano-node area.
#
# Clones IntersectMBO/cardano-node at the SHA from sources.toml and
# copies genesis spec files (alonzo-genesis.json, conway-genesis.json) and
# other test fixtures useful for conformance.
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

log() { echo "[capture-cardano-node] $*"; }

SHA="$(python3 - "${SOURCES_TOML}" cardano-node sha <<'EOF'
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

log "Cloning cardano-node (shallow)..."
git clone --depth=1 \
    "https://github.com/IntersectMBO/cardano-node.git" "${CLONE_DIR}"
if ! git -C "${CLONE_DIR}" rev-parse --verify "${SHA}^{commit}" >/dev/null 2>&1; then
    log "SHA not at tip, fetching..."
    git -C "${CLONE_DIR}" fetch --depth=1 origin "${SHA}"
    git -C "${CLONE_DIR}" checkout "${SHA}"
fi

# Collect genesis spec files used by conformance tests
for genesis_name in alonzo-genesis conway-genesis shelley-genesis byron-genesis; do
    find "${CLONE_DIR}" -name "${genesis_name}.json" -type f | while read -r f; do
        rel="${f#${CLONE_DIR}/}"
        dest="${CONTENT_DIR}/${rel}"
        mkdir -p "$(dirname "${dest}")"
        cp "${f}" "${dest}"
    done
done

# Also collect any golden/expected JSON files under test directories
find "${CLONE_DIR}" -path "*/test*" -name "*.json" -type f \
    | grep -iE "(golden|expected|spec)" \
    | while read -r f; do
    rel="${f#${CLONE_DIR}/}"
    dest="${CONTENT_DIR}/${rel}"
    mkdir -p "$(dirname "${dest}")"
    cp "${f}" "${dest}"
done

COUNT="$(find "${CONTENT_DIR}" -type f | wc -l | tr -d ' ')"
log "Collected ${COUNT} files"

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
