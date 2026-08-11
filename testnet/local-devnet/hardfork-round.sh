#!/usr/bin/env bash
# hardfork-round.sh — enact a real HardForkInitiation PV10 -> PV11 on a
# throwaway devnet, standard 3-node topology (dugite-relay + dugite-bp +
# cardano-bp; two-forger is NOT required for this round). (#1042)
#
# TERMINAL ROUND
# --------------
# Same shape as gov-enactment-round.sh's NoConfidence/HardForkInitiation
# caveat: `delayingAction` is True for HardForkInitiation, and once it
# enacts the chain is running under a NEW protocol version for the rest of
# the devnet's life — every era-behaviour switch gated on PV is now live.
# This round therefore owns its OWN throwaway devnet end to end (fresh
# ./setup.sh at step 0, ./stop.sh at step 7) and must never be chained after
# another round expecting to stay at PV10, nor followed by one that assumes
# it.
#
# PRE-FLIGHT FINDINGS (static code read, baked into the assertions below —
# see the issue for the full trace)
# -----------------------------------------------------------------------
# dugite-ledger already carries five PV11 arms that are DEAD on every real
# network today (PV10 everywhere) and only reachable once a chain actually
# crosses this boundary — this round is what exercises them for the first
# time on live consensus rather than in a unit test:
#
#   1. crates/dugite-ledger/src/validation/phase1.rs:903 — the DELEG
#      deposit-mismatch constructor is `IncorrectDepositDELEG` at PV 9-10
#      and `DepositIncorrectDELEG` (the `Mismatch`-carrying form) at
#      PV>=11. This is 16e's PV inversion (#979) — steps 1 and 5 pin BOTH
#      arms explicitly, in the same round, on the same live chain.
#   2. crates/dugite-ledger/src/validation/mod.rs (~3995-4014) — Conway
#      cert tag 0 (deposit-less legacy stake registration) is valid at
#      PV10 and rejected at PV>=11. Not separately exercised here (no
#      script targets it yet); recorded for the next round that does.
#   3. crates/dugite-ledger/src/validation/phase1.rs:1606-1631 —
#      `BabbageNonDisjointRefInputs` only fires for 8<PV<11, so PV11
#      ACCEPTS the ref-input/spent-input overlap for non-V3 txs. tx-zoo
#      18f's constructor arm INVERTS at PV11 — 18-plutus-edges is
#      therefore deliberately excluded from the post-HF smoke in step 6.
#      Do not add it back without first flipping 18f's expectation.
#   4. crates/dugite-ledger/src/validation/conway.rs:239-263 — PPU
#      nOpt=0 rejection + strict CostModels structural validation are
#      both PV>=11 only.
#   5. crates/dugite-ledger/src/validation/mod.rs:2605-2614 —
#      `UnelectedCommitteeVoters` is PV>=11 only.
#   6. crates/dugite-ledger/src/eras/conway.rs — the pvMajor-11 arm of the
#      intra-era HARDFORK rule, `populateVRFKeyHashes` (#1085). THE
#      BOUNDARY THIS ROUND CROSSES IS THE ONLY PLACE IT EVER RUNS: it
#      seeds `psVRFKeyHashes` from every current and future pool, once,
#      and every later duplicate-VRF verdict reads what it produced. A
#      unit test can drive the function; only this round drives it from a
#      real ratified HardForkInitiation with cardano-node watching.
#   7. crates/dugite-ledger/src/eras/conway.rs — `totalRefScriptSizeInBlock`
#      switches from measuring every tx against the BLOCK-INITIAL UTxO to
#      an accumulating fold at PV>=11 (#1086). Both arms decide BLOCK
#      validity, so a divergence here is a forge that Haskell peers
#      reject; steps before and after the fork exercise one arm each.
#
# The live half of the pre-flight — does cardano-node 11.0.1 actually
# ratify a PV11 HardForkInitiation at all — is NOT something a static read
# can answer; it is exactly what this round exercises. If ratification or
# enactment fails on the throwaway devnet, that is the NO-GO signal for
# shipping PV11 support: treat any FAIL from step 4 (the PV flip assertion)
# as blocking, not as a harness flake.
#
# NO CONFIG INJECTION — PV11 IS NATIVE ON cardano-node 11.0.1
# ----------------------------------------------------
# PRE-FLIGHT RESOLVED (issue #1042 step 2): PV10->PV11 is an INTRA-Conway
# HardForkInitiation (PV11 is still the Conway era), which cardano-node 11.0.1
# enacts natively — preview mainnet has run PV11 on 11.0.1 since before this
# round was written.
#
# An earlier revision of this round flipped ExperimentalHardForksEnabled to
# `true` in the rendered cardano configs as "belt-and-braces". That is WRONG on
# 11.0.1 and was caught on the first live run: enabling experimental hard forks
# makes cardano-node require a DijkstraGenesisFile (the NEXT, experimental era
# after Conway) and refuse to start with `key "DijkstraGenesisFile" not found`.
# The flag gates the *next* era, not an intra-Conway PV bump. So this round now
# leaves the rendered configs at their template default
# (ExperimentalHardForksEnabled=false) — the exact config every other round
# starts cardano-bp with.
#
# WHAT THIS ROUND REUSES, AND FROM WHERE
# ---------------------------------------
# `vote_all`, `delegate_votes_to_drep`, `wait_boundaries`, `cur_epoch`, and
# the anchor plumbing are COPIED from gov-enactment-round.sh (same
# directory) rather than sourced — that script is a standalone round, not a
# library. Any bugfix landing there should be mirrored here by hand.
# `treasury_of`/`reward_of` are NOT copied — HardForkInitiation carries no
# treasury gate (unlike TreasuryWithdrawals, whose ratification reads a
# frozen `ensTreasury`), so this round has nothing for them to check.
# The HardForkInitiation proposal-build shape (governance action
# create-hardfork, --protocol-major-version/--protocol-minor-version, no
# --prev-governance-action-id — there is no earlier hardfork action on a
# fresh devnet to chain from) is copied from
# tx-zoo/06-proposals/06c-hard-fork-initiation.sh's logic and inlined here
# (matching how gov-enactment-round inlines 06d's TreasuryWithdrawals logic
# rather than shelling out to it) so this round can capture the resulting
# txid directly instead of scraping it back out of results.csv.
# tx-zoo/16-cert-negatives/16e-stake-registration-wrong-deposit.sh is run
# UNMODIFIED, twice, as a real subprocess (ZOO_SOCKET + ZOO_RESULTS_CSV
# overrides give each run its own isolated results file) — it already
# branches on the LIVE protocol version internally, but this round pins the
# EXPECTED constructor independently at each call site rather than trusting
# 16e's own branch, so a bug in 16e's own PV read cannot mask a real
# divergence.
#
# HFI GUARDRAILS-HASH CHECK (asked for explicitly — verified, not assumed)
# --------------------------------------------------------------------------
# `cardano-cli conway governance action create-hardfork` takes only
# --protocol-major-version/--protocol-minor-version plus the standard
# deposit/anchor/testnet flags — no guardrails-script-hash flag exists for
# this action type (that field is unique to ParameterChange, since only a
# ParameterChange can touch guardrail-governed parameters). Confirmed by
# reading 06c's invocation, which passes none, and by cardano-cli's own
# --help for the subcommand. This round therefore needs no genesis
# guardrails setup at all.
#
# Usage:
#   ./hardfork-round.sh [--skip-setup]
set +e
[ -n "${ZSH_VERSION:-}" ] && { unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true; }

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 2
SKIP_SETUP=0
[ "${1:-}" = "--skip-setup" ] && SKIP_SETUP=1

