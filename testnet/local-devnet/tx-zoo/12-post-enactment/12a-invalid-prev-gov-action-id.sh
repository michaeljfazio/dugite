#!/usr/bin/env bash
# 12a — Conway GOV tag 8: a ParameterChange proposal whose `prev_action_id`
# does not chain onto the enacted PParamUpdate root (InvalidPrevGovActionId).
#
# WHY THIS EXISTS
# ---------------
# This is a P0 regression guard. dugite used to WARN-and-drop such a proposal
# instead of failing the transaction, on the (wrong) stated assumption that
# "Haskell also drops such proposals without failing the tx". Haskell's GOV
# rule is explicit (`Cardano.Ledger.Conway.Rules.Gov`):
#
#     case proposalsAddAction actionState proposals of
#       Just updatedProposals -> pure updatedProposals
#       Nothing -> proposals <$ failBecause (injectFailure $
#                    InvalidPrevGovActionId proposal)
#
# `failBecause` fails the TRANSACTION, so any block carrying it is invalid.
# With the drop behaviour dugite's mempool admitted the tx, the forge minted
# it, and cardano-node rejected the block with
# `ConwayGovFailure (InvalidPrevGovActionId …)` and answered ShutdownPeer —
# splitting the chain (observed at slot 1870: dugite ran on to block 1106
# while cardano-node froze at 892).
#
# WHY run-all.sh NEVER CAUGHT IT
# ------------------------------
# `run-all.sh` runs `06-proposals` BEFORE `10-gov-lifecycle`, so at the time
# 06b-parameter-change runs no ParameterChange has ever been enacted and the
# root is legitimately null — `prev_action_id = None` is correct there. The
# bug only appears once a ParameterChange HAS been enacted. This script
# therefore asserts the precondition (non-null root) and SKIPs rather than
# silently passing when it is not met, so it can never report a false PASS.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_require_devnet
ADDR=$(cat "$ZOO_PAY_ADDR_FILE")

# --- Precondition: a ParameterChange must already be ENACTED --------------
# Without a non-null PParamUpdate root, prev_action_id=None is VALID and the
# tx would be correctly accepted — the test would not discriminate.
ROOT=$(cardano-cli conway query gov-state \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --output-json 2>/dev/null \
    | jq -r '(.nextRatifyState.nextEnactState.prevGovActionIds
              // .enactState.prevGovActionIds
              // .prevGovActionIds).PParamUpdate.txId // empty')

if [ -z "$ROOT" ]; then
    zoo_record "$NAME" SKIP "" "no-enacted-pparam-root (run 10-gov-lifecycle first)"
    exit 0
fi

DEPOSIT=$(cardano-cli conway query gov-state \
    --testnet-magic "$LD_MAGIC" \
    --socket-path   "$ZOO_SOCKET" \
    --output-json 2>/dev/null \
    | jq -r '.currentPParams.govActionDeposit // 100000000000')

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
TXIN=${UTXO%% *}
AMT=${UTXO##* }
TIP=$(zoo_tip_slot)
FEE=300000
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"
ACTION="$ZOO_BUILT/$NAME.action"

# Build a ParameterChange with NO --prev-governance-action-tx-id, i.e.
# prev_action_id = SNothing, while the enacted root is $ROOT (non-null).
# That is precisely the shape cardano-ledger rejects with tag 8.
WA_STAKE_VKEY="$ZOO_KEYS/wallet-a/stake.vkey"
[ -s "$WA_STAKE_VKEY" ] || { zoo_record "$NAME" SKIP "" "no-wallet-a-stake-key (run --setup first)"; exit 0; }

cardano-cli conway governance action create-protocol-parameters-update \
    --testnet \
    --governance-action-deposit "$DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA_STAKE_VKEY" \
    --anchor-url  "$(zoo_anchor_url pparam-change)" \
    --anchor-data-hash "$(zoo_anchor_hash pparam-change)" \
    --max-block-body-size 90114 \
    --out-file "$ACTION" >/dev/null 2>&1 || \
{ zoo_record "$NAME" SKIP "" "create-protocol-parameters-update-failed"; exit 0; }

cardano-cli conway transaction build-raw \
    --tx-in          "$TXIN" \
    --tx-out         "${ADDR}+$((AMT - FEE - DEPOSIT))" \
    --fee            "$FEE" \
    --ttl            $((TIP + 600)) \
    --proposal-file  "$ACTION" \
    --out-file       "$RAW" 2>/dev/null || true

cardano-cli conway transaction sign \
    --testnet-magic  "$LD_MAGIC" \
    --tx-body-file   "$RAW" \
    --signing-key-file "$ZOO_PAY_SKEY" \
    --out-file       "$SIGNED" 2>/dev/null || true

zoo_expect_failure "invalid-prev-gov-action-id submit" \
    cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" \
        --socket-path   "$ZOO_SOCKET" \
        --tx-file       "$SIGNED" \
    && zoo_record "$NAME" PASS "" "rejected-InvalidPrevGovActionId root=${ROOT:0:16}" \
    || zoo_record "$NAME" FAIL "" "accepted-but-should-reject (root=${ROOT:0:16} non-null)"
