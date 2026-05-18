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

# Build a V3 always-true validator via Aiken. Writes the resulting
# textEnvelope JSON file to $PLUTUS_DIR/always-true-v3.plutus.
build_v3_via_aiken() {
    if ! command -v aiken >/dev/null; then
        return 1
    fi
    local work; work="$(mktemp -d)"
    zoo_info "building Plutus V3 always-true via Aiken in $work"

    pushd "$work" >/dev/null
    # `aiken new <ns>/<name>` strips the namespace and creates ./<name>/
    aiken new tx-zoo/always-true-v3 >/dev/null 2>&1 \
        || { popd >/dev/null; zoo_fail "aiken new failed"; return 1; }
    cd always-true-v3

    # Conway Plutus V3 validator surface: a real-typed validator from
    # the stdlib so that the compiled UPLC is wire-valid (the placeholder
    # `46010100002601` failed deserialisation with TooMuchSpace).
    # The stdlib bring imports for Credential / PolicyId / Transaction /
    # OutputReference — we use them as opaque types since the body is
    # `True` regardless. Removing the default placeholder is required
    # because the `aiken build` invocation otherwise fails on its
    # `todo` calls.
    rm -f validators/placeholder.ak
    cat > validators/always_true.ak <<'EOF'
use cardano/address.{Credential}
use cardano/assets.{PolicyId}
use cardano/transaction.{Transaction, OutputReference}

validator always_true {
  mint(_redeemer: Data, _policy_id: PolicyId, _self: Transaction) {
    True
  }

  spend(_datum: Option<Data>, _redeemer: Data, _utxo: OutputReference, _self: Transaction) {
    True
  }

  withdraw(_redeemer: Data, _account: Credential, _self: Transaction) {
    True
  }
}
EOF

    aiken build >/tmp/aiken-build-tx-zoo.log 2>&1 \
        || { popd >/dev/null; zoo_fail "aiken build failed (see /tmp/aiken-build-tx-zoo.log)"; return 1; }

    # Use `aiken blueprint convert --to cardano-cli` to produce the
    # exact textEnvelope cardano-cli expects. This DOUBLE-CBOR-wraps
    # the compiled plutus-core bytes (bytes(N) of bytes(M) of plutus),
    # matching the encoding cardano-cli reads when loading a script
    # file. Reading `compiledCode` from `plutus.json` directly yields
    # only the single-wrapped form and produces a "TooMuchSpace"
    # deserialisation failure at script load.
    if ! aiken blueprint convert --module always_true \
        > "$PLUTUS_DIR/always-true-v3.plutus" 2>/tmp/aiken-convert-tx-zoo.log; then
        popd >/dev/null
        rm -rf "$work"
        zoo_fail "aiken blueprint convert failed (see /tmp/aiken-convert-tx-zoo.log)"
        return 1
    fi

    popd >/dev/null
    rm -rf "$work"
    zoo_ok "wrote aiken-built V3 always-true to $PLUTUS_DIR/always-true-v3.plutus"
    return 0
}

build_plutus_all() {
    # V1/V2: always overwrite with the known-good vendored bytes (cheap
    # and self-healing if a previous run produced bad files).
    write_v1_v2_vendored
    zoo_info "wrote vendored V1/V2 always-true to $PLUTUS_DIR"

    # V3: skip the rebuild if the existing file looks plausible
    # (cborHex starts with 0x59 or 0x58 — bytes tag for a sufficiently
    # large bytestring matching a real plutus-core script). Otherwise
    # try aiken.
    local v3_file="$PLUTUS_DIR/always-true-v3.plutus"
    if [ -s "$v3_file" ]; then
        local hex
        hex=$(jq -r '.cborHex' "$v3_file" 2>/dev/null || true)
        # Vendored placeholder is "46010100002601" — a 6-byte string.
        # Real aiken output is 60+ bytes. Use length as a quick filter.
        if [ -n "$hex" ] && [ "${#hex}" -ge 100 ]; then
            zoo_info "V3 binary already present (${#hex} hex chars) — skipping rebuild"
            return 0
        fi
    fi

    if ! build_v3_via_aiken; then
        zoo_fail "aiken not available — writing placeholder V3 (03c/03f/03h will fail)"
        cat > "$v3_file" <<'EOF'
{
    "type": "PlutusScriptV3",
    "description": "always-true (PLACEHOLDER — install aiken and rebuild)",
    "cborHex": "46010100002601"
}
EOF
    fi
}

# Allow direct execution.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    build_plutus_all
fi
