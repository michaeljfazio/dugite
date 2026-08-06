#!/usr/bin/env bash
# Shared helpers for 19-era-negatives (#1034): Conway wire-rejection of
# legacy-era artifacts.
#
# Two distinct mechanisms are exercised by this category, and they are NOT
# interchangeable — see the category README.md for the full analysis:
#
#   19a-19d  Build a GENUINE Shelley-era transaction (`cardano-cli compatible
#            shelley transaction signed-transaction`, top-level array(3),
#            envelope type "TxSignedShelley") carrying a certificate/body
#            field that Conway's OWN wire format can no longer express (MIR,
#            GenesisKeyDelegation, a legacy update proposal in body key 6),
#            and submit it against a chain that is past the hard fork.
#            `cardano-cli conway transaction submit` auto-detects the
#            envelope's era and forwards it untouched — it does NOT refuse
#            these files client-side (empirically confirmed: it proceeds to
#            open the socket rather than erroring on the envelope type), so
#            no raw-socket fallback is needed here.
#
#   19e-19f  Build a VALID Conway transaction, then splice one certificate's
#            own tag byte to 5 or 6 using tx-cbor-tool.py's `splice-cert-tag`
#            (#1023's regression pin). Unlike 19a-19d, `cardano-cli` itself
#            uses the real ledger decoder to READ a Conway-tagged tx file and
#            hard-rejects tag 5/6 client-side with the exact cardano-ledger
#            message ("MIR certificates are no longer supported" / "Genesis
#            delegation certificates are no longer supported") BEFORE ever
#            opening a socket — confirmed empirically against cardano-cli
#            11.0.0.0. So submission for 19e/19f goes through `dugite-cli
#            transaction submit`, which forwards cborHex to the node
#            untouched (same precedent as 08-negative/08f-double-spend.sh).
set -euo pipefail

ERA_NEG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---- Guard: `cardano-cli compatible shelley` may be absent on some installs ----
#
# 19a-19d are unusable without it (there is no other way to construct a
# genuine pre-Alonzo standalone tx). Structural — env-skip, not state-skip.
era_neg_require_compatible_shelley() {
    local name="$1"
    if ! cardano-cli compatible shelley transaction signed-transaction --help \
            >/dev/null 2>&1; then
        zoo_record_env_skip "$name" "cardano-cli-compatible-shelley-subcommand-missing"
        return 1
    fi
    return 0
}

# ---- Guard: `dugite-cli` must be built for 19e/19f (raw-forwarding submit) ----
era_neg_require_dugite_cli() {
    local name="$1"
    DUGITE_CLI="${DUGITE_CLI:-$LD_REPO_ROOT/target/release/dugite-cli}"
    if [ ! -x "$DUGITE_CLI" ]; then
        zoo_record_env_skip "$name" "dugite-cli-not-built (cargo build --release -p dugite-cli)"
        return 1
    fi
    return 0
}

