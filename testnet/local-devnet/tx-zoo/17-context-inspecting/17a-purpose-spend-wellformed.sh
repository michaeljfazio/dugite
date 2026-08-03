#!/usr/bin/env bash
# 17a — POSITIVE: `purposeIsWellformedWithDatum` asserts, from inside the
# script, that the TxOutRef being spent actually appears in `txInfoInputs`.
#
#   ScriptContext txInfo _ (SpendingScript txOutRef (Just _)) ->
#     not $ null $ filter ((txOutRef ==) . txInInfoOutRef) (txInfoInputs txInfo)
#
# So this fails if dugite builds the inputs list wrongly, or encodes a TxOutRef
# differently from upstream — the bare-txid encoding that bit us in #772.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"
NAME="$(zoo_name)"
zoo_require_devnet
ctx_spend purpose-is-wellformed-with-datum inline '{"int": 42}' accept "$NAME"