step() { echo; echo "########## $* ##########"; date -u +%H:%M:%SZ; }
FAILURES=0

# record <step-id> <outcome PASS|FAIL|NOTE> <detail...>
# Evidence CSV: $LD_EVIDENCE/<ts>/hardfork-round.csv, columns
# ts,step,outcome,detail — written by every ok/bad/note call below.
record() {
    local sid="$1" outcome="$2"; shift 2
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$sid,$outcome,${*//,/;}" >> "$CSV"
}
ok()   { printf '\033[0;32m[PASS]\033[0m %s: %s\n' "$1" "${*:2}"; record "$1" PASS "${*:2}"; }
bad()  { printf '\033[0;31m[FAIL]\033[0m %s: %s\n' "$1" "${*:2}"; record "$1" FAIL "${*:2}"; FAILURES=$((FAILURES+1)); }
note() { printf '\033[0;33m[NOTE]\033[0m %s: %s\n' "$1" "${*:2}"; record "$1" NOTE "${*:2}"; }

# ─────────────────────────────────────────────────────────────────────────
step "0. fresh devnet (no config injection — PV11 HFI is native on cardano-node 11.0.1)"
# ─────────────────────────────────────────────────────────────────────────
if [ "$SKIP_SETUP" -eq 0 ]; then
    ./stop.sh  >/dev/null 2>&1
    ./setup.sh >/dev/null 2>&1 || { echo "SETUP FAILED"; exit 2; }
fi
. ./lib/common.sh
set +e

TS="$(date -u +%Y%m%dT%H%M%SZ)"
EVID="$LD_EVIDENCE/$TS"
mkdir -p "$EVID"
CSV="$EVID/hardfork-round.csv"
echo "ts,step,outcome,detail" > "$CSV"

if [ "$SKIP_SETUP" -eq 0 ]; then
    # PRE-FLIGHT RESOLVED (issue #1042 step 2): PV10->PV11 is an INTRA-Conway
    # HardForkInitiation — PV11 is still the Conway era, not a new one — and
    # cardano-node 11.0.1 enacts it NATIVELY (preview mainnet has run PV11 on
    # 11.0.1 since before this round was written). ExperimentalHardForksEnabled
    # is NOT needed and is actively HARMFUL on 11.0.1: setting it true makes
    # cardano-node require a DijkstraGenesisFile (the NEXT, experimental era)
    # and refuse to start ("key DijkstraGenesisFile not found"). So the round
    # leaves the rendered configs at their template default (flag=false) — the
    # same config every other round starts cardano-bp with.
    note "0-inject-flag" "no config injection — PV11 HFI is intra-Conway and native on cardano-node 11.0.1 (ExperimentalHardForksEnabled would pull in the Dijkstra era and break startup)"
    ok "0-inject-flag" "rendered configs left at template default (ExperimentalHardForksEnabled=false)"

