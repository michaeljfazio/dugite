#!/usr/bin/env bash
# 17e — NEGATIVE, and the matched pair for 17d.
#
# Same script, same datum, DIFFERENT redeemer. `redeemerSameAsDatum` must now
# return False and the transaction must fail phase-2.
#
# This is the half that makes 17d meaningful. A validator that always succeeded
# — because dugite handed it a context it could not parse, say, and it took a
# fallback branch — would pass 17d and fail here. Without a negative, "the
# script accepted it" and "the script never really ran" look identical.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"
NAME="$(zoo_name)"
zoo_require_devnet
ctx_spend redeemer-same-as-datum inline '{"int": 43}' reject "$NAME"
