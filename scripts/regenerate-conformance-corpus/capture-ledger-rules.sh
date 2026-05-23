#!/usr/bin/env bash
# Capture step for the ledger-rules area (Phase 4 — standalone Haskell fixture generator).
#
# ## Approach: dugite-fixture-gen (Option A)
#
# This script builds and runs `tools/ledger-fixture-gen/` — a small Haskell
# executable that instantiates Conway STS rules with known inputs, runs the
# Haskell STS transitions, and emits 4 CBOR files per test case.
#
# This replaces the ImpSpec approach (CONFORMANCE_CBOR_DUMP_PATH) which only
# fires on Haskell/Agda divergences and produces ZERO files at a stable SHA.
# Confirmed by oracle research on SHA ebed62de1ebcd4b13512418d49d17802a193e2c1.
#
# ## Generator build strategy
#
# The generator is compiled INSIDE the cardano-ledger workspace (cloned at the
# pinned SHA) by adding it as a sub-package to cabal.project.  This guarantees
# that all cardano-ledger-* dependency versions match exactly — no separate
# cabal freeze file or version negotiation needed.
#
# ## Output layout (5 files per test-case directory; st_out is optional)
#
#   content/ConwayNEWEPOCH/<test_name>/conformance_dump_ctx.cbor     — CBOR null (0xF6)
#   content/ConwayNEWEPOCH/<test_name>/conformance_dump_env.cbor     — CBOR null (0xF6)
#   content/ConwayNEWEPOCH/<test_name>/conformance_dump_st.cbor      — NewEpochState array(7)
#   content/ConwayNEWEPOCH/<test_name>/conformance_dump_sig.cbor     — EpochNo (CBOR uint)
#   content/ConwayNEWEPOCH/<test_name>/conformance_dump_st_out.cbor  — Haskell final state (if STS succeeded)
#
# ctx/env are CBOR null (0xF6) because the NEWEPOCH rule uses () for both,
# and Haskell's `EncCBOR ()` instance is `encodeNull`.
# st_out is absent when the STS rule rejects the transition (e.g. signal == initial_epoch).
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
ledger-rules — stub placeholder (no Haskell toolchain found)

This area requires a Haskell toolchain (GHC 9.6.x + cabal 3.10.x) to populate.
See: scripts/regenerate-conformance-corpus/capture-ledger-rules.sh

To generate real CBOR conformance vectors:
  1. Install GHC 9.6.x + cabal 3.10.x (or Nix).
  2. Run `just regenerate-corpus-local` from the workspace root.
  3. Update manifest.toml to point at the new release tag.
  4. Run `cargo xtask download-upstream-fixtures`.

The generator (tools/ledger-fixture-gen/) builds as a sub-package inside
the cloned cardano-ledger workspace and produces Conway NEWEPOCH fixtures.

Vector format: 5 files per test-case directory (st_out is optional)
  ConwayNEWEPOCH/<test_name>/conformance_dump_ctx.cbor     — CBOR null (0xF6, EncCBOR ())
  ConwayNEWEPOCH/<test_name>/conformance_dump_env.cbor     — CBOR null (0xF6, EncCBOR ())
  ConwayNEWEPOCH/<test_name>/conformance_dump_st.cbor      — NewEpochState array(7)
  ConwayNEWEPOCH/<test_name>/conformance_dump_sig.cbor     — EpochNo (CBOR uint)
  ConwayNEWEPOCH/<test_name>/conformance_dump_st_out.cbor  — Haskell final state (if STS succeeded)

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

# ── Set up the Dugite fixture generator inside the cardano-ledger workspace ───
#
# We add the generator as a cabal sub-package inside the cloned cardano-ledger
# workspace.  This guarantees that all cardano-ledger-* dependency versions
# resolve automatically from the workspace's own cabal.project — no separate
# freeze file or manual version pinning needed.

GENERATOR_DIR="${CLONE_DIR}/dugite-fixture-gen"
mkdir -p "${GENERATOR_DIR}/src"
log "Installing Dugite fixture generator into ${GENERATOR_DIR}..."

