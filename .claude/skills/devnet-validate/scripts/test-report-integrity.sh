#!/usr/bin/env bash
# test-report-integrity.sh — prove the release-report generator FAILS on the
# evidence shapes that used to be reported as clean zeros (#953).
#
# WHY THIS EXISTS
# ---------------
# The backlog this closes (#945, #923, #953) is one repeated failure: a check
# that reports success while measuring nothing. The fix for that class is never
# "write a stricter check" on its own — it is "demonstrate the stricter check
# goes RED on the exact input that used to pass". This script is that
# demonstration, mechanized so it stays true.
#
# Each case below builds a synthetic evidence tree, runs the real generator
# against it, and asserts the exit code AND the message. Case 0 is the control:
# a complete tree must still pass, otherwise the gate is merely broken rather
# than strict.
#
# Usage: test-report-integrity.sh [--keep]
# Exit: 0 = every case behaved as specified; 1 = at least one did not.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$SCRIPT_DIR/generate-release-report.sh"
DENOM="$SCRIPT_DIR/../schemas/denominators.json"

KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1

TMP=$(mktemp -d "${TMPDIR:-/tmp}/report-integrity.XXXXXX")
cleanup() { [ "$KEEP" -eq 1 ] || rm -rf "$TMP"; }
trap cleanup EXIT

PASSED=0; FAILED=0

# Pinned counts, so fixtures track the manifest instead of drifting from it.
ZOO_N=$(jq -r '.tx_zoo.expected_scripts // 85' "$DENOM" 2>/dev/null || echo 85)
# `expected_cases` was split into per-preset keys by #959; reading the retired
# name silently fell back to the literal default, which is the same
# fixture-drifts-from-manifest failure this block exists to prevent.
CHAOS_N=$(jq -r '.chaos.expected_cases_standard // .chaos.expected_cases // 5' "$DENOM" 2>/dev/null || echo 5)
RPC_N=$(jq -r '.rpc.expected_checks // 27' "$DENOM" 2>/dev/null || echo 27)
# ROWS, not scripts: two cli-parity scripts emit a second assertion row each
# (#963's parity_assert_pool_filter), and the gate counts rows.
CLI_N=$(jq -r '.cli_parity.expected_rows // .cli_parity.expected_queries // 24' "$DENOM" 2>/dev/null || echo 24)