# ---- UTxO + balance helper ----
#
# Picks the funding wallet's largest UTxO and prints
# "TXIN INPUT_LOVELACE FEE CHANGE_LOVELACE" for a single-output,
# pay-fee-and-return-change transaction. Returns non-zero if the UTxO can't
# cover the fee (state-skip territory, not environmental — the funding wallet
# is momentarily too depleted this round).
era_neg_pick_utxo() {
    local addr="$1" fee="${2:-300000}"
    local utxo txin amt
    utxo=$(zoo_largest_utxo "$addr") || return 1
    txin=${utxo%% *}
    amt=${utxo##* }
    [ "$amt" -gt "$fee" ] || return 1
    printf '%s %s %s %s\n' "$txin" "$amt" "$fee" "$((amt - fee))"
}

# ---- Submit + classify a single socket's response ----
#
# era_neg_submit_cli <signed-file> <socket> [<cli-cmd...>]
#
# Defaults to `cardano-cli conway transaction submit` (19a-19d: the file
# needs era auto-detection, which only cardano-cli does). 19e/19f override
# with dugite-cli (see era_neg_submit_dugite below) because cardano-cli
# refuses those files before ever touching a socket.
#
# Prints "<OUTCOME> <detail>" on stdout, one line, and returns:
#   0  rejected (the tx did NOT go through) — the wanted outcome here
#   1  accepted (rc=0) — FAIL condition for every script in this category
#   2  socket absent — could not exercise this observer at all
era_neg_submit_cli() {
    local signed="$1" sock="$2"
    if [ ! -S "$sock" ]; then
        printf 'SKIP socket-not-found\n'
        return 2
    fi
    local out rc
    out=$(cardano-cli conway transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
            --tx-file "$signed" 2>&1) && rc=0 || rc=1
    local short
    short=$(printf '%s' "$out" | tr '\n' ' ' | cut -c1-220)
    if [ "$rc" -eq 0 ]; then
        printf 'ACCEPTED %s\n' "$short"
        return 1
    fi
    printf 'REJECTED %s\n' "$short"
    return 0
}

# era_neg_submit_dugite <signed-file> <socket>
#
# Same contract as era_neg_submit_cli, but via dugite-cli (raw-forwarding —
# no client-side ledger-decoder refusal). Used by 19e/19f, and also usable
# against $LD_CARDANO_BP_SOCK: dugite-cli speaks the standard N2C wire
# protocol regardless of which implementation is listening on the far end.
era_neg_submit_dugite() {
    local signed="$1" sock="$2"
    if [ ! -S "$sock" ]; then
        printf 'SKIP socket-not-found\n'
        return 2
    fi
    local out rc
    out=$("$DUGITE_CLI" transaction submit \
            --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
            --tx-file "$signed" 2>&1) && rc=0 || rc=1
    local short
    short=$(printf '%s' "$out" | tr '\n' ' ' | cut -c1-220)
    if [ "$rc" -eq 0 ]; then
        printf 'ACCEPTED %s\n' "$short"
        return 1
    fi
    printf 'REJECTED %s\n' "$short"
    return 0
}

# ---- 19e/19f shared setup: a VALID Conway tx carrying one stake-registration
# cert (tag 7, `reg_deposit_cert`), built with `build-raw`, then signed BOTH
# by cardano-cli and by the vendored signer (lib/ed25519_pure.py via
# tx-cbor-tool.py's `sign`) — the two are compared byte-for-byte before
# either is trusted, exactly like 08-negative/08f-double-spend.sh: a broken
# vendored signer would otherwise make a splice-based negative test pass
# vacuously (the node rejecting a bad SIGNATURE rather than the spliced
# cert). On success prints "BODY_PATH SIGNED_PATH" (the vendored-signed
# path); on any failure records an appropriate SKIP/FAIL and returns
# non-zero — callers must not proceed.
era_neg_conway_stake_reg_base() {
    local name="$1" addr="$2"
    local utxo_line txin amt fee change
    utxo_line=$(era_neg_pick_utxo "$addr" 300000) || {
        zoo_record "$name" SKIP "" "no-precondition:funding-utxo-too-small"
        return 1
    }
    read -r txin amt fee change <<< "$utxo_line"

    local stake_vkey="$ZOO_BUILT/$name-stake.vkey" stake_skey="$ZOO_BUILT/$name-stake.skey"
    cardano-cli conway stake-address key-gen \
        --verification-key-file "$stake_vkey" --signing-key-file "$stake_skey" || {
        zoo_record "$name" FAIL "" "stake-keygen"; return 1; }

    local pparams deposit
    pparams=$(zoo_pparams_file)
    deposit=$(jq -r '.stakeAddressDeposit // .keyDeposit // 2000000' "$pparams")
    # Rebalance the change output to also cover the registration deposit —
    # the deposit is withdrawn from the tx's own UTxO balance, not minted.
    if [ "$change" -le "$deposit" ]; then
        zoo_record "$name" SKIP "" "no-precondition:funding-utxo-too-small-for-deposit"
        return 1
    fi
    change=$((change - deposit))

    local cert="$ZOO_BUILT/$name-base.cert"
    cardano-cli conway stake-address registration-certificate \
        --stake-verification-key-file "$stake_vkey" \
        --key-reg-deposit-amt "$deposit" \
        --out-file "$cert" || {
        zoo_record "$name" FAIL "" "reg-cert-create"; return 1; }

    local body="$ZOO_BUILT/$name-base.body"
    cardano-cli conway transaction build-raw \
        --tx-in "$txin" \
        --tx-out "${addr}+${change}" \
        --fee "$fee" \
        --certificate-file "$cert" \
        --out-file "$body" || {
        zoo_record "$name" FAIL "" "build-raw-failed"; return 1; }

    local signed_ref="$ZOO_BUILT/$name-base-ref.signed"
    cardano-cli conway transaction sign \
        --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$body" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --signing-key-file "$stake_skey" \
        --out-file "$signed_ref" || {
        zoo_record "$name" SKIP "" "cardano-cli-sign-failed"; return 1; }

    local signed_chk="$ZOO_BUILT/$name-base-chk.signed"
    python3 "$ZOO_PY_TX_CBOR" sign \
        --in "$body" --out "$signed_chk" \
        --signing-key-file "$ZOO_PAY_SKEY" \
        --signing-key-file "$stake_skey" >/dev/null || {
        zoo_record "$name" SKIP "" "vendored-signer-failed"; return 1; }

    local ref_hex chk_hex
    ref_hex=$(jq -r '.cborHex' "$signed_ref")
    chk_hex=$(jq -r '.cborHex' "$signed_chk")
    if [ "$ref_hex" != "$chk_hex" ]; then
        zoo_record "$name" SKIP "" "vendored-signer-not-byte-identical-to-cardano-cli"
        return 1
    fi
    log_info "$name: vendored signer verified byte-identical to cardano-cli"

    printf '%s %s\n' "$body" "$signed_chk"
}

