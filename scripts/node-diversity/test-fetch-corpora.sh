#!/usr/bin/env bash
# Self-test of fetch-corpora.sh: a corrupted pin must FAIL the fetch, and for the right reason.
#
# Offline.  Builds tiny local git repositories and archives, writes a pin file that points at
# them, runs the real fetch script against it, then corrupts one pin (or one input) at a time and
# asserts a non-zero exit AND the message that names the cause.  A pass-through script would
# fail here, and so would a script that fails for an unrelated reason.
#
# Run by `just ndc-selftest` and by the `fetch_script_rejects_corrupted_pins` test.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
FETCH="$HERE/fetch-corpora.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_TERMINAL_PROMPT=0
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

passed=0
fail() {
    echo "SELFTEST FAILED: $*" >&2
    exit 1
}

flip() { # change the first hex digit, keeping the shape of a commit or digest
    local value=$1
    if [[ ${value:0:1} == 0 ]]; then echo "1${value:1}"; else echo "0${value:1}"; fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# make_repo DIR -> prints the commit; the caller has already written the files into DIR
make_repo() {
    git init -q "$1"
    git -C "$1" config uploadpack.allowFilter true
    git -C "$1" config uploadpack.allowAnySHA1InWant true
    git -C "$1" add -A
    git -C "$1" commit -q -m fixture
    git -C "$1" rev-parse HEAD
}

# ── the fixtures ───────────────────────────────────────────────────────────────────────────
write_ndc() { # DIR [extra: ships-m]
    local fx="$1/scenarios/ledger-rules/fixtures"
    mkdir -p "$fx/scenarios" "$fx/era-history" "$fx/protocol-parameters"
    echo '{"title":"a","category":"x","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/p.json"},"initial_state":{"utxo":[],"pools":[]},"expected":"Pass"}' >"$fx/scenarios/00001-pass-a.json"
    echo '{"title":"b","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/m.json"},"initial_state":{"utxo":[]},"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json"
    echo '{"era":1}' >"$fx/era-history/e.json"
    echo '{"p":1}' >"$fx/protocol-parameters/p.json"
    echo '{"$schema":"x"}' >"$fx/schema.json"
    if [[ ${2:-} == ships-m ]]; then echo '{"m":1}' >"$fx/protocol-parameters/m.json"; fi
    if [[ ${2:-} == edit-b ]]; then
        echo '{"title":"CHANGED","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/m.json"},"initial_state":{"utxo":[]},"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json"
    fi
}

write_amaru() { # DIR
    local t="$1/crates/t"
    mkdir -p "$t/scenarios" "$t/common/era-history" "$t/common/protocol-parameters"
    echo '{"title":"a","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/p.json"},"initial_state":{"utxo":[],"pools":[],"accounts":[]},"expected":"Pass"}' >"$t/scenarios/00001-pass-a.json"
    echo '{"title":"b","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/m.json"},"initial_state":{"utxo":[]},"expected":"Pass"}' >"$t/scenarios/00002-pass-b.json"
    echo '{"era":1}' >"$t/common/era-history/e.json"
    echo '{"p":1}' >"$t/common/protocol-parameters/p.json"
    echo '{"m":1}' >"$t/common/protocol-parameters/m.json"
    echo '{"$schema":"x"}' >"$t/schema.json"
}

mkdir "$WORK/ndc" "$WORK/ndc-ships" "$WORK/ndc-edit" "$WORK/amaru" "$WORK/cbor" "$WORK/art"
write_ndc "$WORK/ndc"
write_ndc "$WORK/ndc-ships" ships-m
write_ndc "$WORK/ndc-edit" edit-b
write_amaru "$WORK/amaru"
echo '{"cbor":1}' >"$WORK/cbor/x.json"
NDC=$(make_repo "$WORK/ndc")
NDC_SHIPS=$(make_repo "$WORK/ndc-ships")
NDC_EDIT=$(make_repo "$WORK/ndc-edit")
AMARU=$(make_repo "$WORK/amaru")
CBOR=$(make_repo "$WORK/cbor")
M_SHA=$(sha256_of "$WORK/amaru/crates/t/common/protocol-parameters/m.json")

mkdir "$WORK/art/good" "$WORK/art/evil"
echo hello >"$WORK/art/good/a.txt"
tar -c -C "$WORK/art/good" . | zstd -q -o "$WORK/good.tar.zst"
python3 -I - "$WORK/evil.tar" <<'PY'
import io, sys, tarfile
with tarfile.open(sys.argv[1], "w") as t:
    data = b"escaped"
    info = tarfile.TarInfo("../escaped.txt")
    info.size = len(data)
    t.addfile(info, io.BytesIO(data))
PY
zstd -q -o "$WORK/evil.tar.zst" "$WORK/evil.tar"
GOOD_SHA=$(sha256_of "$WORK/good.tar.zst")
EVIL_SHA=$(sha256_of "$WORK/evil.tar.zst")

# write_pins FILE KEY=VALUE...  (defaults describe a consistent corpus)
write_pins() {
    local file=$1
    shift
    local ndc=$NDC ndc_repo="file://$WORK/ndc" amaru=$AMARU cbor=$CBOR m_sha=$M_SHA uplc_sha=$GOOD_SHA uplc_url="file://$WORK/good.tar.zst"
    local known='' missing_count=1 turbo_sha=$GOOD_SHA
    local kv
    for kv in "$@"; do
        case $kv in
            ndc=*) ndc=${kv#*=} ;;
            ndc_repo=*) ndc_repo=${kv#*=} ;;
            amaru=*) amaru=${kv#*=} ;;
            cbor=*) cbor=${kv#*=} ;;
            m_sha=*) m_sha=${kv#*=} ;;
            uplc_sha=*) uplc_sha=${kv#*=} ;;
            uplc_url=*) uplc_url=${kv#*=} ;;
            turbo_sha=*) turbo_sha=${kv#*=} ;;
            known=*) known=${kv#*=} ;;
            missing_count=*) missing_count=${kv#*=} ;;
        esac
    done
    cat >"$file" <<EOF
[ndc]
repository = "$ndc_repo"
commit = "$ndc"
[amaru]
repository = "file://$WORK/amaru"
dataset_commit = "$amaru"
scenarios_dir = "crates/t/scenarios"
common_dir = "crates/t/common"
[[ledger_rules.missing_protocol_parameters]]
file = "m.json"
sha256 = "$m_sha"
fixtures = $missing_count
[ledger_rules.known_divergences]
$known
[cbor_dataset]
repository = "file://$WORK/cbor"
commit = "$cbor"
[uplc_conformance]
url = "$uplc_url"
sha256 = "$uplc_sha"
[uplc_turbo]
url = "file://$WORK/good.tar.zst"
sha256 = "$turbo_sha"
EOF
}

# run_fetch NAME PINS_FILE COMPONENTS... -> sets $out and $code; each run has its own empty target dir
run_fetch() {
    local name=$1 pins=$2
    shift 2
    code=0
    out=$(DUGITE_NDC_PINS="$pins" DUGITE_NDC_DIR="$WORK/run-$name" "$FETCH" "$@" 2>&1) || code=$?
}

expect_ok() { # NAME PINS COMPONENTS...
    local name=$1
    run_fetch "$@"
    [[ $code -eq 0 ]] || fail "$name: expected success, exit $code: $out"
    passed=$((passed + 1))
    echo "ok   $name"
}

expect_fail() { # NAME 'message substring' PINS COMPONENTS...
    local name=$1 needle=$2
    shift 2
    run_fetch "$name" "$@"
    [[ $code -ne 0 ]] || fail "$name: a corrupted pin was ACCEPTED (exit 0): $out"
    grep -qF -- "$needle" <<<"$out" || fail "$name: failed, but not for the expected reason ('$needle'): $out"
    passed=$((passed + 1))
    echo "ok   $name -> exit $code, '$needle'"
}

# ── the consistent corpus passes ──────────────────────────────────────────────────────────
write_pins "$WORK/good.toml" 'known='
expect_ok consistent-corpus "$WORK/good.toml"
[[ -f "$WORK/run-consistent-corpus/ledger-rules/protocol-parameters/m.json" ]] || fail "the missing parameter file was not added to ledger-rules/"
[[ -f "$WORK/run-consistent-corpus/uplc-conformance/a.txt" ]] || fail "the tarball was not extracted"
# a second run on the same directory is a no-op that still passes
DUGITE_NDC_PINS="$WORK/good.toml" DUGITE_NDC_DIR="$WORK/run-consistent-corpus" "$FETCH" >/dev/null || fail "an idempotent re-run failed"
passed=$((passed + 1))
echo "ok   idempotent re-run"

# ── a corrupted pin fails the fetch ───────────────────────────────────────────────────────
write_pins "$WORK/p1.toml" "cbor=$(flip "$CBOR")"
expect_fail cbor-commit-flipped 'cannot fetch' "$WORK/p1.toml" cbor
write_pins "$WORK/p2.toml" "ndc=$(flip "$NDC")"
expect_fail ndc-commit-flipped 'cannot fetch' "$WORK/p2.toml" ndc
write_pins "$WORK/p3.toml" "uplc_sha=$(flip "$GOOD_SHA")"
expect_fail uplc-sha-flipped 'pin is' "$WORK/p3.toml" uplc
write_pins "$WORK/p4.toml" "turbo_sha=$(flip "$GOOD_SHA")"
expect_fail turbo-sha-flipped 'pin is' "$WORK/p4.toml" turbo
write_pins "$WORK/p5.toml" "m_sha=$(flip "$M_SHA")"
expect_fail missing-parameter-sha-flipped 'pin is' "$WORK/p5.toml" ledger-rules
write_pins "$WORK/p6.toml" "uplc_sha=$EVIL_SHA" "uplc_url=file://$WORK/evil.tar.zst"
expect_fail archive-escaping-its-directory 'parent-relative' "$WORK/p6.toml" uplc
write_pins "$WORK/p7.toml" "uplc_sha=nothex"
expect_fail malformed-digest 'is not' "$WORK/p7.toml" uplc

# ── the ledger-rules checks ───────────────────────────────────────────────────────────────
write_pins "$WORK/l1.toml" "ndc=$NDC_SHIPS" "ndc_repo=file://$WORK/ndc-ships"
expect_fail ndc-ships-a-pinned-file 'now in node-diversity-conformance' "$WORK/l1.toml" ledger-rules
write_pins "$WORK/l2.toml" "ndc=$NDC_EDIT" "ndc_repo=file://$WORK/ndc-edit"
expect_fail unlisted-divergence 'corpora.toml does not list it' "$WORK/l2.toml" ledger-rules
write_pins "$WORK/l3.toml" 'known="00001" = ["title"]'
expect_fail stale-divergence-entry 'remove the entry' "$WORK/l3.toml" ledger-rules
write_pins "$WORK/l4.toml" 'missing_count=5'
expect_fail wrong-fixture-count 'the pin says 5' "$WORK/l4.toml" ledger-rules
write_pins "$WORK/l5.toml" 'known="00002" = ["title"]' "ndc=$NDC_EDIT" "ndc_repo=file://$WORK/ndc-edit"
expect_ok listed-divergence-accepted "$WORK/l5.toml" ledger-rules

echo "fetch-corpora self-test: $passed checks passed"
