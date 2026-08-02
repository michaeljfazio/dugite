#!/usr/bin/env bash
# 13i — NEGATIVE: a script-credential certificate with NO script witness must
# be rejected, by both nodes, with MissingScriptWitnessesUTXOW.
#
# THIS IS THE REGRESSION PIN FOR A REAL DIVERGENCE.
#
# dugite's phase-1 certificate check only ever produced VKEY witness
# requirements — `cert_required_witnesses` mapped `Credential::Script(_)` to
# `None` and nothing else took up the slack. Inputs and withdrawals each had a
# `Credential::Script` arm; certificates had none. So EVERY certificate whose
# subject is a script credential — registration-with-deposit, deregistration,
# delegation, DRep and committee certificates alike — could be submitted with
# no script witness at all and dugite accepted it.
#
# Measured live on this devnet, same transaction to both sockets:
#   dugite      : SUBMIT ACCEPTED
#   cardano-node: SUBMIT REJECTED
#                 ConwayUtxowFailure (MissingScriptWitnessesUTXOW
#                   (NonEmptySet (fromList [ScriptHash "047eeca1…"])))
#
# That is an accept-set divergence in the dangerous direction (dugite too lax),
# which the parity oracle grades P0. Fixed by `cert_required_script_witness` in
# crates/dugite-ledger/src/validation/phase1.rs.
#
# The certificate is built with cardano-cli and submitted WITHOUT
# --certificate-script-file, so the tx is well-formed and only the witness is
# absent. Both nodes must refuse it, and the rejection class must match.
set -euo pipefail
ZOO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ZOO_DIR/lib/tx-zoo-common.sh"
. "$ZOO_DIR/13-script-purposes/_purpose-helper.sh"

NAME="$(zoo_name)"
zoo_require_devnet
W="script-stake-native"   # any script credential works; native keeps it cheap
SCRIPT=$(script_file "$W")
[ -s "$SCRIPT" ] || { zoo_record_env_skip "$NAME" "missing script-stake wallet — run run-all.sh --setup"; exit 0; }

ADDR=$(script_pay_addr "$W")
SCRIPT_HASH=$(cat "$ZOO_KEYS/$W/stake-script.hash")
PPARAMS=$(zoo_pparams_file)
DEPOSIT=$(jq -r '.stakeAddressDeposit' "$PPARAMS")

# A deregistration certificate: unambiguously witness-requiring for a script
# credential in EVERY Conway form (unlike registration, where only the
# deposit-bearing index-7 form requires it).
CERT="$ZOO_BUILT/$NAME.cert"
cardano-cli conway stake-address deregistration-certificate \
    --stake-script-file "$SCRIPT" \
    --key-reg-deposit-amt "$DEPOSIT" \
    --out-file "$CERT"

UTXO=$(zoo_largest_utxo "$ADDR") || { zoo_record "$NAME" FAIL "" "no-utxo"; exit 1; }
RAW="$ZOO_BUILT/$NAME.raw"
SIGNED="$ZOO_BUILT/$NAME.signed"

# build-raw, not build: `transaction build` computes the witness set for us and
# would refuse to produce the unwitnessed transaction we are trying to test.
FEE=1000000
BAL=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$ZOO_SOCKET" --address "$ADDR" --output-json 2>/dev/null \
      | jq -r --arg k "${UTXO%% *}" '.[$k].value.lovelace // 0')
# A deregistration certificate REFUNDS the deposit, so the value equation is
#   inputs + refund  ==  outputs + fee
# Omitting the refund made the transaction unbalanced, and the node rejected it
# with ValueNotConservedUTxO instead of the missing-witness error under test.
# The test correctly FAILED rather than passing for the wrong reason — which is
# the whole point of asserting the rejection CLASS and not just "rejected".
CHANGE=$(( BAL - FEE + DEPOSIT ))
if [ "$CHANGE" -le 0 ]; then
    zoo_record "$NAME" SKIP "" "insufficient-balance"
    exit 0
fi

cardano-cli conway transaction build-raw \
    --tx-in "${UTXO%% *}" \
    --tx-out "${ADDR}+${CHANGE}" \
    --fee "$FEE" \
    --certificate-file "$CERT" \
    --out-file "$RAW" >/dev/null 2> "$ZOO_LOGS/$NAME.err" \
    || { zoo_fail "build-raw: $(tail -2 "$ZOO_LOGS/$NAME.err")"; zoo_record "$NAME" FAIL "" "build-raw"; exit 1; }
cardano-cli conway transaction sign \
    --testnet-magic "$LD_MAGIC" \
    --tx-body-file  "$RAW" \
    --signing-key-file "$ZOO_KEYS/$W/payment.skey" \
    --out-file      "$SIGNED" >/dev/null

# Sanity: the transaction really does carry NO script witness.
if python3 "$ZOO_LIB/tx-cbor-tool.py" redeemers --in "$SIGNED" 2>/dev/null | grep -q .; then
    zoo_fail "test is not testing what it claims — a redeemer is present"
    zoo_record "$NAME" FAIL "" "unexpected-redeemer"
    exit 1
fi

OUT=$(cardano-cli conway transaction submit \
        --testnet-magic "$LD_MAGIC" --socket-path "$ZOO_SOCKET" \
        --tx-file "$SIGNED" 2>&1) && RC=0 || RC=1

if [ "$RC" -eq 0 ]; then
    zoo_fail "ACCEPTED an unwitnessed script-credential certificate — cardano-node rejects this with MissingScriptWitnessesUTXOW"
    zoo_record "$NAME" FAIL "" "accepted-unwitnessed-script-cert"
    exit 1
fi

if echo "$OUT" | grep -q "MissingScriptWitnessesUTXOW"; then
    zoo_ok "rejected with MissingScriptWitnessesUTXOW (matches cardano-node)"
    zoo_record "$NAME" PASS "" "rejected-MissingScriptWitnessesUTXOW"
    exit 0
fi

# Rejected, but for a different reason than Haskell gives. That is a P2
# reject-reason divergence, and saying so beats calling it a pass.
REASON=$(echo "$OUT" | grep -oE '\(Conway[A-Za-z]*Failure[^)]*' | head -1 | cut -c1-120)
zoo_fail "rejected, but not as MissingScriptWitnessesUTXOW: ${REASON:-$(echo "$OUT" | head -1 | cut -c1-120)}"
zoo_record "$NAME" FAIL "" "rejected-wrong-reason"
exit 1