# ---- Synthetic evidence builders --------------------------------------------
# Build one round dir that satisfies the standard preset completely.
make_round() { # make_round <dir> <n2n_rows> <cli_rows> <parity_rows>
    # Defaults come from the manifest, never from a literal — a hardcoded 22
    # here is exactly the drift the pinned counts above exist to prevent, and
    # it is what made this suite red when the cli-parity pin moved to rows.
    local d="$1" n2n="${2:-26}" cli="${3:-$CLI_N}" par="${4:-41}"
    mkdir -p "$d/logs"

    cat > "$d/metadata.json" <<'EOF'
{"dugite_node_git":"0000000000000000000000000000000000000000",
 "cardano_node_version":"cardano-node 11.0.1","cardano_cli_version":"cardano-cli 11.0.0.0",
 "duration_seconds":120}
EOF

    { echo "ts,node,event,slot,hash,pool"
      for i in $(seq 1 10); do
          echo "2026-08-02T00:00:0${i}Z,dugite-bp,forge,$((100+i)),hash$i,pool1"
          echo "2026-08-02T00:00:0${i}Z,cardano-bp,recv,$((100+i)),hash$i,pool1"
      done
    } > "$d/blocks.csv"

    { echo "ts,node,slot,block,hash"
      for i in $(seq 1 10); do echo "2026-08-02T00:00:0${i}Z,dugite-bp,$((100+i)),$i,hash$i"; done
    } > "$d/tip-samples.csv"

    { echo "ts,node,age_seconds"
      for i in $(seq 1 10); do echo "2026-08-02T00:00:0${i}Z,dugite-bp,1.5"; done
    } > "$d/tip-age-samples.csv"

    echo "ts,txid,socket,submit_rc" > "$d/tx-submissions.csv"
    echo "2026-08-02T00:00:01Z,deadbeef,relay,0" >> "$d/tx-submissions.csv"

    # verify.sh's in-round report — all predicates PASS
    cat > "$d/report.md" <<'EOF'
# verify report

| id | predicate | result | detail |
|---|---|---|---|
| p1 | forge-cross-check | PASS | 10 canonical blocks |
| p2 | per-bp-attribution | PASS | validator-only |
| p3 | tx-inclusion | PASS | 1/1 |
| p4 | tip-parity | PASS | 10/10 ticks |
| p5 | tip-age | PASS | p99 1.5s |
EOF

    # tx-zoo per-round snapshot AT the pinned denominator, read from the
    # manifest rather than hardcoded — a fixture that drifts from the pin makes
    # the control case fail for a reason unrelated to what is being tested.
    { echo "ts,script,status,txid,detail"
      for i in $(seq 1 "$ZOO_N"); do echo "2026-08-02T00:00:01Z,script$i,PASS,txid$i,ok"; done
    } > "$d/tx-results.csv"

    { echo "ts,protocol,msg_type,peer,dir,size_bytes,outcome,notes"
      for i in $(seq 1 "$n2n"); do echo "2026-08-02T00:00:01Z,handshake,msg$i,peer,out,10,REJECTED,ok"; done
    } > "$d/n2n-trace.csv"

    { echo "ts,query,status,dugite_sha256,cardano_sha256,equal,notes"
      for i in $(seq 1 "$cli"); do echo "2026-08-02T00:00:01Z,query$i,EQUAL,aa,aa,true,"; done
    } > "$d/cli-parity.csv"

    { echo "name,category,status_relay,detail_relay,class_relay,status_cardano_bp,detail_cardano_bp,class_cardano_bp,match"
      for i in $(seq 1 "$par"); do echo "01a-script$i,01,PASS,ok,ok,PASS,ok,ok,MATCH"; done
    } > "$d/parity-matrix.csv"
    cat > "$d/parity-matrix.meta.json" <<EOF
{"expected": $par, "total": $par, "match": $par, "offdiag": 0, "classdiff": 0, "categories": ["01-bookkeeping"]}
EOF

    # `detail` TRAILS `result` — this is the real layout, and reproducing it is
    # the whole point: the fixture used to stop at `result`, so the last column
    # WAS the verdict and `$NF` looked correct here while being wrong in
    # production. A fixture that cannot express the bug cannot catch it (#987).
    { echo "ts,scenario,action,recovery_seconds,result,detail"
      for i in $(seq 1 "$CHAOS_N"); do echo "2026-08-02T00:00:01Z,scenario$i,act,5,PASS,tip_before=1 tip_after=1"; done
    } > "$d/chaos-events.csv"

    # rpc.csv (#960). Read from the manifest like ZOO_N/CHAOS_N so the fixture
    # cannot drift away from the pinned denominator.
    { echo "ts,check,api_version,endpoint,status,detail"
      for i in $(seq 1 "$RPC_N"); do
        echo "2026-08-02T00:00:01Z,check$i,v1alpha,svc/Method,PASS,ok"
      done
    } > "$d/rpc.csv"

    # The two continuous boundary-parity samplers (#977, #988). Both are
    # `any`-scoped: required in at least one round, not every round. Their
    # verdict column is `verdict` and their vocabularies differ, so both are in
    # VERDICT_CSVS and both are exercised by the vocabulary cases below.
    { echo "ts,slot,epoch,dugite_tag,cardano_tag,equal,verdict"
      for i in $(seq 1 40); do
        echo "2026-08-02T00:00:0${i}Z,$((800+i)),2,NoPParamsUpdate,NoPParamsUpdate,true,MATCH"
      done
    } > "$d/futurepparams-parity.csv"

    { echo "ts,slot,epoch,socket_agree,dugite,cardano,verdict"
      for i in $(seq 1 40); do
        echo "2026-08-02T00:00:0${i}Z,$((800+i)),2,true,{},{},MATCH"
      done
      echo "2026-08-02T00:00:41Z,1200,3,true,{},,PLAN_APPLIED"
    } > "$d/ratify-state-parity.csv"
}

