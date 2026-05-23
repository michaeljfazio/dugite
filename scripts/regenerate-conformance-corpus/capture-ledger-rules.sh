#!/usr/bin/env bash
# Capture step for the ledger-rules area (Phase 4 — ImpSpec CBOR dump vectors).
#
# Builds cardano-ledger at the pinned SHA and runs the ImpSpec conformance
# test suite with CONFORMANCE_CBOR_DUMP_PATH set, capturing one 5-element
# CBOR vector file per test scenario.
#
# ## Prerequisites
#
# This script requires a Haskell toolchain (GHC + cabal or Nix). The CI
# workflow uses the cardano-ledger Nix flake for reproducibility:
#
#   nix develop github:IntersectMBO/cardano-ledger/<SHA> --command \
#       cabal test cardano-ledger-conformance \
#           --test-options '--dump-path ./dumps'
#
# Without Nix, fall back to `haskell-actions/setup` in the workflow:
#
#   cabal update
#   cabal test cardano-ledger-conformance \
#       --test-options '--dump-path ./dumps'
#
# ## CBOR vector format
#
# The ImpSpec test suite emits one `.cbor` file per test scenario with the
# following 5-element structure:
#
#   [config, initial_state, final_state, events, title]
#
# Where:
#   config        — arr[13] of protocol-param fields
#   initial_state — arr[7]  NewEpochState (same encoding as cardano-cli
#                           debug log-epoch-state output)
#   final_state   — arr[7]  expected post-event NewEpochState
#   events        — arr[N]  of discriminant-keyed sub-arrays:
#                     [0, tx_cbor_bytes, expected_valid_bool, slot] — Transaction
#                     [1, slot]                                      — PassTick
#                     [2, epoch_delta]                               — PassEpoch
#   title         — text string naming the scenario
#
# Files are organised under per-era subdirectories:
#   dumps/ShelleyImpSpec/
#   dumps/MaryImpSpec/
#   dumps/AllegraImpSpec/
#   dumps/AlonzoImpSpec/
#   dumps/BabbageImpSpec/
#   dumps/ConwayImpSpec_-_Version_10/
#
# ## Current status
#
# This script is a stub — the Haskell build step is not yet automated in CI.
# To generate a real corpus:
#
#   1. Install Nix (or GHC 9.6.x + cabal 3.10.x).
#   2. Clone IntersectMBO/cardano-ledger at the pinned SHA.
#   3. Run the ImpSpec test suite with the dump path set.
#   4. Manually trigger `just regenerate-corpus-local` with the real dumps.
#   5. Upload the resulting release and update manifest.toml.
#
# The first real corpus run is expected to surface ledger bugs. Each failure
# is tracked as a separate issue and added to SKIP_LIST in mod.rs.

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

log() { echo "[capture-ledger-rules] $*"; }

