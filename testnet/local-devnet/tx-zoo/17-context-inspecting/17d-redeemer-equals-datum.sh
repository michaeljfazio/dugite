#!/usr/bin/env bash
# 17d — POSITIVE: `redeemerSameAsDatum` compares the redeemer against the
# datum, both read out of the ScriptContext.
#
#   ScriptContext _ (Redeemer redeemer) (SpendingScript _ (Just (Datum datum)))
#     -> datum == redeemer
#
# It fails unless BOTH are delivered to the script intact — this is the
# redeemer-pointer resolution and the datum plumbing checked against each
# other, rather than each being assumed correct. The datum is `{"int": 42}`
# (see _lock-helper.sh), so the redeemer must match it.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"
NAME="$(zoo_name)"
zoo_require_devnet
ctx_spend redeemer-same-as-datum inline '{"int": 42}' accept "$NAME"
