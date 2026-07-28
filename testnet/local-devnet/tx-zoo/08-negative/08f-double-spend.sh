#!/usr/bin/env bash
# 08f — Rule 1b: a duplicate input WITHIN one transaction must be rejected.
#
# Which double-spend this is, and why (#918)
# ------------------------------------------
# The script's original intent is the *within-tx* duplicate-input path, not two
# transactions racing for one UTxO — that cross-tx case is 11b's job and is
# already covered there. This script keeps the within-tx intent.
#
# It is a real rule, not a no-op. From protocol version 9 (Conway) every
# `Set`-typed wire field decodes through cardano-ledger-binary's
# `decodeSetEnforceNoDuplicates`, which hard-fails when the decoded element
# count is lower than the encoded one. Before PV9 the same field went through
# `Set.fromList`, which silently deduplicates — which is why historical
# Babbage-era transactions with repeated inputs exist and were accepted. dugite
# mirrors the PV-gated semantic in Phase-1 (`ValidationError::DuplicateInput`,
# gated on `protocol_version_major >= 9`; see Rule 1b in
# crates/dugite-ledger/src/validation/phase1.rs), surfaced over N2C as
# `BadInputsUTxO`. The devnet runs Conway, so rejection is required.
#
# Why the transaction is built and submitted by hand
# --------------------------------------------------
# The cardano-cli toolchain refuses to touch such a transaction, because it
# decodes bodies with the very decoder that enforces the rule:
#
#   $ cardano-cli conway transaction sign --tx-body-file <dup>
#   TextEnvelope decode error: DecoderErrorDeserialiseFailure "Shelley Tx"
#   (DeserialiseFailure 79 "Final number of elements: 1 does not match the
#    total count that was decoded: 2")
#
# `transaction build-raw` additionally collapses repeated `--tx-in` arguments,
# so the duplicate never even reaches the file. That is what the old
# `cardano-cli-build-raw-dedupes-inputs` SKIP recorded — and a permanent SKIP is
# indistinguishable from a PASS in the summary line.
#
# So: build-raw a normal single-input body, splice the duplicate into the body
# CBOR (lib/tx-cbor-tool.py), sign the modified body with the vendored Ed25519
# signer (lib/ed25519_pure.py), and submit the raw bytes through
# `dugite-cli transaction submit`, which hands cborHex to the node untouched.
#
# The signer is proven, in-line, before it is trusted: the same body is signed
# with cardano-cli AND with the vendored signer and the two are byte-compared
# (Ed25519 is deterministic). If they differ the script env-skips rather than
# submitting a transaction that would be rejected for the wrong reason — a
# broken signer would otherwise make this negative test pass vacuously.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet

DUGITE_CLI="${DUGITE_CLI:-$LD_REPO_ROOT/target/release/dugite-cli}"
if [ ! -x "$DUGITE_CLI" ]; then
    zoo_record_env_skip "$NAME" "dugite-cli-not-built (cargo build --release -p dugite-cli)"
    exit 0
fi

ADDR=$(cat "$ZOO_PAY_ADDR_FILE")
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=200000

BODY="$ZOO_BUILT/${NAME}.body"
BODY_DUP="$ZOO_BUILT/${NAME}-dup.body"
SIGNED_REF="$ZOO_BUILT/${NAME}-ref.signed"     # cardano-cli, single input
SIGNED_CHK="$ZOO_BUILT/${NAME}-check.signed"   # vendored signer, single input
SIGNED_DUP="$ZOO_BUILT/${NAME}-dup.signed"     # vendored signer, DUPLICATE input

# ── 1. A perfectly ordinary single-input body ────────────────────────────────
cardano-cli conway transaction build-raw \
    --tx-in    "$TXIN" \
    --tx-out   "${ADDR}+$((AMT - FEE))" \
    --fee      "$FEE" \
    --ttl      $((TIP + 600)) \
    --out-file "$BODY" 2>/dev/null \
    || { zoo_record_env_skip "$NAME" "build-raw-failed"; exit 0; }