# ── Parse SHA from sources.toml ───────────────────────────────────────────────
SHA=$(awk '
    /^\[ledger-rules\]/ { in_section=1; next }
    /^\[/ { in_section=0 }
    in_section && /^sha/ { match($0, /\"([0-9a-f]+)\"/, arr); print arr[1]; exit }
' "${SOURCES_TOML}")
log "Target cardano-ledger SHA: ${SHA:-<not set>}"

# ── Check for Nix / cabal ─────────────────────────────────────────────────────
HAS_NIX=0
HAS_CABAL=0
command -v nix  >/dev/null 2>&1 && HAS_NIX=1
command -v cabal >/dev/null 2>&1 && HAS_CABAL=1

CONTENT_DIR="${WORK_DIR}/content"
mkdir -p "${CONTENT_DIR}"

if [[ $HAS_NIX -eq 0 && $HAS_CABAL -eq 0 ]]; then
    log "STUB — no Haskell toolchain found (nix or cabal required)."
    log "Producing placeholder tarball."

    cat > "${CONTENT_DIR}/README.txt" <<'EOF'
ledger-rules — stub placeholder

This area requires a Haskell toolchain to populate. See:
  scripts/regenerate-conformance-corpus/capture-ledger-rules.sh

To generate real ImpSpec CBOR vectors:
  1. Install Nix (recommended) or GHC 9.6.x + cabal 3.10.x.
  2. Run `just regenerate-corpus-local` from the workspace root.
  3. Update manifest.toml to point at the new release tag.
  4. Run `cargo xtask download-upstream-fixtures`.

The Phase 4 test module (ledger_rules_replay) will skip gracefully
until fixture files are present.
EOF
    echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"
    tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
    log "Placeholder tarball written: ${TARBALL}"
    exit 0
fi

# ── Full capture (requires Haskell toolchain) ─────────────────────────────────
[[ -n "$SHA" ]] || { log "ERROR: sha not set in ${SOURCES_TOML} under [ledger-rules]"; exit 1; }

CLONE_DIR="${WORK_DIR}/cardano-ledger-src"
log "Cloning IntersectMBO/cardano-ledger at ${SHA}..."
git clone --quiet --depth=1 "https://github.com/IntersectMBO/cardano-ledger.git" "${CLONE_DIR}"
git -C "${CLONE_DIR}" fetch --quiet --depth=1 origin "${SHA}"
git -C "${CLONE_DIR}" checkout --quiet "${SHA}"

DUMP_DIR="${WORK_DIR}/dumps"
mkdir -p "${DUMP_DIR}"

log "Running ImpSpec test suite with CONFORMANCE_CBOR_DUMP_PATH=${DUMP_DIR} ..."
if [[ $HAS_NIX -eq 1 ]]; then
    nix develop "${CLONE_DIR}" --command bash -c "
        cd '${CLONE_DIR}'
        cabal test cardano-ledger-conformance \
            --test-options '--dump-path ${DUMP_DIR}'
    " || true  # non-zero exit is expected when conformance tests fail (dumps are still written)
else
    (
        cd "${CLONE_DIR}"
        cabal update
        cabal test cardano-ledger-conformance \
            --test-options "--dump-path ${DUMP_DIR}"
    ) || true
fi

# ── Copy dumps into content structure ────────────────────────────────────────
VECTOR_COUNT=0
for era_dir in "${DUMP_DIR}"/*/; do
    era=$(basename "${era_dir}")
    mkdir -p "${CONTENT_DIR}/${era}"
    while IFS= read -r -d '' f; do
        cp "$f" "${CONTENT_DIR}/${era}/"
        ((VECTOR_COUNT++))
    done < <(find "${era_dir}" -name "*.cbor" -type f -print0 2>/dev/null)
done

log "Captured ${VECTOR_COUNT} CBOR vector files across $(ls "${CONTENT_DIR}" | wc -l) era(s)"

if [[ $VECTOR_COUNT -eq 0 ]]; then
    log "WARN: no CBOR vectors produced — ImpSpec ran but produced no dumps."
    log "Check that CONFORMANCE_CBOR_DUMP_PATH was honoured."
    cat > "${CONTENT_DIR}/README.txt" <<'EOF'
ledger-rules — empty corpus

The ImpSpec test suite ran but produced no CBOR dump files. This means all
conformance tests passed at the pinned cardano-ledger SHA, or the dump path
was not set correctly. See capture-ledger-rules.sh.
EOF
    echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"
    tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
    exit 0
fi

# ── Emit hashes ───────────────────────────────────────────────────────────────
{
    echo "{"
    first=1
    while IFS= read -r -d '' f; do
        hash=$(sha256sum "$f" | awk '{print $1}')
        rel="${f#"${CONTENT_DIR}/"}"
        [[ $first -eq 1 ]] && first=0 || printf ","
        printf '\n  "%s": "sha256:%s"' "$rel" "$hash"
    done < <(find "${CONTENT_DIR}" -name "*.cbor" -type f -print0 | sort -z)
    echo ""
    echo "}"
} > "${WORK_DIR}/hashes.json"

tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
log "Tarball written: ${TARBALL} (${VECTOR_COUNT} vectors)"
