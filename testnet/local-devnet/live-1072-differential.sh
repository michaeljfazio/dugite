#!/usr/bin/env bash
# LIVE differential test of #1072 on the devnet.
#
# A RED-proven unit test bounds the FUNCTION, not the SYSTEM. This drives the
# real node through the real boundary and compares against cardano-node.
#
# Mechanism: dugite-bp is the SOLE forger in the standard topology, so stopping
# it halts ALL block production. Stop it before the epoch's `4k/f` mark (slot
# 320 of 400) and restart after the epoch end; no block lands in
# (320, 400], so Haskell's `nesRu` stays SNothing and the boundary — which
# fires on the first block after restart — must apply NO reward update on
# EITHER node.
#
# Before #1072 dugite moved pots here while cardano-node did not.
cd /Users/michaelfazio/Source/dugite/.claude/worktrees/nonmyopic-1067/testnet/local-devnet || exit 1
. ./lib/common.sh
set +e
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

MAGIC=42
DBP=$LD_DUGITE_BP_SOCK
CBP=$LD_CARDANO_BP_SOCK

pots() { # socket -> "treasury reserves"
    timeout 40 cardano-cli query ledger-state --testnet-magic $MAGIC --socket-path "$1" 2>/dev/null \
      | jq -r '(.stateBefore.esChainAccountState // .esChainAccountState)
               | "\(.treasury) \(.reserves)"'
}
slot_of() { timeout 20 cardano-cli query tip --testnet-magic $MAGIC --socket-path "$1" 2>/dev/null | jq -r '.slot // empty'; }
epoch_of() { timeout 20 cardano-cli query tip --testnet-magic $MAGIC --socket-path "$1" 2>/dev/null | jq -r '.epoch // empty'; }

echo "=== setup ==="
./setup.sh 2>&1 | tail -3 || exit 1
./run.sh   2>&1 | tail -4 || exit 1
sleep 20

# Wait until we are inside epoch 0 but BEFORE the 4k/f mark (slot 320).
echo "=== waiting for a safe stop point (slot < 300) ==="
for _ in $(seq 1 200); do
    s=$(slot_of "$DBP"); [ -n "$s" ] || { sleep 2; continue; }
    [ "$s" -ge 250 ] && break
    sleep 2
done
s=$(slot_of "$DBP"); e=$(epoch_of "$DBP")
echo "stopping dugite-bp at slot=$s epoch=$e (mark is 320, epoch end 400)"
[ "$s" -lt 315 ] || { echo "INCONCLUSIVE: overshot the stop window (slot=$s)"; ./stop.sh >/dev/null 2>&1; exit 1; }

T_BEFORE_D=$(pots "$DBP"); T_BEFORE_C=$(pots "$CBP")
echo "pots BEFORE  dugite=[$T_BEFORE_D]  cardano=[$T_BEFORE_C]"

# SIGTERM, never SIGKILL.
kill "$(cat state/dugite-bp.pid)" 2>/dev/null
for _ in $(seq 1 60); do kill -0 "$(cat state/dugite-bp.pid)" 2>/dev/null || break; sleep 1; done
echo "dugite-bp stopped; no forger => no blocks across the window"

# Sleep past the epoch end (slot 400) with margin.
sleep 140

echo "=== restarting dugite-bp ==="
../../.claude/skills/devnet-validate/scripts/restart-dugite-bp.sh 2>&1 | tail -3
# The first block after restart lands in epoch 1 and triggers the boundary.
for _ in $(seq 1 90); do
    e=$(epoch_of "$DBP"); [ -n "$e" ] && [ "$e" -ge 1 ] && break
    sleep 2
done
sleep 25

S_AFTER=$(slot_of "$DBP"); E_AFTER=$(epoch_of "$DBP")
T_AFTER_D=$(pots "$DBP"); T_AFTER_C=$(pots "$CBP")
echo "after boundary: slot=$S_AFTER epoch=$E_AFTER"
echo "pots AFTER   dugite=[$T_AFTER_D]  cardano=[$T_AFTER_C]"

echo "=== verdict ==="
rc=0
[ -n "$T_AFTER_D" ] && [ -n "$T_AFTER_C" ] || { echo "INCONCLUSIVE: pots unreadable"; ./stop.sh >/dev/null 2>&1; exit 1; }
[ "${E_AFTER:-0}" -ge 1 ] || { echo "INCONCLUSIVE: boundary never crossed"; ./stop.sh >/dev/null 2>&1; exit 1; }

if [ "$T_AFTER_D" = "$T_AFTER_C" ]; then
    echo "PASS parity: dugite and cardano-node agree on pots after the no-pulser boundary"
else
    echo "FAIL parity: dugite=[$T_AFTER_D] cardano=[$T_AFTER_C]"; rc=1
fi
if [ "$T_AFTER_D" = "$T_BEFORE_D" ]; then
    echo "PASS no-op: dugite applied NO reward update (pots unchanged)"
else
    echo "FAIL no-op: dugite moved pots [$T_BEFORE_D] -> [$T_AFTER_D] at a boundary with no pulser"; rc=1
fi

echo "=== dugite log evidence ==="
grep -c "No RUPD pulser for the closed epoch" logs/dugite-bp.log 2>/dev/null || echo 0

./stop.sh >/dev/null 2>&1
echo "LIVE-1072 rc=$rc"
exit $rc
