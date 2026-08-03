#!/usr/bin/env bash
# 17b — POSITIVE: `datumIsWellformed` asserts the spent datum is present in
# `txInfoData`.
#
#   not $ null $ filter (datum ==) $ elems (txInfoData txInfo)
#
# `txInfoData` is populated from the witness set's datum map, so a datum
# supplied by HASH must be reachable there. This is the only zoo script that
# asserts the datum witness map is threaded into the context at all.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"
NAME="$(zoo_name)"
zoo_require_devnet
ctx_spend datum-is-wellformed hash '{"int": 42}' accept "$NAME"