# ---- Combined two-socket assertion ----
#
# era_neg_assert_rejected_both <name> <signed-file> <txid> <submit-fn>
#
# Submits via <submit-fn> (era_neg_submit_cli or era_neg_submit_dugite) to
# BOTH $ZOO_SOCKET (dugite — relay by default) and $LD_CARDANO_BP_SOCK
# (Haskell), and records ONE result line combining both observations in the
# detail field — this IS the "known reject-reason differences" convention
# used elsewhere in the zoo (see `_cert-neg-helper.sh`'s
# `expect_cert_rejection` and 08f-double-spend.sh's REASON variable): record
# the observed text rather than pretending both implementations must answer
# identically. PASS iff BOTH observers reject (any reason); FAIL if EITHER
# accepts; ENV-SKIP if BOTH sockets are absent.
era_neg_assert_rejected_both() {
    local name="$1" signed="$2" txid="${3:-}" submit_fn="${4:-era_neg_submit_cli}"
    local dugite_line cbp_line dugite_rc cbp_rc

    dugite_line=$("$submit_fn" "$signed" "$ZOO_SOCKET") && dugite_rc=0 || dugite_rc=$?
    cbp_line=$("$submit_fn" "$signed" "$LD_CARDANO_BP_SOCK") && cbp_rc=0 || cbp_rc=$?

    if [ "$dugite_rc" -eq 2 ] && [ "$cbp_rc" -eq 2 ]; then
        zoo_record_env_skip "$name" "both-sockets-not-found"
        return 0
    fi

    local detail="dugite(${ZOO_SOCKET})=${dugite_line}; cbp(${LD_CARDANO_BP_SOCK})=${cbp_line}"

    if [ "$dugite_rc" -eq 1 ] || [ "$cbp_rc" -eq 1 ]; then
        zoo_fail "$name: accepted where rejection was expected — $detail"
        zoo_record "$name" FAIL "$txid" "$detail"
        return 1
    fi

    # One or both may have SKIPped (socket absent) while the other rejected —
    # still a PASS: the observer(s) that WERE reachable rejected it, which is
    # the property under test. A pure double-SKIP was already handled above.
    zoo_ok "$name: rejected — $detail"
    zoo_record "$name" PASS "$txid" "$detail"
    return 0
}