# ---- Case runner -------------------------------------------------------------
run_case() { # run_case <name> <expected_exit> <expect_substr> <evidence dirs...>
    local name="$1" want_rc="$2" want_msg="$3"; shift 3
    local out rc
    out=$("$GEN" --preset standard --output-dir "$TMP/out-$RANDOM" \
                 --denominators "$DENOM" "$@" 2>&1)
    rc=$?
    local ok=1
    [ "$rc" -eq "$want_rc" ] || ok=0
    if [ -n "$want_msg" ]; then
        echo "$out" | grep -qi -- "$want_msg" || ok=0
    fi
    if [ "$ok" -eq 1 ]; then
        printf '  \033[32mPASS\033[0m  %-52s (exit %d)\n' "$name" "$rc"
        PASSED=$(( PASSED + 1 ))
    else
        printf '  \033[31mFAIL\033[0m  %-52s (exit %d, wanted %d)\n' "$name" "$rc" "$want_rc"
        [ -n "$want_msg" ] && printf '        wanted message match: %s\n' "$want_msg"
        printf '%s\n' "$out" | sed 's/^/        | /' | tail -20
        FAILED=$(( FAILED + 1 ))
    fi
}

echo "=== report gate-integrity tests ==="
echo

# --- Case 0: CONTROL. A complete evidence tree must still PASS. ---
# Without this, "everything fails" would look like success.
R1="$TMP/complete/r1"; R2="$TMP/complete/r2"
make_round "$R1"; make_round "$R2"
run_case "control: complete evidence passes" 0 "" "$R1" "$R2"

# --- Case 1: absent cli-parity.csv (the #953 finding 1 shape) ---
# Before this change the generator emitted cli_parity {0,0,0,0} and exited 0.
R1="$TMP/no-cli/r1"; R2="$TMP/no-cli/r2"
make_round "$R1"; make_round "$R2"; rm -f "$R1/cli-parity.csv" "$R2/cli-parity.csv"
run_case "absent cli-parity.csv fails the gate" 3 "cli-parity.csv absent in EVERY round" "$R1" "$R2"

# --- Case 2: absent n2n-trace.csv ---
R1="$TMP/no-n2n/r1"; R2="$TMP/no-n2n/r2"
make_round "$R1"; make_round "$R2"; rm -f "$R1/n2n-trace.csv" "$R2/n2n-trace.csv"
run_case "absent n2n-trace.csv fails the gate" 3 "n2n-trace.csv absent in EVERY round" "$R1" "$R2"

# --- Case 3: absent parity-matrix.csv (finding 2 — no durable record) ---
R1="$TMP/no-parity/r1"; R2="$TMP/no-parity/r2"
make_round "$R1"; make_round "$R2"; rm -f "$R1/parity-matrix.csv" "$R2/parity-matrix.csv"
run_case "absent parity-matrix.csv fails the gate" 3 "parity-matrix.csv absent" "$R1" "$R2"

# --- Case 4: tx-results.csv missing for a round → shared fallback (finding 3) ---
# This is the shape that manufactured v2.4.5's "+12 tx-zoo pass" trend.
R1="$TMP/shared/r1"; R2="$TMP/shared/r2"
make_round "$R1"; make_round "$R2"; rm -f "$R2/tx-results.csv"
mkdir -p "$TMP/shared/state"
{ echo "ts,script,status,txid,detail"
  for i in $(seq 1 85); do echo "2026-08-02T00:00:01Z,script$i,PASS,txid$i,ok"; done
} > "$TMP/shared/state/results.csv"
out=$("$GEN" --preset standard --output-dir "$TMP/out-shared" --denominators "$DENOM" \
             --tx-zoo-state "$TMP/shared/state" "$R1" "$R2" 2>&1); rc=$?
if [ "$rc" -eq 3 ] && echo "$out" | grep -q 'source="shared"'; then
    printf '  \033[32mPASS\033[0m  %-52s (exit %d)\n' "shared tx-zoo source fails the gate" "$rc"
    PASSED=$(( PASSED + 1 ))
else
    printf '  \033[31mFAIL\033[0m  %-52s (exit %d, wanted 3)\n' "shared tx-zoo source fails the gate" "$rc"
    printf '%s\n' "$out" | sed 's/^/        | /' | tail -20
    FAILED=$(( FAILED + 1 ))
