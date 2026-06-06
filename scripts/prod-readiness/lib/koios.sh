#!/usr/bin/env bash
# koios.sh <network> <endpoint> [json-body]  — authoritative Koios REST per network.
#
# The Koios *MCP* cannot be trusted for network selection: it was observed serving
# Preview (epoch 1320) when preprod (epoch 293) was expected, which silently breaks
# byte-exact ground-truth comparison. Always reach ground truth through this wrapper,
# which pins the correct per-network REST host.
#
# Examples:
#   koios.sh preprod tip
#   koios.sh preprod pool_history '{"_pool_bech32":"pool1...","_epoch_no":57}'
#   koios.sh mainnet account_reward_history '{"_stake_addresses":["stake1..."]}'
# shellcheck source=scripts/prod-readiness/lib/common.sh disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"

net="${1:?network: mainnet|preprod|preview}"
endpoint="${2:?endpoint, e.g. pool_history}"
# NB: do not write ${3:-{}} — the brace closes the expansion early and appends a
# stray } to the body. Default explicitly instead.
body="${3:-}"
[ -z "$body" ] && body='{}'

case "$net" in
  mainnet) base="https://api.koios.rest/api/v1" ;;
  preprod) base="https://preprod.koios.rest/api/v1" ;;
  preview) base="https://preview.koios.rest/api/v1" ;;
  *) die "unknown network: $net (want mainnet|preprod|preview)" ;;
esac

curl -s "$base/$endpoint" \
  -H 'accept: application/json' -H 'content-type: application/json' \
  -d "$body"
