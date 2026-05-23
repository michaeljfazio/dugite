#!/usr/bin/env bash
# Capture step for the cardano-base area (Phase 5 — VRF test vectors).
#
# Clones IntersectMBO/cardano-base at the SHA pinned in sources.toml and
# copies the VRF crypto test-vector files from:
#   cardano-crypto-praos/test_vectors/
#
# Each file is a single VRF test vector in key:value format (no .txt extension):
#   vrf: <identifier>
#   ver: ietfdraft03   # or ietfdraft13 (batch-compatible; dugite skips these)
#   ciphersuite: ECVRF-ED25519-SHA512-ELL2
#   sk: <32-byte-seed-hex>
#   pk: <32-byte-pubkey-hex>
#   alpha: <variable-hex>   # may be empty
#   pi: <80-byte-hex>       # v03 proof; 128 bytes for v13
#   beta: <64-byte-hex>     # VRF output
#
# v03 (ECVRF-ED25519-SHA512-Elligator2 draft-03) vectors are validated by
# the Phase 5 test module; v13 batch-compatible vectors are skipped with an
# explanatory message (Cardano Praos uses v03).
#
# KES note: cardano-base uses property-based testing for KES, not static
# vector files. There are no KES fixture files to copy.
#
# Usage (called by regenerate.sh):
#   capture-cardano-base.sh \
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

log() { echo "[capture-cardano-base] $*"; }

# ── Parse SHA from sources.toml ───────────────────────────────────────────────
# Expected entry:
#   [cardano-base]
#   repo = "IntersectMBO/cardano-base"
#   sha  = "<40-char-hex>"

SHA=$(awk '
    /^\[cardano-base\]/ { in_section=1; next }
    /^\[/ { in_section=0 }
    in_section && /^sha/ { match($0, /\"([0-9a-f]+)\"/, arr); print arr[1]; exit }
' "${SOURCES_TOML}")

[[ -n "$SHA" ]] || { echo "[capture-cardano-base] ERROR: could not parse sha from ${SOURCES_TOML}" >&2; exit 1; }
log "Using cardano-base SHA: ${SHA}"

# ── Clone and checkout ────────────────────────────────────────────────────────
CLONE_DIR="${WORK_DIR}/cardano-base-src"
log "Cloning IntersectMBO/cardano-base (shallow, then fetch pinned SHA)..."
git clone --quiet --depth=1 "https://github.com/IntersectMBO/cardano-base.git" "${CLONE_DIR}"
git -C "${CLONE_DIR}" fetch --quiet --depth=1 origin "${SHA}"
git -C "${CLONE_DIR}" checkout --quiet "${SHA}"
log "Checked out ${SHA}"

# ── Copy VRF test vectors ─────────────────────────────────────────────────────
VECTORS_SRC="${CLONE_DIR}/cardano-crypto-praos/test_vectors"
CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CONTENT_DIR}"

if [[ ! -d "${VECTORS_SRC}" ]]; then
    log "ERROR: test_vectors directory not found at ${VECTORS_SRC}" >&2
    exit 1
fi

VRF_COUNT=0
# cardano-base stores vector files without any extension (e.g., vrf_ver03_generated_1).
# The glob matches all files whose name starts with "vrf" regardless of extension.
for f in "${VECTORS_SRC}"/vrf*; do
    [[ -f "$f" ]] || continue
    cp "$f" "${CONTENT_DIR}/"
    VRF_COUNT=$((VRF_COUNT + 1))
    log "Copied: $(basename "$f")"
done

log "Copied ${VRF_COUNT} VRF vector file(s)"

if [[ $VRF_COUNT -eq 0 ]]; then
    log "ERROR: no VRF vector files found in ${VECTORS_SRC}" >&2
    exit 1
fi

# ── Emit hashes ───────────────────────────────────────────────────────────────
HASHES_FILE="${WORK_DIR}/hashes.json"
echo "{" > "${HASHES_FILE}"
first=1
# Hash all files matching vrf* (no extension required).
for f in "${CONTENT_DIR}"/vrf*; do
    [[ -f "$f" ]] || continue
    hash=$(sha256sum "$f" | awk '{print $1}')
    name=$(basename "$f")
    [[ $first -eq 1 ]] && first=0 || echo "," >> "${HASHES_FILE}"
    printf '  "%s": "sha256:%s"' "$name" "$hash" >> "${HASHES_FILE}"
done
echo "" >> "${HASHES_FILE}"
echo "}" >> "${HASHES_FILE}"

log "Hashes written to ${HASHES_FILE}"

# ── Package tarball ───────────────────────────────────────────────────────────
tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Tarball written: ${TARBALL} (${VRF_COUNT} vector files)"