fi

# --- Case 5: n2n short of its pinned denominator (finding 4 — no denominator) ---
# 3 rows would previously have been reported as "3/3 adversarial, 0 panic".
R1="$TMP/short-n2n/r1"; R2="$TMP/short-n2n/r2"
make_round "$R1" 3; make_round "$R2" 3
run_case "n2n below pinned denominator fails" 3 "below the pinned" "$R1" "$R2"

# --- Case 5b: no round ran the full tx-zoo ---
# Rounds 2/3 legitimately hold partial slices, so the denominator is asserted
# across rounds rather than per round. If NO round ran it to completion the
# gate must still fail.
R1="$TMP/short-zoo/r1"; R2="$TMP/short-zoo/r2"
make_round "$R1"; make_round "$R2"
for d in "$R1" "$R2"; do
    { echo "ts,script,status,txid,detail"
      for i in $(seq 1 12); do echo "2026-08-02T00:00:01Z,script$i,PASS,txid$i,ok"; done
    } > "$d/tx-results.csv"
done
run_case "no round ran the full tx-zoo fails" 3 "no round ran the full tx-zoo" "$R1" "$R2"

# --- Case 5c: one full round + partial later rounds is ACCEPTED ---
# The realistic 3-round shape must not be a false failure.
R1="$TMP/partial-ok/r1"; R2="$TMP/partial-ok/r2"
make_round "$R1"; make_round "$R2"
{ echo "ts,script,status,txid,detail"
  for i in $(seq 1 12); do echo "2026-08-02T00:00:01Z,script$i,PASS,txid$i,ok"; done
} > "$R2/tx-results.csv"
run_case "full zoo in round 1 + trickle in round 2 passes" 0 "" "$R1" "$R2"

# --- Case 5d: cli-parity FULL of rows but mostly SKIPPED must FAIL ---
#
# Observed live: heavy tx-zoo/parity load pushed cardano-bp behind, the suite
# emitted all 22 rows but 18 of them were `SKIP  TIP_UNSTABLE after 20
# attempts`, and the gate reported "denominator: 22/22 queries OK".
#
# 22 rows, 4 comparisons. A denominator that counts rows EMITTED rather than
# comparisons MADE is the #953 disease inside the #953 fix.
R1="$TMP/skipped-cli/r1"; R2="$TMP/skipped-cli/r2"
make_round "$R1"; make_round "$R2"
{ echo "ts,query,status,dugite_sha256,cardano_sha256,equal,notes"
  echo "2026-08-02T00:00:01Z,tip/era,EQUAL,aa,aa,true,"
  for i in $(seq 2 "$CLI_N"); do
    echo "2026-08-02T00:00:01Z,query$i,SKIP,,,,TIP_UNSTABLE after 20 attempts"
  done
} > "$R1/cli-parity.csv"
cp "$R1/cli-parity.csv" "$R2/cli-parity.csv"
run_case "cli-parity full of rows but mostly SKIPPED fails" 3 "below the pinned" "$R1" "$R2"

# --- Case 6: cli-parity short of its denominator (finding 5) ---
R1="$TMP/short-cli/r1"; R2="$TMP/short-cli/r2"
make_round "$R1" 26 "$(( CLI_N - 4 ))"; make_round "$R2" 26 "$(( CLI_N - 4 ))"
run_case "cli-parity below pinned denominator fails" 3 "below the pinned" "$R1" "$R2"

# --- Case 7: --no-strict records the omission instead of hiding it ---
# Non-strict must still be HONEST: exit 0 is allowed, silence is not.
R1="$TMP/nostrict/r1"
make_round "$R1"; rm -f "$R1/cli-parity.csv" "$R1/n2n-trace.csv" "$R1/parity-matrix.csv" "$R1/chaos-events.csv" "$R1/rpc.csv" "$R1/futurepparams-parity.csv" "$R1/ratify-state-parity.csv"
OUTD="$TMP/out-nostrict"
"$GEN" --preset standard --no-strict --output-dir "$OUTD" --denominators "$DENOM" "$R1" >/dev/null 2>&1
rc=$?
adm=$(jq -r '.gate_integrity.admissible' "$OUTD/report.json" 2>/dev/null)
nmiss=$(jq -r '.gate_integrity.missing | length' "$OUTD/report.json" 2>/dev/null)
cli_status=$(jq -r '.rounds[0].cli_parity.status' "$OUTD/report.json" 2>/dev/null)
cli_equal=$(jq -r '.rounds[0].cli_parity.equal' "$OUTD/report.json" 2>/dev/null)
if [ "$rc" -eq 0 ] && [ "$adm" = "false" ] && [ "${nmiss:-0}" -ge 4 ] \
   && [ "$cli_status" = "absent" ] && [ "$cli_equal" = "null" ]; then
    printf '  \033[32mPASS\033[0m  %-52s (exit %d)\n' "--no-strict records omissions, counts are null" "$rc"
    PASSED=$(( PASSED + 1 ))
