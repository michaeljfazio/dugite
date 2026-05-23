#!/usr/bin/env bash
# Capture step for the ledger-rules area (Phase 4 — ImpSpec CBOR dump vectors).
#
# ## IMPORTANT: ImpSpec dump semantics
#
# The ImpSpec framework only produces CBOR dump files when the Haskell ledger
# implementation DIVERGES from the Agda formal spec.  Confirmed by oracle
# research on SHA ebed62de1ebcd4b13512418d49d17802a193e2c1, function
# `checkConformance` in:
#   libs/cardano-ledger-conformance/src/Test/Cardano/Ledger/Conformance/ExecSpecRule/Core.hs
#
# Logic:
#   - Haskell result == Agda result  →  pure ()   (no dump)
#   - Both fail                      →  pure ()   (no dump)
#   - They diverge                   →  dump fires (writes 4 files to CONFORMANCE_CBOR_DUMP_PATH)
#
# At the stable pinned SHA the reference implementation passes all its own
# ImpSpec tests.  Therefore: running `CONFORMANCE_CBOR_DUMP_PATH=/path cabal
# test cardano-ledger-conformance` produces ZERO dump files.
#
# This script uses the correct env-var mechanism (not the non-existent
# `--dump-path` CLI flag).  At the pinned SHA the dump directory will be
# empty; the stub-fallback below handles this gracefully.
#
# See HANDOFF.md for the full analysis and alternative corpus generation
# approaches that the product owner must decide among.
#
# ## Output layout (4 files per test-case directory, when dumps are produced)
#
#   content/<Rule>/<test_name>/conformance_dump_ctx.cbor   — ExecContext
#   content/<Rule>/<test_name>/conformance_dump_env.cbor   — Environment
#   content/<Rule>/<test_name>/conformance_dump_st.cbor    — State (NewEpochState array(7))
#   content/<Rule>/<test_name>/conformance_dump_sig.cbor   — Signal
#
# Where <Rule> is the ledger rule name (e.g. ConwayNEWEPOCH, ConwayUTXO) and
# <test_name> is the ImpSpec test scenario name.
#
# ## Current status
#
# This script is a stub. Phase 4 corpus generation requires a redesigned
# approach because ImpSpec only dumps on divergence. See HANDOFF.md for
# Option A (standalone Haskell fixture generator — recommended), Option B
# (QuickCheck generator), Option C (Agda/MAlonzo direct), Option D
# (hand-crafted vectors).
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
command -v nix   >/dev/null 2>&1 && HAS_NIX=1
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

Vector format: 4 files per test-case directory
  <Rule>/<test_name>/conformance_dump_ctx.cbor  — ExecContext
  <Rule>/<test_name>/conformance_dump_env.cbor  — Environment
  <Rule>/<test_name>/conformance_dump_st.cbor   — State (NewEpochState array(7))
  <Rule>/<test_name>/conformance_dump_sig.cbor  — Signal

The Phase 4 test module (ledger_rules_replay) will skip gracefully
until real fixture directories are present (only the synthetic
ConwayNEWEPOCH/test_minimal_epoch_advance fixture runs in stub mode).
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
log "NOTE: At the stable pinned SHA the reference impl passes all its own ImpSpec tests."
log "      Dumps only fire on Haskell/Agda divergences — expect ZERO files in ${DUMP_DIR}."
log "      See HANDOFF.md for the Phase 4 redesign decision."
if [[ $HAS_NIX -eq 1 ]]; then
    nix develop "${CLONE_DIR}" --command bash -c "
        cd '${CLONE_DIR}'
        CONFORMANCE_CBOR_DUMP_PATH='${DUMP_DIR}' \
            cabal test cardano-ledger-conformance \
            --test-show-details=streaming
    " || true  # non-zero exit OK; dumps are written only on divergence
else
    (
        cd "${CLONE_DIR}"
        cabal update
        CONFORMANCE_CBOR_DUMP_PATH="${DUMP_DIR}" \
            cabal test cardano-ledger-conformance \
            --test-show-details=streaming
    ) || true
fi

# ── Mirror 4-file test-case directories into content structure ────────────────
#
# The ImpSpec framework emits:
#   <DUMP_DIR>/<Rule>/<test_name>/conformance_dump_{ctx,env,st,sig}.cbor
#
# We copy each 4-file test-case directory verbatim into CONTENT_DIR so the
# Rust test module can scan subdirectories and find all 4 files.
#
REQUIRED_FILES=(
    "conformance_dump_ctx.cbor"
    "conformance_dump_env.cbor"
    "conformance_dump_st.cbor"
    "conformance_dump_sig.cbor"
)

TEST_CASE_COUNT=0
RULE_COUNT=0

for rule_dir in "${DUMP_DIR}"/*/; do
    [[ -d "${rule_dir}" ]] || continue
    rule=$(basename "${rule_dir}")
    rule_found=0

    for test_case_dir in "${rule_dir}"*/; do
        [[ -d "${test_case_dir}" ]] || continue
        test_name=$(basename "${test_case_dir}")

        # Verify all 4 required files are present.
        all_present=1
        for req in "${REQUIRED_FILES[@]}"; do
            if [[ ! -f "${test_case_dir}/${req}" ]]; then
                log "WARN: ${rule}/${test_name}: missing ${req} — skipping"
                all_present=0
                break
            fi
        done
        [[ $all_present -eq 0 ]] && continue

        # Copy all 4 files into the content tree.
        dest="${CONTENT_DIR}/${rule}/${test_name}"
        mkdir -p "${dest}"
        for req in "${REQUIRED_FILES[@]}"; do
            cp "${test_case_dir}/${req}" "${dest}/"
        done
        ((TEST_CASE_COUNT++))
        rule_found=1
    done

    [[ $rule_found -eq 1 ]] && ((RULE_COUNT++))
done

log "Captured ${TEST_CASE_COUNT} test cases across ${RULE_COUNT} rule(s)"

if [[ $TEST_CASE_COUNT -eq 0 ]]; then
    log "INFO: no 4-file test-case directories found under ${DUMP_DIR}."
    log "This is EXPECTED at the stable pinned SHA: ImpSpec dumps only on Haskell/Agda"
    log "divergences, which never occur at the validated reference implementation."
    log "Phase 4 corpus generation requires a redesigned approach. See HANDOFF.md."
    cat > "${CONTENT_DIR}/README.txt" <<'EOF'
ledger-rules — empty corpus (expected at stable pinned SHA)

The ImpSpec test suite ran but produced no dump files.  This is expected:
CONFORMANCE_CBOR_DUMP_PATH only fires when the Haskell ledger implementation
diverges from the Agda formal spec.  At the stable pinned SHA the reference
implementation passes all its own ImpSpec tests, so no dumps are produced.

Phase 4 requires a redesigned capture approach.  See HANDOFF.md for options:
  Option A: Standalone Haskell fixture generator (recommended)
  Option B: QuickCheck-based fixture generator
  Option C: Agda/MAlonzo direct invocation
  Option D: Hand-crafted CBOR vectors
EOF
    echo '{"__stub__": true}' > "${WORK_DIR}/hashes.json"
    tar -czf "${TARBALL}" -C "${CONTENT_DIR}" .
    exit 0
fi

# ── Emit hashes (one entry per CBOR file) ────────────────────────────────────
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
log "Tarball written: ${TARBALL} (${TEST_CASE_COUNT} test cases, ${RULE_COUNT} rules)"
