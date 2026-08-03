#!/usr/bin/env bash
# 17c — POSITIVE: `inputsOutputsAreNotEmptyWithDatum` reads BOTH
# `txInfoInputs` and `txInfoOutputs` and fails if either is empty.
#
# Cheap, but it is the only assertion in the zoo that the outputs list reaches
# the context at all — every other validator ignores it.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/17-context-inspecting/_ctx-helper.sh"
NAME="$(zoo_name)"
zoo_require_devnet
ctx_spend inputs-outputs-are-not-empty-with-datum inline '{"int": 42}' accept "$NAME"
