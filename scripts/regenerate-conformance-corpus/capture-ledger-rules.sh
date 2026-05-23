#!/usr/bin/env bash
# Capture step for the ledger-rules area (Phase 4 — ImpSpec CBOR dump vectors).
#
# Builds cardano-ledger at the pinned SHA and runs the ImpSpec conformance
# test suite with CONFORMANCE_CBOR_DUMP_PATH set.  The ImpSpec framework
# produces **4 separate CBOR files per diverging test case**, organised as:
#
#   <dump_path>/<Rule>/<test_name>/conformance_dump_ctx.cbor
#   <dump_path>/<Rule>/<test_name>/conformance_dump_env.cbor
#   <dump_path>/<Rule>/<test_name>/conformance_dump_st.cbor
#   <dump_path>/<Rule>/<test_name>/conformance_dump_sig.cbor
#
# This script mirrors that layout verbatim into the output tarball so that
# `tests/conformance/src/upstream/ledger_rules_replay/mod.rs` can scan
# subdirectories and decode each test case from its 4 files.
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
# ## Output layout (4 files per test-case directory)
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
# This script is a stub — the Haskell build step is not yet automated in CI.
# To generate a real corpus:
#
#   1. Install Nix (recommended) or GHC 9.6.x + cabal 3.10.x.
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
if [[ $HAS_NIX -eq 1 ]]; then
    nix develop "${CLONE_DIR}" --command bash -c "
        cd '${CLONE_DIR}'
        cabal test cardano-ledger-conformance \
            --test-options '--dump-path ${DUMP_DIR}'
    " || true  # non-zero exit expected when conformance tests fail (dumps are still written)
else
    (
        cd "${CLONE_DIR}"
        cabal update
        cabal test cardano-ledger-conformance \
            --test-options "--dump-path ${DUMP_DIR}"
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
    log "WARN: no 4-file test-case directories found under ${DUMP_DIR}."
    log "Check that CONFORMANCE_CBOR_DUMP_PATH was honoured by the ImpSpec suite."
    cat > "${CONTENT_DIR}/README.txt" <<'EOF'
ledger-rules — empty corpus

The ImpSpec test suite ran but produced no 4-file test-case directories.
This means all conformance tests passed at the pinned cardano-ledger SHA,
or CONFORMANCE_CBOR_DUMP_PATH was not set correctly.
See capture-ledger-rules.sh.
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
