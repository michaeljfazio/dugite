#!/usr/bin/env bash
# nesru-transition-parity.sh — live devnet validation of NewEpochState[4]
# (`nesRu`, the pending RUPD/reward-update) against real cardano-node, across
# the SNothing -> Pulsing -> Complete transition (#1071, Step B of the Fable
# design analysis for the Complete-arm fix landed the same day, c5dbb5b5bd /
# bee26af6b5).
#
# WHY THIS IS A RAW WIRE CHECK, NOT A cardano-cli CHECK
# -------------------------------------------------------
# `cardano-cli query ledger-state`'s JSON renders Haskell's `Pulsing s p` the
# SAME as `SNothing` — `instance ToJSON PulsingRewUpdate` maps both to `Null`
# (`RewardUpdate.hs:359-365`). So the one distinction this script exists to
# check — whether dugite and cardano-node agree on WHEN the fold starts and
# WHEN it completes — is invisible to cardano-cli's own JSON. Only the raw LSQ
# tag-12 (`GetCurrentNewEpochState`/`GetDebugNewEpochState`) bytes carry it,
# via `dugite-network`'s own N2C client (the same Ouroboros LocalStateQuery
# wire protocol either implementation speaks) walking to `NewEpochState[4]`
# and classifying the CBOR head — see
# `crates/dugite-network/examples/capture_nesru.rs`, which this script shells
# out to (built once as `target/release/examples/capture_nesru`, invoked
# directly rather than through `cargo run` so a sampling loop is not paying
# cargo's per-invocation freshness-check overhead).
#
# WHAT DUGITE IS EXPECTED TO DO, PRECISELY
# -----------------------------------------
# `encode_possible_reward_update` (n2c_query/encoding.rs) models exactly two
# of Haskell's three `PulsingRewUpdate` constructors: `SNothing` (array(0))
# and `SJust (Complete r)`. `Pulsing` has NO wire arm — deliberately: dugite's
# internal `Some(PulsingRewUpdate::Pulsing(_))` state renders as `SNothing` on
# the wire (see the doc comment there and
# `crate::node::n2c_query::types::PossibleRewardUpdateSnapshot`). So across
# the window between the `4k/f` freeze and completion, cardano-node's own
# answer legitimately passes through THREE states (SNothing -> Pulsing ->
# Complete) while dugite's answer passes through only TWO (SNothing ->
# Complete). The one ACCEPTABLE mismatch this script must not flag as a
# failure is exactly (cardano=Pulsing, dugite=SNothing) — every other
# disagreement is a real divergence.
#
# TWO PREDICATES
# ---------------
#   (a) Complete-onset alignment. Every tip-pinned sample (both sockets read
#       at the IDENTICAL tip slot — see TIP PINNING below) must have
#       (dugite reports Complete) == (cardano-node reports Complete). If ever
#       one side has completed the fold and the other has not, at the SAME
#       chain tip, the two implementations disagree about when the fold
#       finishes — which is exactly the "two ticks early" class of bug fixed
#       in bee26af6b5. This is checked on EVERY accepted sample, not just at
#       a hand-identified "onset block", because tip-pinning already makes
#       every accepted sample a same-block comparison.
#   (b) Byte-identical Complete payload. Whenever BOTH sides report Complete
#       at the same pinned tip, the raw `nesRu` bytes captured by
#       `capture_nesru` (SJust wrapper, sum tag, and the full RewardUpdate(5)
#       record — deltaT/deltaR/rs/deltaF/nonMyopic) must be byte-identical.
#       This is the confirming check for the four Complete-arm defects fixed
#       earlier in the same session: rs map key shape (Credential
#       array(2)[disc,hash28] vs bare bstr32), rs sort order
#       (ScriptHashObj-first), zero-amount leader-reward entries not being
#       dropped, and Complete-onset timing. A stale shape or a dropped/
#       reordered/mistimed entry cannot produce byte-identical output against
#       a real cardano-node 11.0.1 peer.
#
# NON-VACUITY
# ------------
# Per this repo's standing rule (a check that reports success while measuring
# nothing is the failure class the whole harness exists to catch — see
# feedback_a_field_with_no_writer_compares_vacuously /
# feedback_verify_which_fields_drive_the_verdict), this script requires BOTH
# the Pulsing sub-window AND the Complete state to have actually been
# observed on cardano-node's side, and Complete to have actually been
# observed on dugite's side, before it will report PASS. A run that never
# left SNothing has exercised nothing.
#
# TIP PINNING
# ------------
# Same technique as futurepparams-boundary-parity.sh / ratify-state-parity.sh:
# the two sockets (and the two capture_nesru invocations against them) cannot
# be read atomically, so each sample re-reads both tips after both queries and
# discards itself unless nothing moved on EITHER node. Discarded samples are
# counted, not failed.
#
# Usage: nesru-transition-parity.sh [--seconds N] [--out FILE]
#            [--dugite-sock PATH] [--cardano-sock PATH] [--magic N]
#            [--epoch-length N] [--capture-bin PATH]
#
# The socket/magic/epoch-length defaults come from lib/common.sh (the same
# convention as the sibling scripts), but can be pointed at ANY already-running
# devnet's sockets via the explicit flags — this script does not start or stop
# any node itself.
set +e
unsetopt ERR_EXIT ERR_RETURN 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LD_ROOT="$(cd "$SCRIPT_DIR/../../../../testnet/local-devnet" && pwd)"
. "$LD_ROOT/lib/common.sh"