    ./run.sh >/dev/null 2>&1 || { bad "0-run" "RUN FAILED"; exit 2; }
fi

# Copied from gov-enactment-round.sh — socket + first-blocks wait.
for i in $(seq 1 40); do sleep 2; [ -S "$LD_RELAY_SOCK" ] && break; done
for i in $(seq 1 40); do
    sleep 3
    B=$(cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" 2>/dev/null | jq -r '.block // 0')
    [ "${B:-0}" -ge 5 ] && break
done
if [ -S "$LD_RELAY_SOCK" ] && [ "${B:-0}" -ge 5 ]; then
    ok "0-devnet-up" "relay socket ready, tip block=$B"
else
    bad "0-devnet-up" "relay socket/blocks never became ready (block=${B:-0}) — aborting"
    exit 2
fi

# ---- helpers (cur_epoch/pv_major/wait_boundaries/vote_all/delegate_votes_to_drep
#      are copies from gov-enactment-round.sh in this same directory) ----

cur_epoch() { cardano-cli query tip --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null | jq -r '.epoch // 0'; }
pv_major()  { cardano-cli conway query protocol-parameters --testnet-magic "$LD_MAGIC" --socket-path "$1" 2>/dev/null | jq -r '.protocolVersion.major // empty'; }

# wait_boundaries N — block until the epoch has advanced by N.
# COPIED VERBATIM from gov-enactment-round.sh.
wait_boundaries() {
    local n="$1" start now deadline
    start=$(cur_epoch "$LD_RELAY_SOCK")
    deadline=$(( n * 500 ))   # 400-slot epochs at 1s, plus slack
    echo "  waiting $n epoch boundary/ies from epoch $start (up to ${deadline}s)"
    local i=0
    while [ "$i" -lt "$deadline" ]; do
        now=$(cur_epoch "$LD_RELAY_SOCK")
        if [ "$(( now - start ))" -ge "$n" ]; then
            echo "  reached epoch $now after ${i}s"
            return 0
        fi
        sleep 5; i=$((i+5))
    done
    echo "  TIMEOUT: still at epoch $(cur_epoch "$LD_RELAY_SOCK")"
    return 1
}

# delegate_votes_to_drep — COPIED VERBATIM from gov-enactment-round.sh.
# Puts real stake behind drep-1 via the genesis stake delegators, whose
# credentials are already registered on-chain and who hold the entire
# delegated supply — the only thing that matters, since dRepAcceptedRatio
# folds over the stake DISTRIBUTION, never over who cast a ballot.
delegate_votes_to_drep() {
    local d="$LD_GENESIS/stake-delegators/delegator1"
    local drep="tx-zoo/state/keys/drep-1/drep.vkey"
    local tmp="$LD_STATE/gov-vote-deleg"
    mkdir -p "$tmp"
    [ -f "$d/staking.skey" ] || { echo "genesis delegator keys absent — DRep would have zero power"; return 1; }
    [ -f "$drep" ] || { echo "drep-1 vkey absent — run tx-zoo --setup first"; return 1; }

    local ent base addr txin=""
    cardano-cli conway address build --payment-verification-key-file "$d/payment.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$tmp/ent.addr" 2>/dev/null
    cardano-cli conway address build --payment-verification-key-file "$d/payment.vkey" \
        --stake-verification-key-file "$d/staking.vkey" \
        --testnet-magic "$LD_MAGIC" --out-file "$tmp/base.addr" 2>/dev/null
    ent=$(cat "$tmp/ent.addr" 2>/dev/null)
    base=$(cat "$tmp/base.addr" 2>/dev/null)
    for a in "$base" "$ent"; do
        [ -z "$a" ] && continue
        txin=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
                 --address "$a" --output-json 2>/dev/null \
               | jq -r 'to_entries | max_by(.value.value.lovelace) | .key // empty')
        [ -n "$txin" ] && { addr="$a"; break; }
    done
    [ -z "$txin" ] && { echo "no UTxO at either genesis delegator address (base=$base ent=$ent)"; return 1; }

    cardano-cli conway stake-address vote-delegation-certificate \
        --stake-verification-key-file "$d/staking.vkey" \
        --drep-verification-key-file "$drep" \
        --out-file "$tmp/vote.cert" 2>"$tmp/err" || { echo "vote-delegation cert build failed: $(tail -2 "$tmp/err")"; return 1; }

    cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
        --tx-in "$txin" --change-address "$addr" \
        --certificate-file "$tmp/vote.cert" --out-file "$tmp/raw" 2>>"$tmp/err" \
    && cardano-cli conway transaction sign --tx-body-file "$tmp/raw" \
        --signing-key-file "$d/payment.skey" --signing-key-file "$d/staking.skey" \
        --testnet-magic "$LD_MAGIC" --out-file "$tmp/signed" 2>>"$tmp/err" \
    && cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" --tx-file "$tmp/signed" >/dev/null 2>>"$tmp/err" \
    || { echo "vote-delegation submit failed: $(tail -3 "$tmp/err" | tr '\n' ' ')"; return 1; }

    echo "genesis delegator1 stake vote-delegated to drep-1"
    sleep 8
    return 0
}

# vote_all <action-id> <tag> [roles] — COPIED VERBATIM from
# gov-enactment-round.sh. roles here is ALWAYS "drep,spo,cc" for
# HardForkInitiation (SPO=yes, CC=yes, DRep=yes per
# Governance/Internal.hs's voter-eligibility table — an ineligible role is
# a hard DisallowedVoters rejection of the WHOLE vote tx, not merely an
# uncounted ballot).
vote_all() {
    local action_id="$1" tag="$2" roles="${3:-drep,cc}"
    local tx="${action_id%#*}" ix="${action_id#*#}"
    local votes=() signs=()
    local D="tx-zoo/state/keys/drep-1"
    if [[ ",$roles," == *",drep,"* ]]; then
        cardano-cli conway governance vote create --yes \
            --governance-action-tx-id "$tx" --governance-action-index "$ix" \
            --drep-verification-key-file "$D/drep.vkey" \
            --out-file "$ZOO_TMP/$tag-drep.vote" 2>/dev/null \
            && { votes+=(--vote-file "$ZOO_TMP/$tag-drep.vote"); signs+=(--signing-key-file "$D/drep.skey"); }
    fi
    if [[ ",$roles," == *",spo,"* ]]; then
        cardano-cli conway governance vote create --yes \
            --governance-action-tx-id "$tx" --governance-action-index "$ix" \
            --cold-verification-key-file "$LD_KEYS/pool1/cold.vkey" \
            --out-file "$ZOO_TMP/$tag-spo.vote" 2>/dev/null \
            && { votes+=(--vote-file "$ZOO_TMP/$tag-spo.vote"); signs+=(--signing-key-file "$LD_KEYS/pool1/cold.skey"); }
    fi
    local CC="tx-zoo/state/keys/cc-2"
    if [[ ",$roles," == *",cc,"* ]] && [ -s "$CC/cc-hot.vkey" ]; then
        cardano-cli conway governance vote create --yes \
            --governance-action-tx-id "$tx" --governance-action-index "$ix" \
            --cc-hot-verification-key-file "$CC/cc-hot.vkey" \
            --out-file "$ZOO_TMP/$tag-cc.vote" 2>/dev/null \
            && { votes+=(--vote-file "$ZOO_TMP/$tag-cc.vote"); signs+=(--signing-key-file "$CC/cc-hot.skey"); }
    fi
    [ ${#votes[@]} -eq 0 ] && { echo "no votes could be created"; return 1; }

    local u; u=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
        --socket-path "$LD_RELAY_SOCK" --address "$ADDR" --output-json 2>/dev/null \
        | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
    if ! cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-in "$u" --change-address "$ADDR" \
            "${votes[@]}" --out-file "$ZOO_TMP/$tag-votes.raw" 2>"$ZOO_TMP/$tag-build.err"; then
        echo "vote BUILD failed: $(grep -m1 -E 'Error|Failure' "$ZOO_TMP/$tag-build.err" | cut -c1-180)"
        return 1
    fi
    cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" \
        --tx-body-file "$ZOO_TMP/$tag-votes.raw" \
        --signing-key-file "$WA/payment.skey" "${signs[@]}" \
        --out-file "$ZOO_TMP/$tag-votes.signed" 2>"$ZOO_TMP/$tag-sign.err" || {
            echo "vote SIGN failed: $(head -1 "$ZOO_TMP/$tag-sign.err" | cut -c1-180)"; return 1; }
    if ! SUBV=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
            --socket-path "$LD_RELAY_SOCK" --tx-file "$ZOO_TMP/$tag-votes.signed" 2>&1); then
        echo "vote SUBMIT rejected: $(echo "$SUBV" | grep -m1 -E 'Error|Failure' | cut -c1-180)"
        return 1
    fi
    echo "cast ${#votes[@]} vote(s) on $action_id"
    return 0
}

# run_16e <step-id> <expected-pv> <csv-out> — run 16e as an isolated
# subprocess (own ZOO_RESULTS_CSV) and independently pin the constructor
# this round EXPECTS at the given PV, rather than trusting only 16e's own
# live-PV branch (see header).
run_16e() {
    local sid="$1" want_pv="$2" csv="$3" want
    if [ "$want_pv" -ge 11 ]; then want="DepositIncorrectDELEG"; else want="IncorrectDepositDELEG"; fi
    rm -f "$csv"
    ZOO_SOCKET="$LD_RELAY_SOCK" ZOO_RESULTS_CSV="$csv" \
        bash ./tx-zoo/16-cert-negatives/16e-stake-registration-wrong-deposit.sh \
        >"$EVID/16e-$sid.log" 2>&1
    local row status detail
    row=$(tail -n +2 "$csv" 2>/dev/null | tail -1)
    status=$(printf '%s' "$row" | awk -F, '{print $3}')
    detail=$(printf '%s' "$row" | awk -F, '{print $5}')
    if [ "$status" = "PASS" ] && printf '%s' "$detail" | grep -q "$want"; then
        ok "$sid" "16e observed $want at PV$want_pv (row: $row)"
    else
        bad "$sid" "16e expected $want at PV$want_pv but got status=$status detail=$detail (row: $row; log: $EVID/16e-$sid.log)"
    fi
}

# run_zoo_smoke <step-id> <socket> <csv-out> — run the post-HF negative/
# bookkeeping smoke against one socket, asserting zero FAIL rows.
# 18-plutus-edges is deliberately NOT included — see pre-flight finding 3.
run_zoo_smoke() {
    local sid="$1" sock="$2" csv="$3"
    rm -f "$csv"
    ZOO_SOCKET="$sock" ZOO_RESULTS_CSV="$csv" \
        ./tx-zoo/run-all.sh 01-bookkeeping 08-negative 16-cert-negatives \
        >"$EVID/zoo-smoke-$sid.log" 2>&1
    local total fails
    total=$(tail -n +2 "$csv" 2>/dev/null | wc -l | tr -d ' ')
    if [ -z "$total" ] || [ "$total" -eq 0 ]; then
        bad "$sid" "zoo smoke produced no rows at all — $csv empty; see $EVID/zoo-smoke-$sid.log"
        return
    fi
    fails=$(awk -F, 'NR>1 && $3=="FAIL"' "$csv" | wc -l | tr -d ' ')
    if [ "$fails" -eq 0 ]; then
        ok "$sid" "zoo smoke clean: $total rows, 0 FAIL (state-skips non-fatal) — $csv"
    else
        bad "$sid" "zoo smoke: $fails FAIL row(s) of $total — see $csv"
        awk -F, 'NR>1 && $3=="FAIL" {print "    " $2 ": " $5}' "$csv"
    fi
}

# classify_sampler <step-id> <log> <rc> — the two boundary-parity samplers
# both encode PASS/FAIL/INCONCLUSIVE as prefixed lines on stdout/stderr
# (captured together in <log>) with the SAME rc=1 for FAIL and
# INCONCLUSIVE alike (see their own headers) — the prefix, not the rc, is
# the source of truth. INCONCLUSIVE is recorded as its own NOTE outcome,
# never silently upgraded to PASS.
classify_sampler() {
    local sid="$1" log="$2" rc="$3"
    if grep -q '^FAIL:' "$log"; then
        bad "$sid" "sampler reported FAIL (rc=$rc): $(grep '^FAIL:' "$log" | tr '\n' ' ')"
    elif grep -q '^INCONCLUSIVE:' "$log"; then
        note "$sid" "sampler reported INCONCLUSIVE, not a hard failure (rc=$rc): $(grep '^INCONCLUSIVE:' "$log" | tr '\n' ' ')"
    elif [ "$rc" -eq 0 ]; then
        ok "$sid" "sampler PASS (rc=0): $(tail -1 "$log")"
    else
        bad "$sid" "sampler exited rc=$rc with no recognised FAIL/INCONCLUSIVE line — see $log"
    fi
}

# ─────────────────────────────────────────────────────────────────────────
step "1. zoo setup, then assert PV=10 on both sockets + run 16e pre-HF"
# ─────────────────────────────────────────────────────────────────────────
# The zoo --setup (keys + on-chain funding for wallet-a) MUST run before the
# pre-HF 16e: 16e builds a stake-registration tx from wallet-a, so without
# funding it fails at build time (`build-failed`) rather than reaching the
# IncorrectDepositDELEG constructor it asserts. An earlier revision ran the
# setup in step 2, after this 16e call — caught on the first live run.
if [ "$SKIP_SETUP" -eq 0 ]; then
    ./tx-zoo/run-all.sh --setup >/dev/null 2>&1
fi
PVD=$(pv_major "$LD_RELAY_SOCK")
PVH=$(pv_major "$LD_CARDANO_BP_SOCK")
echo "  protocolVersion.major: dugite(relay)=$PVD haskell(cardano-bp)=$PVH"
# RED-PROOF: this must observe 10/10 here. Flipping the expected literal
# below from 10 to 11 (asserting the PV11 state BEFORE any proposal has
# even been built) must FAIL on a freshly-injected devnet — there is no
# path for PV to be 11 this early.
if [ "$PVD" = "10" ] && [ "$PVH" = "10" ]; then
    ok "1-pv-pre" "PV major=10 on both sockets"
else
    bad "1-pv-pre" "expected PV10 on both sockets before proposing; got dugite=$PVD haskell=$PVH"
fi
run_16e "1-16e-pre-hf" 10 "$EVID/16e-pre-hf.csv"

# ─────────────────────────────────────────────────────────────────────────
step "2. DRep power + propose/vote HardForkInitiation PV11"
# ─────────────────────────────────────────────────────────────────────────
if [ "$SKIP_SETUP" -eq 0 ]; then
    ZOO_SOCKET="$LD_RELAY_SOCK" bash ./tx-zoo/04-stake/04a-stake-register.sh 2>&1 | tail -2
    ZOO_SOCKET="$LD_RELAY_SOCK" bash ./tx-zoo/05-governance-certs/05a-drep-register.sh 2>&1 | tail -1
    ZOO_SOCKET="$LD_RELAY_SOCK" bash ./tx-zoo/05-governance-certs/05g-cc-hot-key-authorization.sh 2>&1 | tail -1
    if delegate_votes_to_drep; then
        ok "2-drep-power" "genesis delegator1 stake vote-delegated to drep-1"
    else
        bad "2-drep-power" "delegate_votes_to_drep failed — HFI will be structurally unratifiable by DReps"
    fi
else
    note "2-drep-power" "--skip-setup: assuming zoo keys + DRep power already established by the caller"
fi

WA="tx-zoo/state/keys/wallet-a"
ADDR=$(cat "$WA/payment-stake.addr" 2>/dev/null)
if [ -z "$ADDR" ]; then
    bad "2-zoo-setup" "wallet-a address unavailable after zoo setup — cannot propose"
    ./stop.sh >/dev/null 2>&1
    exit 1
fi
PPARAMS=$(mktemp)
cardano-cli conway query protocol-parameters --testnet-magic "$LD_MAGIC" \
    --socket-path "$LD_RELAY_SOCK" --out-file "$PPARAMS" 2>/dev/null
GOV_DEPOSIT=$(jq -r '.govActionDeposit // 100000000000' "$PPARAMS")

ZOO_TMP=$(mktemp -d); trap 'rm -rf "$ZOO_TMP"' EXIT
. ./tx-zoo/lib/tx-zoo-common.sh
set +e
zoo_anchor_start >/dev/null 2>&1
ANCHOR_URL=$(zoo_anchor_url hardfork)
ANCHOR_HASH=$(zoo_anchor_hash hardfork)
[ -n "$ANCHOR_HASH" ] || { bad "2-anchor" "could not compute anchor hash — aborting"; ./stop.sh >/dev/null 2>&1; exit 2; }

# ---- propose HardForkInitiation PV 10 -> 11 (06c's build logic, inlined
#      per the header note) ----
ACTION="$ZOO_TMP/hfi.action"
cardano-cli conway governance action create-hardfork \
    --testnet \
    --governance-action-deposit "$GOV_DEPOSIT" \
    --deposit-return-stake-verification-key-file "$WA/stake.vkey" \
    --anchor-url "$ANCHOR_URL" --anchor-data-hash "$ANCHOR_HASH" \
    --protocol-major-version 11 \
    --protocol-minor-version 0 \
    --out-file "$ACTION" 2>"$ZOO_TMP/hfi.action.err"
if [ ! -s "$ACTION" ]; then
    bad "2-propose-hfi" "action-create failed: $(head -3 "$ZOO_TMP/hfi.action.err" | tr '\n' ' ')"
    HFI_PROPOSED=0
else
    U=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
          --address "$ADDR" --output-json 2>/dev/null | jq -r 'to_entries|sort_by(-.value.value.lovelace)|.[0].key // empty')
    if ! cardano-cli conway transaction build --testnet-magic "$LD_MAGIC" --socket-path "$LD_RELAY_SOCK" \
            --tx-in "$U" --change-address "$ADDR" --proposal-file "$ACTION" \
            --out-file "$ZOO_TMP/hfi.raw" >/dev/null 2>"$ZOO_TMP/hfi.build.err"; then
        bad "2-propose-hfi" "BUILD failed: $(grep -m1 -vE '^\s*$' "$ZOO_TMP/hfi.build.err" | cut -c1-200)"
        HFI_PROPOSED=0
    else
        cardano-cli conway transaction sign --testnet-magic "$LD_MAGIC" --tx-body-file "$ZOO_TMP/hfi.raw" \
            --signing-key-file "$WA/payment.skey" --out-file "$ZOO_TMP/hfi.signed" >/dev/null 2>&1
        HFI_TXID=$(cardano-cli conway transaction txid --tx-file "$ZOO_TMP/hfi.signed" 2>/dev/null \
                   | jq -r 'if type=="object" then .txhash else . end' 2>/dev/null | tr -d '"[:space:]')
        if SUB=$(cardano-cli conway transaction submit --testnet-magic "$LD_MAGIC" \
                    --socket-path "$LD_RELAY_SOCK" --tx-file "$ZOO_TMP/hfi.signed" 2>&1); then
            if [ "${#HFI_TXID}" -eq 64 ]; then
                ok "2-propose-hfi" "HardForkInitiation PV10->11 proposed: $HFI_TXID#0"
                HFI_PROPOSED=1
            else
                bad "2-propose-hfi" "txid did not parse as 64 hex chars: '$HFI_TXID'"
                HFI_PROPOSED=0
            fi
        else
            bad "2-propose-hfi" "proposal rejected: $(echo "$SUB" | grep -m1 -E 'Error|Failure' | cut -c1-200)"
            HFI_PROPOSED=0
        fi
    fi