else
    printf '  \033[31mFAIL\033[0m  %-52s\n' "--no-strict records omissions, counts are null"
    printf '        rc=%s admissible=%s missing=%s cli.status=%s cli.equal=%s\n' \
           "$rc" "$adm" "$nmiss" "$cli_status" "$cli_equal"
    printf '        (want rc=0 admissible=false missing>=4 status=absent equal=null)\n'
    FAILED=$(( FAILED + 1 ))
fi

# --- Case 8: an absent suite must never serialize as 0 ---
# This is the whole thesis of the schema v2 bump: 0 and "did not run" must not
# be the same JSON.
zeros=$(jq -r '[.rounds[0].cli_parity.equal, .rounds[0].n2n_adversarial.pass,
                .rounds[0].parity_matrix.total, .rounds[0].chaos.pass]
               | map(select(. == 0)) | length' "$OUTD/report.json" 2>/dev/null)
if [ "${zeros:-1}" -eq 0 ]; then
    printf '  \033[32mPASS\033[0m  %-52s\n' "absent suites serialize as null, never 0"
    PASSED=$(( PASSED + 1 ))
else
    printf '  \033[31mFAIL\033[0m  %-52s (%s zero-valued)\n' "absent suites serialize as null, never 0" "$zeros"
    FAILED=$(( FAILED + 1 ))
fi

# --- Case 8a: every verdict column is resolved by NAME, not by position ---
#
# #987 closed one instance of "the verdict column moved and the count silently
# went to zero". This closes the CLASS: shuffle a trailing field into every
# verdict CSV and require the counts to be unchanged. Under positional reads
# each of these lands on the wrong field, matches nothing, and serializes a
# clean sweep — which is exactly how "chaos 5/5" survived five releases.
R1="$TMP/colshuffle/r1"
make_round "$R1"
# Insert a new column immediately BEFORE each verdict column, so a positional
# read is off by one in the direction that finds free text.
python3 - "$R1" <<'PYEOF'
import csv, sys, pathlib
d = pathlib.Path(sys.argv[1])
for name, verdict in [("chaos-events.csv", "result"),
                      ("n2n-trace.csv", "outcome"),
                      ("rpc.csv", "status"),
                      ("cli-parity.csv", "status"),
                      ("parity-matrix.csv", "match")]:
    f = d / name
    rows = list(csv.reader(f.open()))
    i = rows[0].index(verdict)
    rows[0].insert(i, "injected_column")
    for r in rows[1:]:
        r.insert(i, "PASS")          # decoy that a positional read would match
    with f.open("w", newline="") as fh:
        csv.writer(fh).writerows(rows)
PYEOF
"$GEN" --preset standard --no-strict --output-dir "$TMP/out-shuf" --denominators "$DENOM" "$R1" >/dev/null 2>&1
shuf_ok=1
read -r sh_chaos sh_n2n sh_rpc sh_cli sh_pm <<EOF2
$(jq -r '[.rounds[0].chaos.pass, .rounds[0].n2n_adversarial.pass,
          .rounds[0].rpc.pass, .rounds[0].cli_parity.equal,
          .rounds[0].parity_matrix.match] | @tsv' "$TMP/out-shuf/report.json" 2>/dev/null)
