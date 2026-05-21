#!/usr/bin/env bash
# Evaluate the 4 soak predicates against an evidence directory.
# Usage:
#   verify.sh <evidence_dir>            — full report on real evidence
#   verify.sh --self-test                — run predicates against test fixtures
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/common.sh"

PREDICATE_PASS=()
PREDICATE_FAIL=()

# Predicate 1: every (slot, hash) seen by any observer is seen by all 3 observers
# (Tolerance: most-recent 10 blocks may have partial observers, since they
# may not have propagated yet at end of soak.)
p1_forge_cross_check() {
    local blocks="$1"
    [ -s "$blocks" ] || { PREDICATE_FAIL+=("p1:no-data"); return; }

    # Get all unique (slot, hash) pairs and trim the most-recent 10
    local distinct_blocks total trimmed
    distinct_blocks=$(awk -F, 'NR>1 && $4!="?" && $5!="?" && $4!="" && $5!="" {print $4","$5}' "$blocks" | sort -u)
    total=$(printf '%s\n' "$distinct_blocks" | grep -c '^' || true)
    if [ "$total" -le 10 ]; then
        trimmed="$distinct_blocks"
    else
        # Drop the most-recent 10 (portable: avoid GNU `head -n -10`)
        local keep=$((total - 10))
        trimmed=$(printf '%s\n' "$distinct_blocks" | sort -t, -k1n -k2 | awk -v k="$keep" 'NR<=k')
    fi

    # Canonical-chain filter (Bug J follow-up, 2026-05-16): the original
    # predicate counted EVERY forge event as a block that must be observed
    # by all three nodes.  This penalised natural slot-battle orphans: when
    # two BPs forge in the same slot (or within the Conway 5-slot tiebreaker
    # window), Praos picks one and the other becomes an orphan.  Orphan
    # blocks are by definition NOT on the canonical chain, so neither the
    # other BP nor (often) the relay ever adopts them.
    #
    # A block is canonical at end-of-soak iff BOTH BPs have an event for
    # it: the forger logs a `forge` event, and the OTHER BP logs a `recv`
    # event when its chain adopts that block.  Orphans only ever have the
    # forger's event.  This filter excludes orphans before counting
    # observers — matching the predicate's intent ("every canonical block
    # must be observed by all three nodes").
    local canonical_only=""
    while IFS=, read -r slot hash; do
        [ -z "$slot" ] && continue
        local dbp_seen cbp_seen
        dbp_seen=$(awk -F, -v s="$slot" -v h="$hash" \
            '$2=="dugite-bp"  && $4==s && $5==h {print 1; exit}' "$blocks")
        cbp_seen=$(awk -F, -v s="$slot" -v h="$hash" \
            '$2=="cardano-bp" && $4==s && $5==h {print 1; exit}' "$blocks")
        if [ "$dbp_seen" = "1" ] && [ "$cbp_seen" = "1" ]; then
            canonical_only="${canonical_only}${slot},${hash}"$'\n'
        fi
    done <<< "$trimmed"

    local total_canonical
    total_canonical=$(printf '%s' "$canonical_only" | grep -c '^' || true)

    local fails=0
    local fail_examples=""
    while IFS=, read -r slot hash; do
        [ -z "$slot" ] && continue
        local n_obs
        n_obs=$(awk -F, -v s="$slot" -v h="$hash" 'NR>1 && $4==s && $5==h {print $2}' "$blocks" | sort -u | wc -l | tr -d ' ')
        if [ "$n_obs" -lt 3 ]; then
            fails=$((fails + 1))
            [ -z "$fail_examples" ] && fail_examples="slot=$slot hash=$hash n_obs=$n_obs"
        fi
    done <<< "$canonical_only"

    local orphans=$((total - total_canonical))
    if [ "$fails" -eq 0 ]; then
        PREDICATE_PASS+=("p1:forge-cross-check ($total_canonical canonical blocks, >=3 observers each; $orphans orphan(s) excluded)")
    else
        PREDICATE_FAIL+=("p1:forge-cross-check ($fails/$total_canonical canonical blocks missing observers; example: $fail_examples)")
    fi
}
# Predicate 2: both pools must have forged >=3 blocks.
#
# Attribution model (2026-05-16): pool = observer. dugite's forge log line
#   "INFO forge: TraceForgedBlock slot=23 block_no=1 block_hash=46db..."
# does NOT emit an issuer= field, and cardano-node's Forge.Loop.AdoptedBlock
# JSON does not reliably populate .data.issuerVerKey either. Since the local
# devnet is a hub-and-spoke with exactly one BP per node (dugite-bp == pool1,
# cardano-bp == pool2), the log SOURCE is itself the pool identity. We
# attribute forges by `observer` directly.
#
# Fallback: if observer-based attribution finds 0 forges from one side, we
# also try the legacy issuer_vkey match (kept for forward compatibility with
# any future dugite log changes that DO emit an issuer field).
p2_per_bp_attribution() {
    local blocks="$1"
    [ -s "$blocks" ] || { PREDICATE_FAIL+=("p2:no-data"); return; }

    # Primary path: observer-based attribution. One forge row per
    # (observer, slot, hash) — dedupe within an observer to avoid counting
    # cardano-node's ForgedBlock+AdoptedBlock as two forges.
    local p1_forges p2_forges
    p1_forges=$(awk -F, '$2=="dugite-bp"  && $3=="forge" {print $4","$5}' "$blocks" \
                | sort -u | wc -l | tr -d ' ')
    p2_forges=$(awk -F, '$2=="cardano-bp" && $3=="forge" {print $4","$5}' "$blocks" \
                | sort -u | wc -l | tr -d ' ')

    # Fallback: if either side has 0 forges, retry with issuer_vkey match
    # in case the user wired up real pool key matching upstream.
    if [ "$p1_forges" -eq 0 ] || [ "$p2_forges" -eq 0 ]; then
        local pool1_vkey="" pool2_vkey=""
        if [ -f "$LD_KEYS/pool1/cold.vkey" ]; then
            pool1_vkey=$(jq -r '.cborHex' "$LD_KEYS/pool1/cold.vkey" 2>/dev/null \
                | tail -c +5 | head -c 64 || echo "")
        fi
        if [ -f "$LD_KEYS/pool2/cold.vkey" ]; then
            pool2_vkey=$(jq -r '.cborHex' "$LD_KEYS/pool2/cold.vkey" 2>/dev/null \
                | tail -c +5 | head -c 64 || echo "")
        fi
        if [ -n "$pool1_vkey" ] && [ "$p1_forges" -eq 0 ]; then
            p1_forges=$(awk -F, -v k="$pool1_vkey" '$3=="forge" && $6==k' "$blocks" \
                | wc -l | tr -d ' ')
        fi
        if [ -n "$pool2_vkey" ] && [ "$p2_forges" -eq 0 ]; then
            p2_forges=$(awk -F, -v k="$pool2_vkey" '$3=="forge" && $6==k' "$blocks" \
                | wc -l | tr -d ' ')
        fi
    fi

    # Detect validator-only mode: since b6084633c, cardano-bp runs as a
    # passive validator (no --shelley-{kes,vrf,operational-certificate}
    # flags) and all stake is delegated to pool1. In that topology pool2
    # cannot win a leader lottery and must have zero forges by design —
    # demanding pool2 >= 3 would always fail. Detect by looking for the
    # forging flags on cardano-bp's running command or its run.sh source.
    local cardano_bp_pid="" cardano_bp_cmd=""
    if [ -f "$LD_STATE/cardano-bp.pid" ]; then
        cardano_bp_pid="$(cat "$LD_STATE/cardano-bp.pid" 2>/dev/null || true)"
    fi
    if [ -n "$cardano_bp_pid" ] && kill -0 "$cardano_bp_pid" 2>/dev/null; then
        cardano_bp_cmd="$(ps -p "$cardano_bp_pid" -o command= 2>/dev/null || true)"
    fi
    local validator_only=0
    if [ -n "$cardano_bp_cmd" ]; then
        if ! echo "$cardano_bp_cmd" | grep -q -- '--shelley-kes-key'; then
            validator_only=1
        fi
    else
        # Process already gone (post-soak verify) — fall back to the run
        # script source which is the canonical recipe for this devnet.
        if ! grep -q -- '--shelley-kes-key' "$SCRIPT_DIR/run.sh" 2>/dev/null; then
            validator_only=1
        fi
    fi

    if [ "$validator_only" -eq 1 ]; then
        if [ "$p1_forges" -ge 3 ] && [ "$p2_forges" -eq 0 ]; then
            PREDICATE_PASS+=("p2:per-bp-attribution (validator-only: pool1=$p1_forges, pool2=0 as designed)")
            {
                printf 'mode\tvalidator-only\n'
                printf 'pool1_forges\t%s\n' "$p1_forges"
                printf 'pool2_forges\t%s\n' "$p2_forges"
            } > "$(dirname "$blocks")/forge-attribution.tsv"
        else
            PREDICATE_FAIL+=("p2:per-bp-attribution (validator-only mode: pool1=$p1_forges need >=3, pool2=$p2_forges must be 0)")
        fi
    elif [ "$p1_forges" -ge 3 ] && [ "$p2_forges" -ge 3 ]; then
        PREDICATE_PASS+=("p2:per-bp-attribution (pool1=$p1_forges pool2=$p2_forges via observer)")
        {
            printf 'pool1_forges\t%s\n' "$p1_forges"
            printf 'pool2_forges\t%s\n' "$p2_forges"
        } > "$(dirname "$blocks")/forge-attribution.tsv"
    else
        PREDICATE_FAIL+=("p2:per-bp-attribution (pool1=$p1_forges pool2=$p2_forges; need >=3 each)")
    fi
}
# Predicate 3: every submitted tx with submit_rc=0 must appear in all 3 nodes' UTxO
# at the change_addr. (Self-test: just verifies all submit_rc are 0.)
p3_tx_inclusion() {
    local txs="$1"
    local evd="$2"
    [ -s "$txs" ] || { PREDICATE_FAIL+=("p3:no-data"); return; }

    local non_zero
    non_zero=$(awk -F, 'NR>1 && $5!=0 && $5!="" {print}' "$txs" | wc -l | tr -d ' ')
    local total
    total=$(awk -F, 'NR>1 {print}' "$txs" | wc -l | tr -d ' ')

    if [ "$non_zero" -gt 0 ]; then
        PREDICATE_FAIL+=("p3:tx-inclusion ($non_zero/$total had submit_rc!=0)")
        return
    fi

    # If running on real evidence (not a fixture) AND devnet is up, verify
    # each tx is CONSISTENTLY visible across all three nodes' UTxO sets at
    # the genesis payment addr.
    #
    # Consistency model (Bug J follow-up, 2026-05-16): the previous predicate
    # required EVERY submitted tx to appear in all three UTxO sets.  In
    # practice, `submit-txs.sh` queries the node's on-chain UTxO between
    # consecutive submissions; because mempool state is NOT visible to
    # `query utxo`, a tx submitted before its predecessor reaches a block
    # picks the same input → double-spend.  Only one of the conflicting
    # txs can be forged; the loser is rejected by the mempool conflict
    # check and never appears in UTxO.
    #
    # This is inherent to the test harness, not a node bug.  The relevant
    # node-side correctness property is that all three nodes AGREE on
    # which txs were successfully included.  A real propagation bug would
    # show up as `r=1 d=0 c=1` (some nodes have the tx, others don't);
    # a natural double-spend loss shows up as `r=0 d=0 c=0` on every
    # node.  The refined predicate fails only on inconsistency.
    #
    # Self-test fixtures (no devnet socket) skip this check and pass on
    # `submit_rc=0` alone, matching the previous behaviour.
    local mismatches=0
    local examples=""
    local accepted=0
    local rejected=0
    if [ -S "$LD_RELAY_SOCK" ] && [ -f "$LD_KEYS/utxo/payment.addr" ]; then
        local addr
        addr=$(cat "$LD_KEYS/utxo/payment.addr")
        local utxo_relay utxo_dbp utxo_cbp
        utxo_relay=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
                      --socket-path "$LD_RELAY_SOCK" --address "$addr" \
                      --output-json 2>/dev/null || echo "{}")
        utxo_dbp=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
                      --socket-path "$LD_DUGITE_BP_SOCK" --address "$addr" \
                      --output-json 2>/dev/null || echo "{}")
        utxo_cbp=$(cardano-cli conway query utxo --testnet-magic "$LD_MAGIC" \
                      --socket-path "$LD_CARDANO_BP_SOCK" --address "$addr" \
                      --output-json 2>/dev/null || echo "{}")
        while IFS=, read -r ts target wave txid rc; do
            [ "$ts" = "ts" ] && continue
            [ -z "$txid" ] && continue
            local in_r in_d in_c
            in_r=$(echo "$utxo_relay" | jq --arg t "$txid" 'keys | map(select(startswith($t))) | length')
            in_d=$(echo "$utxo_dbp"   | jq --arg t "$txid" 'keys | map(select(startswith($t))) | length')
            in_c=$(echo "$utxo_cbp"   | jq --arg t "$txid" 'keys | map(select(startswith($t))) | length')
            if [ "$in_r" != "$in_d" ] || [ "$in_d" != "$in_c" ]; then
                mismatches=$((mismatches + 1))
                [ -z "$examples" ] && examples="$txid (r=$in_r d=$in_d c=$in_c)"
            elif [ "$in_r" -ge 1 ]; then
                accepted=$((accepted + 1))
            else
                rejected=$((rejected + 1))
            fi
        done < "$txs"
    fi

    if [ "$mismatches" -gt 0 ]; then
        PREDICATE_FAIL+=("p3:tx-inclusion ($mismatches/$total txs inconsistent across nodes; example: $examples)")
    elif [ -S "$LD_RELAY_SOCK" ] && [ "$accepted" -eq 0 ]; then
        # If the devnet was queried but not a single tx made it into a
        # block, the system isn't actually executing tx submissions —
        # likely a mempool / forge regression. Fail conservatively.
        PREDICATE_FAIL+=("p3:tx-inclusion (devnet live but 0/$total txs visible in any UTxO — mempool/forge regression?)")
    else
        PREDICATE_PASS+=("p3:tx-inclusion ($total txs submitted; $accepted accepted/$rejected rejected — all 3 nodes agree)")
    fi
}
# Predicate 4: at each 5s tick (grouped by ts), the node tips must be within
# 2 blocks of each other. We exclude dugite-bp from the calculation because of
# a known pre-existing N2C tip-query bug that leaves dugite-bp reporting a
# stale (slot=5, block_no=0) tip. Pass: >=95% of ticks in-parity across the
# remaining two observers (relay + cardano-bp).
p4_tip_parity() {
    local tips="$1"
    [ -s "$tips" ] || { PREDICATE_FAIL+=("p4:no-data"); return; }

    # Compute per-tick parity using awk across ALL three observers.
    #
    # NOTE: Prior to the 2026-05-16 tip-query staleness fix
    # (docs/superpowers/specs/2026-05-16-tip-query-staleness-fix.md), this
    # predicate excluded dugite-bp because its `cardano-cli query tip`
    # snapshot was frozen at the last peer-adopted block — never advancing
    # on own-forge.  That bug is fixed; the exclusion is removed.  All three
    # observers must agree within 2 blocks per tick.
    local result
    result=$(awk -F, '
        NR == 1 { next }   # skip header
        $4 == "?" || $4 == "" { next }   # skip rows lacking block_no
        {
            block[$1, $2] = $4 + 0
            seen[$1] = 1
        }
        END {
            for (t in seen) {
                if ((t SUBSEP "relay") in block \
                 && (t SUBSEP "cardano-bp") in block \
                 && (t SUBSEP "dugite-bp") in block) {
                    r = block[t, "relay"]
                    c = block[t, "cardano-bp"]
                    d = block[t, "dugite-bp"]
                    mn = r
                    if (c < mn) mn = c
                    if (d < mn) mn = d
                    mx = r
                    if (c > mx) mx = c
                    if (d > mx) mx = d
                    total++
                    if (mx - mn <= 2) in_parity++
                }
            }
            if (total == 0) { print "0 0"; exit }
            printf "%d %d\n", in_parity, total
        }
    ' "$tips")

    local in_parity total
    in_parity=$(echo "$result" | awk '{print $1}')
    total=$(echo "$result" | awk '{print $2}')

    if [ "$total" -eq 0 ]; then
        PREDICATE_FAIL+=("p4:tip-parity (no full ticks to evaluate)")
        return
    fi

    local pct=$(( in_parity * 100 / total ))
    local note="($in_parity/$total ticks in-parity = ${pct}% across all 3 observers)"
    if [ "$pct" -ge 95 ]; then
        PREDICATE_PASS+=("p4:tip-parity $note")
    else
        PREDICATE_FAIL+=("p4:tip-parity $note; need >=95%")
    fi
}

# Predicate 5: dugite_tip_age_seconds must be below threshold post-catch-up.
#
# Issue #508 (gap caught by cb509ef91 era-aware tip_age fix): the v1.5.0 BP
# soak reported PASS on `Chain extended` counts but missed an era-aware
# tip-age computation bug that made dugite-monitor render "Tip 19d 0h 0m"
# on preprod even though the chain was healthy. This predicate fails the
# soak immediately if the metric exceeds threshold once the node has had
# time to catch up.
#
# Default threshold: 300s ≈ 5 min. Override via LD_TIP_AGE_THRESHOLD_SEC.
# Loose enough to absorb normal Praos gaps (devnet f=0.05); tight enough
# to catch the 19-day class of bug.
#
# Catch-up grace: we ignore samples in the first GRACE_SEC of the soak so
# the metric has time to settle after node start. Default 60s.
p5_tip_age() {
    local samples="$1"
    local threshold="${LD_TIP_AGE_THRESHOLD_SEC:-300}"
    local grace="${LD_TIP_AGE_GRACE_SEC:-60}"

    if [ ! -s "$samples" ]; then
        PREDICATE_FAIL+=("p5:no-data")
        return
    fi

    # Drop header + first $grace seconds worth of samples. Samples are
    # written every 5s across 2 dugite nodes, so $grace seconds == roughly
    # (grace / 5) * 2 rows.
    local skip_rows=$(( grace / 5 * 2 ))

    # Find the max observed tip_age across all post-grace samples per node,
    # plus the most recent sample. Skip rows with '?' (sampler couldn't reach
    # metrics endpoint — already covered by other predicates).
    local result
    result=$(awk -F, -v skip="$skip_rows" '
        NR == 1 { next }                           # header
        NR <= skip + 1 { next }                    # grace window
        $3 == "?" || $3 == "" { next }             # no-data rows
        {
            age = $3 + 0
            if (age > max[$2]) max[$2] = age
            last[$2] = age
            count[$2]++
        }
        END {
            for (n in count) {
                printf "%s %d %d %d\n", n, max[n], last[n], count[n]
            }
        }
    ' "$samples")

    if [ -z "$result" ]; then
        PREDICATE_FAIL+=("p5:tip-age (no usable samples after ${grace}s grace window)")
        return
    fi

    local fails=0
    local summary=""
    while read -r node max last samples_n; do
        [ -z "$node" ] && continue
        if [ "$max" -gt "$threshold" ]; then
            fails=$((fails + 1))
            summary="${summary}${node}=max${max}s/last${last}s/n${samples_n} "
        else
            summary="${summary}${node}=max${max}s/last${last}s/n${samples_n} "
        fi
    done <<< "$result"

    summary="${summary% }"
    if [ "$fails" -eq 0 ]; then
        PREDICATE_PASS+=("p5:tip-age (threshold ${threshold}s; $summary)")
    else
        PREDICATE_FAIL+=("p5:tip-age ($fails/$(echo "$result" | wc -l | tr -d ' ') nodes above ${threshold}s; $summary)")
    fi
}

generate_report() {
    local evd="$1"
    local rpt="$evd/report.md"

    local n_blocks n_txs n_tip_rows
    n_blocks=$(awk -F, 'NR>1 && $5!="?" && $5!=""' "$evd/blocks.csv" 2>/dev/null | wc -l | tr -d ' ')
    n_txs=$(awk -F, 'NR>1' "$evd/tx-submissions.csv" 2>/dev/null | wc -l | tr -d ' ')
    n_tip_rows=$(awk -F, 'NR>1' "$evd/tip-samples.csv" 2>/dev/null | wc -l | tr -d ' ')

    local overall="PASS"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && overall="FAIL"

    {
        echo "# Local Devnet Soak Report"
        echo
        echo "**Overall: $overall**"
        echo
        echo "## Metadata"
        echo
        if [ -f "$evd/metadata.json" ]; then
            echo '```json'
            cat "$evd/metadata.json"
            echo '```'
        fi
        echo
        echo "## Predicate results"
        echo
        echo "| # | Predicate | Result | Detail |"
        echo "|---|-----------|--------|--------|"
        for p in "${PREDICATE_PASS[@]:+${PREDICATE_PASS[@]}}"; do
            id="${p%%:*}"; rest="${p#*:}"; name="${rest%% *}"; detail="${rest#* }"
            printf "| %s | %s | PASS | %s |\n" "$id" "$name" "$detail"
        done
        for p in "${PREDICATE_FAIL[@]:+${PREDICATE_FAIL[@]}}"; do
            id="${p%%:*}"; rest="${p#*:}"; name="${rest%% *}"; detail="${rest#* }"
            printf "| %s | %s | **FAIL** | %s |\n" "$id" "$name" "$detail"
        done
        echo
        echo "## Counts"
        echo
        echo "- Tip-sample rows: $n_tip_rows"
        echo "- Block events: $n_blocks"
        echo "- Submitted txs: $n_txs"
        if [ -f "$evd/forge-attribution.tsv" ]; then
            echo
            echo "## Forge attribution"
            echo
            echo '```'
            cat "$evd/forge-attribution.tsv"
            echo '```'
        fi
        echo
        echo "## Evidence files"
        echo
        for f in tip-samples.csv tip-age-samples.csv blocks.csv tx-submissions.csv metadata.json; do
            [ -f "$evd/$f" ] && echo "- \`$f\` ($(wc -l < "$evd/$f" | tr -d ' ') lines)"
        done
        echo
        echo "Generated by \`verify.sh\` from \`$evd\`."
    } > "$rpt"

    log_info "Report written: $rpt"
}

self_test() {
    local fix="$SCRIPT_DIR/lib/test-fixtures"
    log_info "=== Self-test predicates against fixtures ==="

    log_info "p1 - good fixture (expect PASS)"
    local saved_pass=("${PREDICATE_PASS[@]:+${PREDICATE_PASS[@]}}") saved_fail=("${PREDICATE_FAIL[@]:+${PREDICATE_FAIL[@]}}")
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p1_forge_cross_check "$fix/predicate-1-good.csv"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || die "p1 self-test on good fixture: expected PASS, got ${PREDICATE_FAIL[*]:-}"
    log_info "  OK"

    log_info "p1 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p1_forge_cross_check "$fix/predicate-1-bad.csv"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || die "p1 self-test on bad fixture: expected FAIL, got ${PREDICATE_PASS[*]:-}"
    log_info "  OK"

    # For p2 self-test: force vkey fallback to literal POOL1/POOL2 by pointing
    # LD_KEYS at a non-existent path so cold.vkey lookups fail.
    local saved_ld_keys="$LD_KEYS"
    LD_KEYS="$fix/_nonexistent_keys"

    log_info "p2 - good fixture (expect PASS)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p2_per_bp_attribution "$fix/predicate-2-good.csv"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || { LD_KEYS="$saved_ld_keys"; die "p2 self-test good: expected PASS, got ${PREDICATE_FAIL[*]:-}"; }
    log_info "  OK"

    log_info "p2 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p2_per_bp_attribution "$fix/predicate-2-bad.csv"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || { LD_KEYS="$saved_ld_keys"; die "p2 self-test bad: expected FAIL, got ${PREDICATE_PASS[*]:-}"; }
    log_info "  OK"

    LD_KEYS="$saved_ld_keys"
    # Clean up artifact written by p2 on the good fixture
    rm -f "$fix/forge-attribution.tsv"

    # For p3 self-test: force socket-check to skip the live UTxO query branch
    # by pointing LD_RELAY_SOCK at a non-existent path.
    local saved_relay_sock="$LD_RELAY_SOCK"
    LD_RELAY_SOCK="$fix/_nonexistent.sock"

    log_info "p3 - good fixture (expect PASS)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p3_tx_inclusion "$fix/predicate-3-good.csv" "$fix"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || { LD_RELAY_SOCK="$saved_relay_sock"; die "p3 self-test good: expected PASS, got ${PREDICATE_FAIL[*]:-}"; }
    log_info "  OK"

    log_info "p3 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p3_tx_inclusion "$fix/predicate-3-bad.csv" "$fix"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || { LD_RELAY_SOCK="$saved_relay_sock"; die "p3 self-test bad: expected FAIL, got ${PREDICATE_PASS[*]:-}"; }
    log_info "  OK"

    LD_RELAY_SOCK="$saved_relay_sock"

    log_info "p4 - good fixture (expect PASS)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p4_tip_parity "$fix/predicate-4-good.csv"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || die "p4 self-test good: expected PASS, got ${PREDICATE_FAIL[*]:-}"
    log_info "  OK"

    log_info "p4 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p4_tip_parity "$fix/predicate-4-bad.csv"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || die "p4 self-test bad: expected FAIL, got ${PREDICATE_PASS[*]:-}"
    log_info "  OK"

    # p5 self-test: use a zero-grace window so the fixture's rows are not
    # all skipped. Default grace (60s) assumes a live 5s-cadence soak.
    local saved_grace="${LD_TIP_AGE_GRACE_SEC:-}"
    LD_TIP_AGE_GRACE_SEC=0
    export LD_TIP_AGE_GRACE_SEC

    log_info "p5 - good fixture (expect PASS)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p5_tip_age "$fix/predicate-5-good.csv"
    [ ${#PREDICATE_PASS[@]} -gt 0 ] && [ ${#PREDICATE_FAIL[@]} -eq 0 ] \
        || { unset LD_TIP_AGE_GRACE_SEC; die "p5 self-test good: expected PASS, got ${PREDICATE_FAIL[*]:-}"; }
    log_info "  OK"

    log_info "p5 - bad fixture (expect FAIL)"
    PREDICATE_PASS=(); PREDICATE_FAIL=()
    p5_tip_age "$fix/predicate-5-bad.csv"
    [ ${#PREDICATE_FAIL[@]} -gt 0 ] && [ ${#PREDICATE_PASS[@]} -eq 0 ] \
        || { unset LD_TIP_AGE_GRACE_SEC; die "p5 self-test bad: expected FAIL, got ${PREDICATE_PASS[*]:-}"; }
    log_info "  OK"

    if [ -n "$saved_grace" ]; then
        LD_TIP_AGE_GRACE_SEC="$saved_grace"
        export LD_TIP_AGE_GRACE_SEC
    else
        unset LD_TIP_AGE_GRACE_SEC
    fi

    PREDICATE_PASS=("${saved_pass[@]:+${saved_pass[@]}}"); PREDICATE_FAIL=("${saved_fail[@]:+${saved_fail[@]}}")
    log_info "Self-test complete."
}

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit 0
fi

EVD="${1:?evidence dir required}"
[ -d "$EVD" ] || die "$EVD is not a directory"

p1_forge_cross_check  "$EVD/blocks.csv"
p2_per_bp_attribution "$EVD/blocks.csv"
p3_tx_inclusion       "$EVD/tx-submissions.csv" "$EVD"
p4_tip_parity         "$EVD/tip-samples.csv"
p5_tip_age            "$EVD/tip-age-samples.csv"
generate_report       "$EVD"

if [ ${#PREDICATE_FAIL[@]} -gt 0 ]; then
    log_error "FAILED: ${PREDICATE_FAIL[*]}"
    exit 1
fi
log_info "PASSED all predicates: ${PREDICATE_PASS[*]}"