# Locate our generator source files relative to the script.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
GENERATOR_SRC="${REPO_ROOT}/tools/ledger-fixture-gen"

if [[ ! -f "${GENERATOR_SRC}/dugite-fixture-gen.cabal" ]]; then
    log "ERROR: ${GENERATOR_SRC}/dugite-fixture-gen.cabal not found"
    log "       tools/ledger-fixture-gen/ must be present in the repo"
    exit 1
fi

cp "${GENERATOR_SRC}/dugite-fixture-gen.cabal" "${GENERATOR_DIR}/"
cp "${GENERATOR_SRC}/src/Main.hs"              "${GENERATOR_DIR}/src/"

# Add the generator to cardano-ledger's cabal.project.
echo "packages: dugite-fixture-gen/" >> "${CLONE_DIR}/cabal.project"
log "Added dugite-fixture-gen/ to cabal.project"

# Build the generator using cabal (deps resolve against cardano-ledger's project).
log "Building Dugite fixture generator (this resolves against cardano-ledger deps)..."
if [[ $HAS_NIX -eq 1 ]]; then
    nix develop "${CLONE_DIR}" --command bash -c "
        cd '${CLONE_DIR}'
        cabal update
        cabal build dugite-fixture-gen 2>&1 | tail -100
    "
else
    (
        cd "${CLONE_DIR}"
        cabal update
        cabal build dugite-fixture-gen 2>&1 | tail -100
    )
fi

# Run the generator to produce CBOR fixture files.
DUMP_DIR="${WORK_DIR}/dumps"
mkdir -p "${DUMP_DIR}"
log "Running Dugite fixture generator → ${DUMP_DIR}..."
if [[ $HAS_NIX -eq 1 ]]; then
    nix develop "${CLONE_DIR}" --command bash -c "
        cd '${CLONE_DIR}'
        cabal run dugite-fixture-gen -- --output-dir '${DUMP_DIR}'
    "
else
    (
        cd "${CLONE_DIR}"
        cabal run dugite-fixture-gen -- --output-dir "${DUMP_DIR}"
    )
fi

# ── Mirror test-case directories into content structure ───────────────────────
#
# The generator writes:
#   <DUMP_DIR>/ConwayNEWEPOCH/<test_name>/conformance_dump_{ctx,env,st,sig}.cbor
#   <DUMP_DIR>/ConwayNEWEPOCH/<test_name>/conformance_dump_st_out.cbor  (optional)
#
# We copy each test-case directory verbatim into CONTENT_DIR so the
# Rust test module can scan subdirectories and find all required files.
# The optional st_out file is copied when present.
#
REQUIRED_FILES=(
    "conformance_dump_ctx.cbor"
    "conformance_dump_env.cbor"
    "conformance_dump_st.cbor"
    "conformance_dump_sig.cbor"
)

# Optional file: present when the STS rule accepted the transition.
OPTIONAL_FILES=(
    "conformance_dump_st_out.cbor"
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

        # Copy all 4 required files into the content tree.
        dest="${CONTENT_DIR}/${rule}/${test_name}"
        mkdir -p "${dest}"
        for req in "${REQUIRED_FILES[@]}"; do
            cp "${test_case_dir}/${req}" "${dest}/"
        done

        # Copy optional files (st_out) when present.
        for opt in "${OPTIONAL_FILES[@]}"; do
            if [[ -f "${test_case_dir}/${opt}" ]]; then
                cp "${test_case_dir}/${opt}" "${dest}/"
            fi
        done

        ((TEST_CASE_COUNT++))
        rule_found=1
    done

    [[ $rule_found -eq 1 ]] && ((RULE_COUNT++))
done

log "Captured ${TEST_CASE_COUNT} test cases across ${RULE_COUNT} rule(s)"

if [[ $TEST_CASE_COUNT -eq 0 ]]; then
    log "ERROR: generator ran but produced no 4-file test-case directories under ${DUMP_DIR}."
    log "       Check the generator output above for compilation or runtime errors."
    exit 1
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