# era_neg_assert_wrong_era_both <name> <signed-file> <txid> <submit-fn>
#
# Stricter form of `era_neg_assert_rejected_both` for the legacy-era cases
# (19a-19d): both observers must reject AND **dugite's reason must name an era
# mismatch**, not a CBOR decode failure.
#
# Why this exists (#1047): before dugite had a wire-era check, a legacy-era
# submission was rejected only as an ACCIDENTAL decode error — the standalone
# Shelley decoder expects a different array length than a real Shelley tx has.
# The verdict was right for the wrong reason, and correcting any one of those
# decoders without adding an era check would have turned these cases into
# ACCEPTS on a Conway chain (MIR Phase-1 validation is a documented no-op at
# PV>=9 and GenesisKeyDelegation has era-unconditional apply-time support, so the
# ledger layer would not have caught them). dugite now answers
# `HardForkApplyTxErrWrongEra` before decoding, so the reason is assertable and
# the accident is no longer load-bearing.
#
# The reason is matched against a SET of renderings rather than one exact string,
# because the text comes from cardano-cli's formatting of
# `HardForkApplyTxErrWrongEra` and is not dugite's to pin. What IS pinned is the
# negative: the reason must NOT be a decode failure, which is precisely the
# pre-#1047 behaviour this asserts we have moved off.
era_neg_assert_wrong_era_both() {
    local name="$1" signed="$2" txid="${3:-}" submit_fn="${4:-era_neg_submit_cli}"
    local dugite_line cbp_line dugite_rc cbp_rc

    dugite_line=$("$submit_fn" "$signed" "$ZOO_SOCKET") && dugite_rc=0 || dugite_rc=$?
    cbp_line=$("$submit_fn" "$signed" "$LD_CARDANO_BP_SOCK") && cbp_rc=0 || cbp_rc=$?

    if [ "$dugite_rc" -eq 2 ] && [ "$cbp_rc" -eq 2 ]; then
        zoo_record_env_skip "$name" "both-sockets-not-found"
        return 0
    fi

    local detail="dugite(${ZOO_SOCKET})=${dugite_line}; cbp(${LD_CARDANO_BP_SOCK})=${cbp_line}"

    if [ "$dugite_rc" -eq 1 ] || [ "$cbp_rc" -eq 1 ]; then
        zoo_fail "$name: accepted where rejection was expected — $detail"
        zoo_record "$name" FAIL "$txid" "$detail"
        return 1
    fi

    # dugite was unreachable — cannot assert its reason; the rejection by the
    # observer that WAS reachable still holds.
    if [ "$dugite_rc" -eq 2 ]; then
        zoo_ok "$name: rejected (dugite socket absent, reason not asserted) — $detail"
        zoo_record "$name" PASS "$txid" "$detail"
        return 0
    fi

    if printf '%s' "$dugite_line" | grep -qiE 'EraMismatch|otherEraName|WrongEra|era of the node'; then
        zoo_ok "$name: rejected with an era mismatch — $detail"
        zoo_record "$name" PASS "$txid" "$detail"
        return 0
    fi

    if printf '%s' "$dugite_line" | grep -qiE 'decode failed|DeserialiseFailure'; then
        zoo_fail "$name: dugite rejected via a CBOR DECODE error, not an era check — this is the pre-#1047 accident and means the wire-era check did not fire — $detail"
        zoo_record "$name" FAIL "$txid" "decode-not-era-check: $detail"
        return 1
    fi

    zoo_fail "$name: dugite rejected but the reason names neither an era mismatch nor a decode failure; #1047 expects HardForkApplyTxErrWrongEra — $detail"
    zoo_record "$name" FAIL "$txid" "unexpected-reason: $detail"
    return 1
}