SECONDS_TO_RUN=900
OUT=""
D="${LD_DUGITE_BP_SOCK:-/tmp/ld-$UID/dbp.sock}"
C="${LD_CARDANO_BP_SOCK:-/tmp/ld-$UID/cbp.sock}"
MAGIC="${LD_MAGIC:-42}"
EPOCH_LEN="${LD_EPOCH_LENGTH:-400}"
CAPTURE_BIN="$LD_ROOT/../../target/release/examples/capture_nesru"
while [ $# -gt 0 ]; do
    case "$1" in
        --seconds) SECONDS_TO_RUN="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --dugite-sock) D="$2"; shift 2 ;;
        --cardano-sock) C="$2"; shift 2 ;;
        --magic) MAGIC="$2"; shift 2 ;;
        --epoch-length) EPOCH_LEN="$2"; shift 2 ;;
        --capture-bin) CAPTURE_BIN="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done
[ -n "$OUT" ] || OUT="$LD_ROOT/evidence/current/nesru-transition-parity.csv"
mkdir -p "$(dirname "$OUT")"

if [ ! -x "$CAPTURE_BIN" ]; then
    echo "FAIL: capture_nesru binary not found at $CAPTURE_BIN — build it first:" >&2
    echo "    cargo build --release -p dugite-network --example capture_nesru" >&2
    exit 2
fi

_tip_slot() { cardano-cli query tip --testnet-magic "$MAGIC" --socket-path "$1" 2>/dev/null | jq -r '.slot // empty'; }

# Mirrors `classify()` in capture_nesru.rs byte-for-byte, so a script-side
# reclassification can never silently disagree with the tool that produced
# the hex it is reading.
#   80                    -> SNOTHING
#   81 <arr-header>=0x80.. 00 -> PULSING   (SJust, inner sum tag 0)
#   81 <arr-header>=0x80.. 01 -> COMPLETE  (SJust, inner sum tag 1)
#   81 ...                -> SJUST_UNKNOWN (unrecognised SJust inner shape)
#   anything else          -> UNRECOGNISED
_classify() {
    local hex="$1" b0 b1 b2
    [ -n "$hex" ] || { echo "QUERY_FAILED"; return; }
    b0="${hex:0:2}"; b1="${hex:2:2}"; b2="${hex:4:2}"
    case "$b0" in
        80) echo "SNOTHING" ;;
        81)
            if [ -n "$b1" ] && [ $((16#$b1)) -ge 128 ]; then
                case "$b2" in
                    00) echo "PULSING" ;;
                    01) echo "COMPLETE" ;;
                    *)  echo "SJUST_UNKNOWN" ;;
                esac
            else
                echo "SJUST_UNKNOWN"
            fi
            ;;
        *) echo "UNRECOGNISED" ;;
    esac
}

echo "ts,slot,epoch,dugite_state,cardano_state,verdict,dugite_hex,cardano_hex" > "$OUT"

deadline=$(( $(date +%s) + SECONDS_TO_RUN ))
compared=0; unstable=0; harness_bugs=0
dugite_complete_seen=0; cardano_complete_seen=0; pulsing_gap_seen=0
onset_mismatches=0; complete_pairs=0; complete_byte_diffs=0
dugite_pulsing_seen=0  # should stay 0 for the whole run — see header

