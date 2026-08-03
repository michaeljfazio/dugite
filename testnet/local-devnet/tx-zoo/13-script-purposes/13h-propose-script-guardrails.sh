#!/usr/bin/env bash
# 13h — THE PROPOSING PURPOSE (redeemer tag 5).
#
# This is the one script purpose that cannot be exercised with an arbitrary
# script, and the reason is worth stating precisely because it dictates the
# whole shape of the test.
#
# Only `ParameterChange` and `TreasuryWithdrawals` carry a guardrails-policy
# field, and Conway's GOV rule hard-checks it (Rules/Gov.hs):
#
#   checkGuardrailsScriptHash expectedHash actualHash =
#     failureUnless (actualHash == expectedHash) $
#       InvalidGuardrailsScriptHash actualHash expectedHash
#
# `expectedHash` is the CURRENT constitution's own guardrails script. The
# comparison is strict equality INCLUDING `SNothing == SNothing`. So:
#
#   * if the constitution has no guardrails script (the devnet default), a
#     proposal naming ANY policy hash is rejected outright, and a proposal
#     naming none requires no Proposing witness at all — the purpose is
#     unreachable;
#   * if the constitution HAS one, every ParameterChange and
#     TreasuryWithdrawals proposal must name exactly that hash — including
#     06b, 06d and 10a, which currently name none.
#
# Seeding a guardrails script into conway-genesis therefore has devnet-wide
# blast radius, so it is opt-in: `LD_SEED_GUARDRAILS=1 ./setup.sh` installs the
# always-true V3 script as the constitution's guardrails script. Without it this
# script records a STATE skip (the surface is exercisable, the chain is simply
# not configured for it) rather than pretending to cover the purpose.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/03-plutus/_lock-helper.sh"
. "$ZOO_DIR/13-script-purposes/_purpose-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
W="script-stake-v3"
SCRIPT=$(script_file "$W")
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing script-stake wallet — run run-all.sh --setup"; exit 0; }

# What guardrails script does the live constitution actually have?
CONSTITUTION=$(cardano-cli conway query constitution \
                 --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" 2>/dev/null)
GUARDRAILS=$(echo "$CONSTITUTION" | jq -r '.script // empty')
SCRIPT_HASH=$(cat "$ZOO_KEYS/$W/stake-script.hash")

if [ -z "$GUARDRAILS" ] || [ "$GUARDRAILS" = "null" ]; then
    zoo_skip "constitution has no guardrails script — the Proposing purpose is structurally unreachable on this genesis (re-run setup with LD_SEED_GUARDRAILS=1)"
    zoo_record "$NAME" SKIP "" "no-guardrails-script-in-constitution"
    exit 0
fi

if [ "$GUARDRAILS" != "$SCRIPT_HASH" ]; then
    zoo_skip "constitution guardrails hash ${GUARDRAILS:0:16}… != our script ${SCRIPT_HASH:0:16}… — cannot satisfy checkGuardrailsScriptHash"
    zoo_record "$NAME" SKIP "" "guardrails-hash-mismatch"
    exit 0
fi

ADDR=$(script_pay_addr "$W")
STAKE_ADDR=$(cat "$ZOO_KEYS/wallet-a/stake.addr")
PPARAMS=$(zoo_pparams_file)
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")

zoo_anchor_start >/dev/null 2>&1 || true
ANCHOR_URL=$(zoo_anchor_url)
ANCHOR_HASH=$(zoo_anchor_hash)

# A minimal ParameterChange naming the constitution's guardrails script. The
# --constitution-script-hash argument is what puts the policy hash in the
# action, and it is what makes the proposal require a Proposing redeemer.
ACTION="$ZOO_BUILT/$NAME.action"
cardano-cli conway governance action create-protocol-parameters-update \
    --testnet-magic "$LD_MAGIC" \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-address "$STAKE_ADDR" \
    --anchor-url "$ANCHOR_URL" \
    --anchor-data-hash "$ANCHOR_HASH" \
    --constitution-script-hash "$GUARDRAILS" \
    --min-pool-cost 340000000 \
    --out-file "$ACTION" 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "action create: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "action-create"; exit 1; }

REDEEMER="$ZOO_BUILT/$NAME.redeemer.json"
write_redeemer "$REDEEMER"
COLLAT=$(plutus_collateral) || { zoo_record "$NAME" FAIL "" "collateral"; exit 1; }
UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
cardano-cli conway transaction build \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --tx-in         "${UTXO%% *}" \
    --tx-in-collateral "$COLLAT" \
    --change-address "$ADDR" \
    --proposal-file          "$ACTION" \
    --proposal-script-file   "$SCRIPT" \
    --proposal-redeemer-file "$REDEEMER" \
    --out-file      "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "proposal build: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file      "$SIGNED" >/dev/null

assert_purpose "$SIGNED" Proposing || { zoo_record "$NAME" FAIL "" "no-proposing-redeemer"; exit 1; }

TXID=$(zoo_submit "$SIGNED") || { zoo_record "$NAME" FAIL "" "submit"; exit 1; }
wait_all_strict "$TXID" 150 "$ADDR" \
    && zoo_record "$NAME" PASS "$TXID" "proposing-purpose guardrails=${GUARDRAILS:0:16}" \
    || { zoo_record "$NAME" FAIL "$TXID" "not-included"; exit 1; }
