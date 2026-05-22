#!/usr/bin/env bash
# 08f — Rule 1b: duplicate input in the same transaction must be rejected.
# Skipped: cardano-cli conway transaction build-raw deduplicates `--tx-in`
# entries (CBOR input set has length 1 even when --tx-in is passed twice),
# so the tx that reaches the node never has duplicate inputs. To exercise
# this rule we'd have to construct CBOR by hand and submit via raw N2C, which
# belongs in the adversarial harness (protocols/), not here.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/tx-zoo-common.sh"

NAME="$(zoo_name)"
zoo_record "$NAME" SKIP "" "cardano-cli-build-raw-dedupes-inputs"