fi
sleep 10

if [ "${HFI_PROPOSED:-0}" -eq 1 ]; then
    if vote_all "${HFI_TXID}#0" hfi "drep,spo,cc"; then
        ok "2-vote-hfi" "votes submitted for ${HFI_TXID:0:16}...#0 (drep,spo,cc)"
    else
        bad "2-vote-hfi" "vote submission failed — cannot distinguish 'not ratified' from 'never voted'"
    fi
else
    bad "2-vote-hfi" "skipped — HFI was never proposed"
fi
sleep 10

# ─────────────────────────────────────────────────────────────────────────
step "3. start futurePParams + ratify-state samplers (background)"
# ─────────────────────────────────────────────────────────────────────────
# Standard invocations of the devnet-validate skill scripts. Left to run
# their own --seconds window (same magnitude as the boundary wait below)
# so THEY compute their own PASS/FAIL/INCONCLUSIVE verdict rather than this
# round reimplementing that logic against a truncated CSV.
SAMPLER_SCRIPTS_DIR="$LD_REPO_ROOT/.claude/skills/devnet-validate/scripts"
FPP_SCRIPT="$SAMPLER_SCRIPTS_DIR/futurepparams-boundary-parity.sh"
RSP_SCRIPT="$SAMPLER_SCRIPTS_DIR/ratify-state-parity.sh"
FPP_CSV="$EVID/futurepparams-boundary-parity.csv"
RSP_CSV="$EVID/ratify-state-parity.csv"
FPP_LOG="$EVID/futurepparams-boundary-parity.log"
RSP_LOG="$EVID/ratify-state-parity.log"
SAMPLER_SECONDS=1800