EOF2
[ "${sh_chaos:-0}" = "$CHAOS_N" ] || shuf_ok=0
[ "${sh_n2n:-0}"   = "26" ]       || shuf_ok=0
[ "${sh_rpc:-0}"   = "$RPC_N" ]   || shuf_ok=0
[ "${sh_cli:-0}"   = "$CLI_N" ]   || shuf_ok=0
[ "${sh_pm:-0}"    = "41" ]       || shuf_ok=0
if [ "$shuf_ok" -eq 1 ]; then
    printf '  \033[32mPASS\033[0m  %-52s\n' "verdict columns survive a column being inserted"
    PASSED=$(( PASSED + 1 ))
else
    printf '  \033[31mFAIL\033[0m  %-52s\n' "verdict columns survive a column being inserted"
    printf '        chaos=%s (want %s) n2n=%s (want 26) rpc=%s (want %s) cli=%s (want %s) parity=%s (want 41)\n' \
           "$sh_chaos" "$CHAOS_N" "$sh_n2n" "$sh_rpc" "$RPC_N" "$sh_cli" "$CLI_N" "$sh_pm"
    FAILED=$(( FAILED + 1 ))
fi

# --- Case 8a2: a RENAMED verdict column is reported, not silently zeroed ---
# The other direction. If the column cannot be found at all, the honest answer
# is "cannot classify" on stderr and a count of 0 that the denominator gate then
# rejects — never a clean sweep.
R1="$TMP/colrename/r1"
make_round "$R1"
sed -i.bak '1s/,result,/,verdict,/' "$R1/chaos-events.csv" && rm -f "$R1/chaos-events.csv.bak"
rename_err=$("$GEN" --preset standard --no-strict --output-dir "$TMP/out-ren" \
                    --denominators "$DENOM" "$R1" 2>&1 >/dev/null || true)
ren_pass=$(jq -r '.rounds[0].chaos.pass' "$TMP/out-ren/report.json" 2>/dev/null)
if printf '%s' "$rename_err" | grep -q "no 'result' column" && [ "${ren_pass:-x}" = "0" ]; then
    printf '  \033[32mPASS\033[0m  %-52s\n' "a renamed verdict column warns instead of passing"
    PASSED=$(( PASSED + 1 ))
else
    printf '  \033[31mFAIL\033[0m  %-52s\n' "a renamed verdict column warns instead of passing"
    printf '        chaos.pass=%s stderr=%s\n' "$ren_pass" "$(printf '%s' "$rename_err" | head -1)"
    FAILED=$(( FAILED + 1 ))
fi

# --- Case 8a3: rows present but NONE classified is a gate-integrity failure ---
# The residual hole after a renamed column: `total` is still the row count, so
# `status` reads "ok" and the round passes with pass=0. Now generalised from
# chaos to every verdict CSV, so any suite whose outcomes were never read is
# reported rather than serialized as a clean sweep.
#
# The file list and each file's verdict header are READ OUT OF THE GENERATOR's
# own `VERDICT_CSVS` table rather than restated here. A second copy is how this
# case came to cover five CSVs while the generator guarded seven — the same
# N-copies drift the guard itself exists to catch.
while IFS='|' read -r vf vhdr _vocab; do
    [ -n "$vf" ] || continue
    R1="$TMP/unclassified-${vf%%.*}/r1"; R2="$TMP/unclassified-${vf%%.*}/r2"
    make_round "$R1"; make_round "$R2"
    for d in "$R1" "$R2"; do
        # Overwrite every verdict cell with a value outside the vocabulary,
        # leaving the row count untouched.
        python3 - "$d/$vf" "$vhdr" <<'PYEOF'
import csv, sys, pathlib
f, hdr = pathlib.Path(sys.argv[1]), sys.argv[2]
rows = list(csv.reader(f.open()))
i = rows[0].index(hdr)
for r in rows[1:]:
    r[i] = "unreadable"
with f.open("w", newline="") as fh:
    csv.writer(fh).writerows(rows)
PYEOF
    done
    run_case "$vf with no classified rows fails the gate" 3 "none classified" "$R1" "$R2"
done <<EOF
$(sed -n "/^VERDICT_CSVS='/,/'\$/p" "$GEN" | sed "s/^VERDICT_CSVS='//; s/'\$//")
EOF