while [ "$(date +%s)" -lt "$deadline" ]; do
    s0d=$(_tip_slot "$D"); s0c=$(_tip_slot "$C")
    dg_hex=$("$CAPTURE_BIN" "$D" "$MAGIC" 2>/dev/null | tail -n1)
    cg_hex=$("$CAPTURE_BIN" "$C" "$MAGIC" 2>/dev/null | tail -n1)
    s1d=$(_tip_slot "$D"); s1c=$(_tip_slot "$C")

    # Tip pinning: any block landing mid-sample, on EITHER node, invalidates
    # the comparison — and the two tips must themselves agree, or the two
    # captures were not taken of the same logical block.
    if [ -z "$s0d" ] || [ -z "$s0c" ] || [ "$s0d" != "$s1d" ] || [ "$s0c" != "$s1c" ] || [ "$s0d" != "$s0c" ]; then
        unstable=$((unstable + 1))
        echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),${s0d:-?},?,?,?,TIP_UNSTABLE,," >> "$OUT"
        continue
    fi
    if [ -z "$dg_hex" ] || [ -z "$cg_hex" ]; then
        unstable=$((unstable + 1))
        echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$s0d,?,QUERY_FAILED,QUERY_FAILED,TIP_UNSTABLE,," >> "$OUT"
        continue
    fi

    epoch=$(( s0d / EPOCH_LEN ))
    dst=$(_classify "$dg_hex")
    cst=$(_classify "$cg_hex")

    [ "$dst" = "COMPLETE" ] && dugite_complete_seen=$((dugite_complete_seen + 1))
    [ "$cst" = "COMPLETE" ] && cardano_complete_seen=$((cardano_complete_seen + 1))
    [ "$cst" = "PULSING" ] && [ "$dst" = "SNOTHING" ] && pulsing_gap_seen=$((pulsing_gap_seen + 1))
    [ "$dst" = "PULSING" ] && dugite_pulsing_seen=$((dugite_pulsing_seen + 1))

    if [ "$dst" = "SJUST_UNKNOWN" ] || [ "$dst" = "UNRECOGNISED" ] || \
       [ "$cst" = "SJUST_UNKNOWN" ] || [ "$cst" = "UNRECOGNISED" ]; then
        harness_bugs=$((harness_bugs + 1))
    fi

    # Predicate (a): Complete onset must agree at every pinned tip. The one
    # allowed disagreement is the documented Pulsing/SNothing gap, which is
    # NOT a mismatch on this predicate (neither side is Complete).
    dcomplete=false; ccomplete=false
    [ "$dst" = "COMPLETE" ] && dcomplete=true
    [ "$cst" = "COMPLETE" ] && ccomplete=true
    verdict="MATCH"
    if [ "$dcomplete" != "$ccomplete" ]; then
        onset_mismatches=$((onset_mismatches + 1))
        verdict="ONSET_MISMATCH"
    elif [ "$dcomplete" = "true" ]; then
        # Predicate (b): both Complete at the same pinned tip — bytes must match.
        complete_pairs=$((complete_pairs + 1))
        if [ "$dg_hex" = "$cg_hex" ]; then
            verdict="COMPLETE_MATCH"
        else
            complete_byte_diffs=$((complete_byte_diffs + 1))
            verdict="COMPLETE_BYTE_DIFF"
        fi
    elif [ "$cst" = "PULSING" ]; then
        verdict="ACCEPTABLE_GAP"
    fi

    compared=$((compared + 1))
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ),$s0d,$epoch,$dst,$cst,$verdict,$dg_hex,$cg_hex" >> "$OUT"
done

echo "nesRu transition parity: compared=$compared tip_unstable=$unstable \
onset_mismatches=$onset_mismatches complete_pairs=$complete_pairs \
complete_byte_diffs=$complete_byte_diffs pulsing_gap_samples=$pulsing_gap_seen \
dugite_complete_samples=$dugite_complete_seen cardano_complete_samples=$cardano_complete_seen \
dugite_pulsing_samples=$dugite_pulsing_seen harness_bugs=$harness_bugs"

rc=0
if [ "$harness_bugs" -gt 0 ]; then
    echo "FAIL: $harness_bugs sample(s) had an unrecognised nesRu wire shape on \
one side — either implementation emitted something outside {SNothing, SJust \
Pulsing, SJust Complete}, or the CBOR walk in capture_nesru.rs needs updating — \
see $OUT" >&2
    rc=1
fi
if [ "$dugite_pulsing_seen" -gt 0 ]; then
    echo "FAIL: dugite reported the Pulsing wire shape $dugite_pulsing_seen \
time(s) — #1071's encoder deliberately has no Pulsing arm and must render \
SNothing for that internal state; this means the encoder shape changed \
without this script being told, or an unmodelled third value crept onto the \
wire" >&2
    rc=1
fi
if [ "$onset_mismatches" -gt 0 ]; then
    echo "FAIL: $onset_mismatches sample(s) where dugite and cardano-node \
disagreed on whether the RUPD fold had completed AT THE SAME PINNED TIP — \
Complete-onset timing divergence — see rows with verdict=ONSET_MISMATCH in \
$OUT" >&2
    rc=1
fi
if [ "$complete_byte_diffs" -gt 0 ]; then
    echo "FAIL: $complete_byte_diffs sample(s) where BOTH sides reported \
Complete but the nesRu bytes differed — see rows with \
verdict=COMPLETE_BYTE_DIFF in $OUT" >&2
    rc=1
fi
if [ "$compared" -lt 20 ]; then
    echo "FAIL: only $compared tip-stable comparisons — this measured nothing" >&2
    rc=1
fi
if [ "$cardano_complete_seen" -lt 1 ] || [ "$dugite_complete_seen" -lt 1 ]; then
    echo "INCONCLUSIVE: never observed BOTH sides report Complete \
(dugite=$dugite_complete_seen cardano=$cardano_complete_seen) — run longer or \
across more epoch boundaries; predicate (b) was never exercised" >&2
    rc=1
fi
if [ "$pulsing_gap_seen" -lt 1 ]; then
    echo "INCONCLUSIVE: never observed cardano-node in the Pulsing state \
(with dugite correctly still reporting SNothing) — the fold may have \
completed in a single pulse (pulse_size covered every credential in one \
block) before this sampler caught the intermediate state, or the run never \
crossed the 4k/f mark. Sample faster or run across more epoch boundaries" >&2
    rc=1
fi
exit $rc