if [ -x "$FPP_SCRIPT" ] || [ -f "$FPP_SCRIPT" ]; then
    bash "$FPP_SCRIPT" --seconds "$SAMPLER_SECONDS" --out "$FPP_CSV" >"$FPP_LOG" 2>&1 &
    FPP_PID=$!
    note "3-samplers-started" "futurepparams-boundary-parity.sh pid=$FPP_PID seconds=$SAMPLER_SECONDS out=$FPP_CSV"
else
    bad "3-samplers-started" "futurepparams-boundary-parity.sh not found at $FPP_SCRIPT"
    FPP_PID=""
fi
if [ -x "$RSP_SCRIPT" ] || [ -f "$RSP_SCRIPT" ]; then
    bash "$RSP_SCRIPT" --seconds "$SAMPLER_SECONDS" --out "$RSP_CSV" >"$RSP_LOG" 2>&1 &
    RSP_PID=$!
    note "3-samplers-started" "ratify-state-parity.sh pid=$RSP_PID seconds=$SAMPLER_SECONDS out=$RSP_CSV"
else
    bad "3-samplers-started" "ratify-state-parity.sh not found at $RSP_SCRIPT"
    RSP_PID=""
fi

# ─────────────────────────────────────────────────────────────────────────
step "4. wait for the pulser freeze + enactment boundary; track the PV flip"
# ─────────────────────────────────────────────────────────────────────────
# Concurrent per-socket PV monitor, backgrounded alongside the blocking
# wait_boundaries call below: relay and cardano-bp only apply the boundary
# block (and therefore only flip PV) once THEY have received and applied
# it, so the two sockets can genuinely tick over a few seconds apart. A
# single post-hoc check after wait_boundaries returns cannot recover WHICH
# epoch each socket flipped in if this round is rerun with looser timing,
# so this samples both sockets every ~5s across the whole wait.
FLIP_D="$EVID/pv-flip-dugite.epoch"
FLIP_H="$EVID/pv-flip-haskell.epoch"
: > "$FLIP_D"; : > "$FLIP_H"
(
    deadline=$(( $(date +%s) + 1800 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if [ ! -s "$FLIP_D" ]; then
            p=$(pv_major "$LD_RELAY_SOCK"); e=$(cur_epoch "$LD_RELAY_SOCK")
            [ "$p" = "11" ] && echo "$e" > "$FLIP_D"
        fi
        if [ ! -s "$FLIP_H" ]; then
            p=$(pv_major "$LD_CARDANO_BP_SOCK"); e=$(cur_epoch "$LD_CARDANO_BP_SOCK")
            [ "$p" = "11" ] && echo "$e" > "$FLIP_H"
        fi
        [ -s "$FLIP_D" ] && [ -s "$FLIP_H" ] && break
        sleep 5
    done
) &
MON_PID=$!

wait_boundaries 2

# Grace period for the monitor to catch a flip landing right at the edge
# of wait_boundaries's own return, then stop it (SIGTERM, never -9 — house
# convention, even for a plain bash helper).
for i in 1 2 3 4 5 6; do
    [ -s "$FLIP_D" ] && [ -s "$FLIP_H" ] && break
    sleep 5
done
kill "$MON_PID" 2>/dev/null
wait "$MON_PID" 2>/dev/null

PVD2=$(pv_major "$LD_RELAY_SOCK")
PVH2=$(pv_major "$LD_CARDANO_BP_SOCK")
echo "  protocolVersion.major after boundary wait: dugite(relay)=$PVD2 haskell(cardano-bp)=$PVH2"
if [ "$PVD2" = "11" ] && [ "$PVH2" = "11" ]; then
    ok "4-pv-post" "PV major=11 on both sockets — HardForkInitiation enacted"
else
    bad "4-pv-post" "expected PV11 on both sockets after enactment; got dugite=$PVD2 haskell=$PVH2 — this is a NO-GO signal, not a harness flake"
fi

D_FLIP=$(cat "$FLIP_D" 2>/dev/null)
H_FLIP=$(cat "$FLIP_H" 2>/dev/null)
if [ -n "$D_FLIP" ] && [ -n "$H_FLIP" ] && [ "$D_FLIP" = "$H_FLIP" ]; then
    ok "4-flip-epoch-match" "both sockets flipped to PV11 in epoch $D_FLIP"
elif [ -n "$D_FLIP" ] && [ -n "$H_FLIP" ]; then
    bad "4-flip-epoch-match" "sockets flipped in DIFFERENT epochs: dugite=$D_FLIP haskell=$H_FLIP"
else
    bad "4-flip-epoch-match" "flip epoch not recorded for at least one socket (dugite=$D_FLIP haskell=$H_FLIP) — see $FLIP_D / $FLIP_H"
fi

# Stop the samplers "cleanly": let them reach their own --seconds deadline
# (sized the same as the boundary wait above) and report their own
# verdict, rather than killing them mid-loop and losing the FAIL/
# INCONCLUSIVE classification their tail logic computes on exit.
if [ -n "$FPP_PID" ]; then
    wait "$FPP_PID"; FPP_RC=$?
    classify_sampler "4-futurepparams-sampler" "$FPP_LOG" "$FPP_RC"
fi
if [ -n "$RSP_PID" ]; then
    wait "$RSP_PID"; RSP_RC=$?
    classify_sampler "4-ratify-state-sampler" "$RSP_LOG" "$RSP_RC"
fi

# ─────────────────────────────────────────────────────────────────────────
step "5. re-run 16e post-HF — expect the PV11 constructor"
# ─────────────────────────────────────────────────────────────────────────
# RED-PROOF: passing want_pv=10 here (post-HF, live PV should now be 11)
# must FAIL — the live chain will answer with DepositIncorrectDELEG, which
# will not match the pinned IncorrectDepositDELEG expectation, and run_16e
# reports FAIL rather than silently accepting whichever constructor showed
# up. The literal `11` below is load-bearing; do not derive it from PVD2.
run_16e "5-16e-post-hf" 11 "$EVID/16e-post-hf.csv"

# ─────────────────────────────────────────────────────────────────────────
step "6. post-HF zoo smoke (01-bookkeeping, 08-negative, 16-cert-negatives)"
# ─────────────────────────────────────────────────────────────────────────
# 18-plutus-edges is deliberately excluded: 18f's BabbageNonDisjointRefInputs
# constructor arm INVERTS at PV11 (pre-flight finding 3) and would need its
# own expectation flip before it belongs in a post-HF smoke.
#
# ONE run, against the relay (dugite) socket. An earlier revision ran the
# same scripts a SECOND time against the cardano-bp socket for "both-socket
# parity", but both runs draw from the same funder wallet — the first run's
# positives (01i/01j fan-out, 08t's accept arm) consume it, so the second run
# failed with fanout-not-included / submit / drained-wallet cascades (5 false
# FAILs), NOT a PV11 divergence. Cross-node parity is already covered without
# a second submit: every positive script calls zoo_wait_all_observers, which
# fails unless the tx reaches BOTH dugite-bp AND cardano-bp with the same
# verdict, and the bidirectional-parity round (1p) runs these exact
# categories through both sockets with disjoint per-batch wallets. So the
# post-HF smoke asserts dugite accepts/rejects correctly at PV11; it does not
# re-submit the identical bytes to a second socket on a shared wallet.
run_zoo_smoke "6-zoo-smoke" "$LD_RELAY_SOCK" "$EVID/zoo-smoke.csv"

# ─────────────────────────────────────────────────────────────────────────
step "SUMMARY"
# ─────────────────────────────────────────────────────────────────────────
if [ "$FAILURES" -eq 0 ]; then
    ok "SUMMARY" "hardfork round: all assertions passed"
else
    bad "SUMMARY" "hardfork round: $FAILURES assertion(s) failed"
fi
echo "final epoch: $(cur_epoch "$LD_RELAY_SOCK") — evidence: $EVID"

# ─────────────────────────────────────────────────────────────────────────
step "7. terminal teardown"
# ─────────────────────────────────────────────────────────────────────────
[ "$SKIP_SETUP" -eq 0 ] && ./stop.sh >/dev/null 2>&1
exit "$FAILURES"