# --- Case 8b: CLASSDIFF rows are counted separately from OFFDIAG ---
# "both rejected" is weaker than it looks: same verdict for a different reason
# is still a compat defect, and it must not be silently absorbed into `match`.
R1="$TMP/classdiff/r1"
make_round "$R1"
{ echo "name,category,status_relay,detail_relay,class_relay,status_cardano_bp,detail_cardano_bp,class_cardano_bp,match"
  echo "08e-no-inputs,08,PASS,rejected-NoInputs,NoInputs,PASS,rejected-BadInputsUTxO,BadInputsUTxO,CLASSDIFF"
  echo "08f-double-spend,08,PASS,rejected-x,reason-matches-rule,PASS,rejected-y,other,KNOWNDIFF"
  echo "05g-cc-hot-key-authorization,05,PASS,ok,(accepted),FAIL,submit,submit,STATEFUL"
  for i in $(seq 4 41); do echo "01a-script$i,01,PASS,ok,ok,PASS,ok,ok,MATCH"; done
} > "$R1/parity-matrix.csv"
"$GEN" --preset standard --no-strict --output-dir "$TMP/out-cd" --denominators "$DENOM" "$R1" >/dev/null 2>&1
cd_count=$(jq -r '.rounds[0].parity_matrix.classdiff' "$TMP/out-cd/report.json" 2>/dev/null)
cd_match=$(jq -r '.rounds[0].parity_matrix.match' "$TMP/out-cd/report.json" 2>/dev/null)
cd_od=$(jq -r '.rounds[0].parity_matrix.offdiag' "$TMP/out-cd/report.json" 2>/dev/null)
cd_cat=$(jq -r '.rounds[0].parity_matrix.per_category["08"].classdiff' "$TMP/out-cd/report.json" 2>/dev/null)
cd_known=$(jq -r '.rounds[0].parity_matrix.knowndiff' "$TMP/out-cd/report.json" 2>/dev/null)
cd_state=$(jq -r '.rounds[0].parity_matrix.stateful' "$TMP/out-cd/report.json" 2>/dev/null)
if [ "$cd_count" = "1" ] && [ "$cd_match" = "38" ] && [ "$cd_od" = "0" ] && [ "$cd_cat" = "1" ] \
   && [ "$cd_known" = "1" ] && [ "$cd_state" = "1" ]; then
    printf '  \033[32mPASS\033[0m  %-52s\n' "CLASSDIFF/KNOWNDIFF/STATEFUL counted separately"
    PASSED=$(( PASSED + 1 ))
else
    printf '  \033[31mFAIL\033[0m  %-52s\n' "CLASSDIFF/KNOWNDIFF/STATEFUL counted separately"
    printf '        classdiff=%s match=%s offdiag=%s per_cat08=%s known=%s stateful=%s (want 1/38/0/1/1/1)\n' \
           "$cd_count" "$cd_match" "$cd_od" "$cd_cat" "$cd_known" "$cd_state"
    FAILED=$(( FAILED + 1 ))
fi

# --- Case 9: output validates against the declared schema ---
# v1 drifted from the emitted keys because nothing ever compared them.
if python3 -c 'import jsonschema' 2>/dev/null; then
    R1="$TMP/schema/r1"; make_round "$R1"
    "$GEN" --preset standard --no-strict --output-dir "$TMP/out-schema" \
           --denominators "$DENOM" "$R1" >/dev/null 2>&1
    if python3 - "$SCRIPT_DIR/../schemas/report.v2.json" "$TMP/out-schema/report.json" <<'PY' 2>/dev/null
import json,sys,jsonschema
jsonschema.validate(json.load(open(sys.argv[2])), json.load(open(sys.argv[1])))
PY
    then
        printf '  \033[32mPASS\033[0m  %-52s\n' "report.json validates against report.v2.json"
        PASSED=$(( PASSED + 1 ))
    else
        printf '  \033[31mFAIL\033[0m  %-52s\n' "report.json validates against report.v2.json"
        FAILED=$(( FAILED + 1 ))
    fi
else
    printf '  \033[33mSKIP\033[0m  %-52s (python3 jsonschema unavailable)\n' "schema validation"
fi

echo
echo "=== $PASSED passed, $FAILED failed ==="
[ "$FAILED" -eq 0 ] || exit 1
