#!/usr/bin/env bash
# Build / vendor the Plutus binaries the zoo needs.
#
# V1 and V2 use canonical vendored "always-true" cborHex bytes — these
# wire shapes are stable and known-good across all post-Conway nodes.
#
# V3 has historically been a moving target during Conway development.
# We require `aiken` on PATH to generate a known-good V3 always-true
# validator (`spend(_datum, _redeemer, _ctx)` returns True). If aiken
# is missing, we fall back to a placeholder cborHex and warn — the
# V3-using scripts (03c/03f/03h) will fail with TooMuchSpace until
# aiken is installed.
#
# Output: $ZOO_LIB/plutus/{always-true-v1,always-true-v2,always-true-v3}.plutus
# Each is a JSON envelope matching cardano-cli's --tx-out-script-file expectation.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/tx-zoo-common.sh"

PLUTUS_DIR="$ZOO_LIB/plutus"
mkdir -p "$PLUTUS_DIR"

# Stable, known-good V1/V2 always-true validators. These have shipped
# unchanged since the corresponding eras and remain the canonical
# reference in cardano-node's integration test fixtures.
write_v1_v2_vendored() {
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
}

# Build a V3 Plutus validator via Aiken whose handlers return $verdict
# (True | fail). Writes the resulting textEnvelope JSON file to
# $PLUTUS_DIR/always-${verdict_name}-v3.plutus.
build_v3_via_aiken() {
    local verdict_name="$1"   # "true" or "false"
    local verdict_body="$2"   # "True" or "fail"
    if ! command -v aiken >/dev/null; then
        return 1
    fi
    local work; work="$(mktemp -d)"
    zoo_info "building Plutus V3 always-${verdict_name} via Aiken in $work"

    pushd "$work" >/dev/null
    # `aiken new <ns>/<name>` strips the namespace and creates ./<name>/
    aiken new "tx-zoo/always-${verdict_name}-v3" >/dev/null 2>&1 \
        || { popd >/dev/null; zoo_fail "aiken new failed"; return 1; }
    cd "always-${verdict_name}-v3"

    # Conway Plutus V3 validator surface: a real-typed validator from
    # the stdlib so that the compiled UPLC is wire-valid (the placeholder
    # `46010100002601` failed deserialisation with TooMuchSpace).
    # The stdlib bring imports for Credential / PolicyId / Transaction /
    # OutputReference — we use them as opaque types since the body is
    # `True`/`fail` regardless. Removing the default placeholder is
    # required because the `aiken build` invocation otherwise fails on
    # its `todo` calls.
    rm -f validators/placeholder.ak
    cat > "validators/always_${verdict_name}.ak" <<EOF
use cardano/address.{Credential}
use cardano/assets.{PolicyId}
use cardano/transaction.{Transaction, OutputReference}

validator always_${verdict_name} {
  mint(_redeemer: Data, _policy_id: PolicyId, _self: Transaction) {
    ${verdict_body}
  }

  spend(_datum: Option<Data>, _redeemer: Data, _utxo: OutputReference, _self: Transaction) {
    ${verdict_body}
  }

  withdraw(_redeemer: Data, _account: Credential, _self: Transaction) {
    ${verdict_body}
  }
}
EOF

    aiken build >/tmp/aiken-build-tx-zoo.log 2>&1 \
        || { popd >/dev/null; zoo_fail "aiken build failed (see /tmp/aiken-build-tx-zoo.log)"; return 1; }

    # Use `aiken blueprint convert --module ...` to produce the exact
    # textEnvelope cardano-cli expects. This DOUBLE-CBOR-wraps the
    # compiled plutus-core bytes (bytes(N) of bytes(M) of plutus),
    # matching the encoding cardano-cli reads when loading a script
    # file. Reading `compiledCode` from `plutus.json` directly yields
    # only the single-wrapped form and produces a "TooMuchSpace"
    # deserialisation failure at script load.
    if ! aiken blueprint convert --module "always_${verdict_name}" \
        > "$PLUTUS_DIR/always-${verdict_name}-v3.plutus" 2>/tmp/aiken-convert-tx-zoo.log; then
        popd >/dev/null
        rm -rf "$work"
        zoo_fail "aiken blueprint convert failed (see /tmp/aiken-convert-tx-zoo.log)"
        return 1
    fi

    popd >/dev/null
    rm -rf "$work"
    zoo_ok "wrote aiken-built V3 always-${verdict_name} to $PLUTUS_DIR/always-${verdict_name}-v3.plutus"
    return 0
}

# Skip Aiken rebuild if the existing file looks plausible (cborHex
# length >= 100 hex chars matches a real plutus-core script; a
# placeholder is ~14 hex chars).
_v3_binary_already_good() {
    local f="$1"
    [ -s "$f" ] || return 1
    local hex; hex=$(jq -r '.cborHex' "$f" 2>/dev/null || true)
    [ -n "$hex" ] && [ "${#hex}" -ge 100 ]
}

build_plutus_all() {
    # V1/V2: always overwrite with the known-good vendored bytes (cheap
    # and self-healing if a previous run produced bad files).
    write_v1_v2_vendored
    zoo_info "wrote vendored V1/V2 always-true to $PLUTUS_DIR"

    # V3 always-true.
    local v3_true="$PLUTUS_DIR/always-true-v3.plutus"
    if _v3_binary_already_good "$v3_true"; then
        zoo_info "V3 always-true already present — skipping rebuild"
    elif ! build_v3_via_aiken "true" "True"; then
        zoo_fail "aiken not available — writing placeholder V3 always-true (03c/03f/03h will fail)"
        cat > "$v3_true" <<'EOF'
{
    "type": "PlutusScriptV3",
    "description": "always-true (PLACEHOLDER — install aiken and rebuild)",
    "cborHex": "46010100002601"
}
EOF
    fi

    # V3 always-false (used by 03j for the legitimate is_valid=false +
    # collateral-consumed path; the previously vendored
    # always-false-v2.plutus carried malformed UPLC bytes).
    local v3_false="$PLUTUS_DIR/always-false-v3.plutus"
    if _v3_binary_already_good "$v3_false"; then
        zoo_info "V3 always-false already present — skipping rebuild"
    elif ! build_v3_via_aiken "false" "fail"; then
        zoo_fail "aiken not available — writing placeholder V3 always-false (03j will SKIP)"
        cat > "$v3_false" <<'EOF'
{
    "type": "PlutusScriptV3",
    "description": "always-false (PLACEHOLDER — install aiken and rebuild)",
    "cborHex": "46010100002601"
}
EOF
    fi
}

# Allow direct execution.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    build_plutus_all
fi