# ── 2. Prove the vendored signer against cardano-cli on that same body ───────
cardano-cli conway transaction sign \
    --testnet-magic    "$LD_MAGIC" \
    --tx-body-file     "$BODY" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file         "$SIGNED_REF" 2>/dev/null \
    || { zoo_record_env_skip "$NAME" "cardano-cli-sign-failed"; exit 0; }

python3 "$ZOO_PY_TX_CBOR" sign \
    --in  "$BODY" \
    --out "$SIGNED_CHK" \
    --signing-key-file "$ZOO_PAY_SKEY" >/dev/null 2>&1 \
    || { zoo_record_env_skip "$NAME" "vendored-signer-failed"; exit 0; }

REF_HEX=$(jq -r '.cborHex' "$SIGNED_REF")
CHK_HEX=$(jq -r '.cborHex' "$SIGNED_CHK")
if [ "$REF_HEX" != "$CHK_HEX" ]; then
    zoo_record_env_skip "$NAME" "vendored-signer-not-byte-identical-to-cardano-cli"
    exit 0
fi
log_info "vendored signer verified byte-identical to cardano-cli"

# ── 3. Splice the duplicate input into the body, then sign THAT body ─────────
python3 "$ZOO_PY_TX_CBOR" dup-input --in "$BODY" --out "$BODY_DUP" >/dev/null 2>&1 \
    || { zoo_record_env_skip "$NAME" "cbor-dup-input-failed"; exit 0; }

DUP_SHAPE=$(python3 "$ZOO_PY_TX_CBOR" show --in "$BODY_DUP" 2>/dev/null || echo '{}')
IN_COUNT=$(printf '%s' "$DUP_SHAPE" | jq -r '.input_count // 0')
DISTINCT=$(printf '%s' "$DUP_SHAPE" | jq -r '.distinct_inputs // 0')
if [ "$IN_COUNT" != "2" ] || [ "$DISTINCT" != "1" ]; then
    zoo_record_env_skip "$NAME" "dup-input-not-constructed count=${IN_COUNT} distinct=${DISTINCT}"
    exit 0
fi

TXID=$(python3 "$ZOO_PY_TX_CBOR" sign \
        --in  "$BODY_DUP" \
        --out "$SIGNED_DUP" \
        --signing-key-file "$ZOO_PAY_SKEY" 2>/dev/null) \
    || { zoo_record_env_skip "$NAME" "vendored-signer-failed-on-dup-body"; exit 0; }

log_info "built duplicate-input tx $TXID (input $TXIN listed twice)"

# ── 4. Submit the raw bytes. The node MUST reject. ───────────────────────────
SUBMIT_OUT=$("$DUGITE_CLI" transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED_DUP" 2>&1) && SUBMIT_RC=0 || SUBMIT_RC=$?
SUBMIT_SHORT=$(printf '%s' "$SUBMIT_OUT" | head -c 200 | tr '\n' ' ')

if [ "$SUBMIT_RC" -eq 0 ]; then
    zoo_record "$NAME" FAIL "$TXID" "duplicate-input tx ACCEPTED at PV>=9 (must be rejected): ${SUBMIT_SHORT}"
    exit 0
fi

# Rejected. Record whether the reason names the right rule: dugite answers
# BadInputsUTxO (Phase-1 DuplicateInput); a decoder-level refusal is equally
# correct (that is what Haskell does) — both are the same observable outcome.
if printf '%s' "$SUBMIT_OUT" | grep -qiE 'badinputs|duplicate|decode|deserialis|set'; then
    REASON="reason-matches-rule"
else
    REASON="rejected-other-reason"
fi
zoo_record "$NAME" PASS "$TXID" "duplicate-input-rejected rc=${SUBMIT_RC} ${REASON}: ${SUBMIT_SHORT}"
