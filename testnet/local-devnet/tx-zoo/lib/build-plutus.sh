#!/usr/bin/env bash
# Build / vendor the Plutus binaries the zoo needs.
#
# Strategy:
#   1. If `aiken` is on PATH, compile minimal "always-true" validators for
#      Plutus V1/V2/V3 (preferred — uses the canonical Plutus toolchain).
#   2. Otherwise, write vendored "always-true" cborHex bytes for V1/V2 and
#      a best-effort V3 candidate. These come from the canonical IOG
#      cardano-node integration test fixtures (V1, V2) and a hand-built
#      V3 validator. If V3 spend/mint scripts fail on submit, install
#      aiken and rerun this script — the V3 wire shape changed several
#      times during Conway development.
#
# Output: $ZOO_LIB/plutus/{always-true-v1,always-true-v2,always-true-v3}.plutus
# Each is a JSON file matching cardano-cli's --tx-out-script-file expectation.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/tx-zoo-common.sh"

PLUTUS_DIR="$ZOO_LIB/plutus"
mkdir -p "$PLUTUS_DIR"

write_vendored() {
    cat > "$PLUTUS_DIR/always-true-v1.plutus" <<'EOF'
{
    "type": "PlutusScriptV1",
    "description": "always-true (vendored)",
    "cborHex": "4e4d01000033222220051200120011"
}
EOF
    cat > "$PLUTUS_DIR/always-true-v2.plutus" <<'EOF'
{
    "type": "PlutusScriptV2",
    "description": "always-true (vendored)",
    "cborHex": "49480100002221200101"
}
EOF
    cat > "$PLUTUS_DIR/always-true-v3.plutus" <<'EOF'
{
    "type": "PlutusScriptV3",
    "description": "always-true (vendored, V3 wire-shape candidate)",
    "cborHex": "46010100002601"
}
EOF
    zoo_info "wrote vendored always-true V1/V2/V3 to $PLUTUS_DIR"
}

build_via_aiken() {
    if ! command -v aiken >/dev/null; then
        return 1
    fi
    local work; work="$(mktemp -d)"
    trap 'rm -rf "$work"' RETURN
    zoo_info "building Plutus always-true via Aiken in $work"

    # Minimal aiken project with three validators, one per Plutus version.
    # Aiken targets the latest Plutus version by default; we override per file.
    pushd "$work" >/dev/null
    aiken new tx-zoo/always-true >/dev/null
    cd tx-zoo/always-true
    cat > validators/always_true_v1.ak <<'EOF'
// Aiken's `validator` block targets the configured plutus-version in
// aiken.toml; we keep one validator and copy the resulting .plutus for
// each version. For V2/V3 we adjust the type tag post-build.
validator always_true_v1 {
  spend(_datum: Data, _redeemer: Data, _purpose: Data, _ctx: Data) {
    True
  }
}
EOF
    aiken build >/dev/null 2>&1 || { zoo_skip "aiken build failed — falling back to vendored bytes"; return 1; }
    local out_hex
    out_hex=$(jq -r '.cborHex' plutus.json 2>/dev/null | head -1)
    if [ -z "$out_hex" ]; then
        zoo_skip "aiken did not produce expected plutus.json — fallback"
        return 1
    fi
    popd >/dev/null
    # Aiken produces a V3 validator by default; copy with V1/V2/V3 tags so the
    # downstream scripts can pick. For V1/V2, this only typechecks if the
    # validator is wire-compatible — for trivial always-true bodies it is.
    for v in V1 V2 V3; do
        local label; label=$(echo "$v" | tr 'V' 'v')
        cat > "$PLUTUS_DIR/always-true-${label}.plutus" <<EOF
{
    "type": "PlutusScript${v}",
    "description": "always-true (aiken-built)",
    "cborHex": "$out_hex"
}
EOF
    done
    zoo_info "wrote aiken-built always-true V1/V2/V3 to $PLUTUS_DIR"
    return 0
}

build_plutus_all() {
    if [ -s "$PLUTUS_DIR/always-true-v1.plutus" ] \
       && [ -s "$PLUTUS_DIR/always-true-v2.plutus" ] \
       && [ -s "$PLUTUS_DIR/always-true-v3.plutus" ]; then
        zoo_info "plutus binaries already present — skipping rebuild"
        return 0
    fi
    if ! build_via_aiken; then
        write_vendored
    fi
}

# Allow direct execution.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    build_plutus_all
fi
