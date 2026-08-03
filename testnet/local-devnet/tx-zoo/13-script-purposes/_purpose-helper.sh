#!/usr/bin/env bash
# Shared helpers for 13-script-purposes.
#
# WHY THIS CATEGORY EXISTS (#955)
# -------------------------------
# Before it, the zoo exercised exactly two of Conway's six Plutus
# ScriptPurposes. Reading the redeemer tags back off every signed transaction in
# the tree confirmed it: every redeemer was Spending (0) or Minting (1);
# Certifying (2), Rewarding (3), Voting (4) and Proposing (5) appeared nowhere.
#
# Each purpose is a distinct ScriptContext construction path and a distinct
# redeemer-pointer tag on the wire — exactly the class where #772 lived (bugs
# found only by diffing an independent cardano-ledger context dump). The devnet
# gate could not have caught a regression in any of the four.

# assert_purpose <signed-tx-file> <PurposeName>
#
# Proves the transaction we are about to submit ACTUALLY carries a redeemer of
# the named purpose. Without this a test could build something else entirely —
# a key-credentialed certificate, say — be accepted, and report PASS while
# never constructing the purpose it claims to cover. That is the failure shape
# this whole backlog exists to remove, so the assertion is on the bytes, not on
# our intent.
assert_purpose() {
    local signed="$1" want="$2" name="${3:-$NAME}"
    local got
    if ! got=$(python3 "$ZOO_LIB/tx-cbor-tool.py" redeemers --in "$signed" 2>&1); then
        zoo_fail "$name: could not read redeemers: $got"
        return 1
    fi
    if ! printf '%s\n' "$got" | grep -q "[[:space:]]${want}$"; then
        zoo_fail "$name: no $want redeemer on the wire (found: ${got//$'\n'/, })"
        return 1
    fi
    zoo_info "  wire check: $want redeemer present ($(printf '%s' "$got" | tr '\n' ';'))"
    return 0
}

# script_stake_addr <wallet>  — the stake address built from the script cred
script_stake_addr() { cat "$ZOO_KEYS/$1/stake.addr"; }
# script_pay_addr <wallet>    — base addr: key payment + script stake
script_pay_addr()   { cat "$ZOO_KEYS/$1/payment-stake.addr"; }
# script_file <wallet>        — the guarding script envelope
script_file()       { echo "$ZOO_KEYS/$1/stake-script.plutus"; }

# is_registered <stake-addr> [timeout_s] -> "yes"|"no"
#
# Polls rather than asking once. 13a can report PASS on all three observers
# (its UTxO is visible everywhere) while `query stake-address-info` on the same
# socket still answers empty a moment later — and 13b/13d then skip themselves
# with "not-registered", silently removing the Certifying-purpose coverage this
# category exists to provide. A flake that disguises itself as a legitimate
# precondition skip is the most expensive kind, so wait for the state instead
# of sampling it once.
is_registered() {
    local addr="$1" timeout="${2:-20}" i=0 r
    while [ "$i" -lt "$timeout" ]; do
        r=$(cardano-cli conway query stake-address-info \
                --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
                --address "$addr" 2>/dev/null \
            | jq -r 'if length>0 then "yes" else "no" end' 2>/dev/null || echo "no")
        [ "$r" = "yes" ] && { echo yes; return 0; }
        sleep 1
        i=$((i+1))
    done
    echo no
}

# A trivial redeemer. The always-true/false validators ignore it entirely;
# what matters is that a redeemer EXISTS, because that is what forces the
# ledger to build the ScriptPurpose and the node to evaluate it.
write_redeemer() { echo '{"int": 0}' > "$1"; }

# wait_all_strict <txid> [timeout] [addr]
#
# Like zoo_wait_all_observers, but REQUIRES the Haskell node to have the tx.
#
# zoo_wait_all_observers soft-passes on "2/3 observers (cbp lagging)". For most
# of the zoo that is a reasonable latency allowance. For THIS category it is
# fatal to the point of the test: these scripts exist to prove that Haskell
# accepts the same script-purpose transactions dugite does, so a pass that
# cardano-bp never confirmed measures nothing.
#
# That is not hypothetical. 13a originally submitted an unwitnessed
# script-credential registration, reported PASS off the soft-pass path, and was
# in fact being REJECTED by cardano-node the whole time
# (MissingScriptWitnessesUTXOW). The soft-pass is exactly what hid a genuine
# accept-set divergence.
wait_all_strict() {
    local txid="$1" timeout="${2:-120}" addr="${3:-}"
    [ -z "$addr" ] && addr="$(cat "$ZOO_PAY_ADDR_FILE")"
    local i=0
    while [ "$i" -lt "$timeout" ]; do
        local n=0 cbp=0
        for sock in "$LD_RELAY_SOCK" "$LD_DUGITE_BP_SOCK" "$LD_CARDANO_BP_SOCK"; do
            [ -S "$sock" ] || continue
            local hit
            hit=$(cardano-cli conway query utxo \
                    --testnet-magic "$LD_MAGIC" --socket-path "$sock" \
                    --address "$addr" --output-json 2>/dev/null \
                  | jq --arg t "$txid" '[keys[] | select(startswith($t))] | length' 2>/dev/null \
                  || echo 0)
            if [ "${hit:-0}" -ge 1 ]; then
                n=$((n+1))
                [ "$sock" = "$LD_CARDANO_BP_SOCK" ] && cbp=1
            fi
        done
        if [ "$n" -ge 3 ]; then
            zoo_ok "tx $txid on all 3 observers (Haskell confirmed) after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i+1))
    done
    zoo_fail "tx $txid NOT confirmed by all 3 observers after ${timeout}s (cardano-bp seen=$cbp) — this category requires Haskell confirmation, no soft-pass"
    return 1
}
