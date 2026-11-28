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
export DUGITE_NDC_ALLOW_LOCAL=1 # the self-test's pins use file:// URLs
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
    echo '{"title":"b","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/m.json"},"initial_state":{"utxo":[]},"n":1,"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json"
    mkdir -p "$fx/test-credentials"
    echo 'aa' >"$fx/test-credentials/k.vkey"
    echo '{"era":1}' >"$fx/era-history/e.json"
    echo '{"p":1}' >"$fx/protocol-parameters/p.json"
    echo '{"$schema":"x"}' >"$fx/schema.json"
    if [[ ${2:-} == ships-m ]]; then echo '{"m":1}' >"$fx/protocol-parameters/m.json"; fi
    local b='"era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/m.json"},"initial_state":{"utxo":[]}'
    case ${2:-} in
        edit-b) echo '{"title":"CHANGED","network":"preprod",'"$b"',"n":1,"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json" ;;
        # true and 1.0 are not 1: Python's == would call them equal
        bool-b) echo '{"title":"b","network":"preprod",'"$b"',"n":true,"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json" ;;
        float-b) echo '{"title":"b","network":"preprod",'"$b"',"n":1.0,"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json" ;;
        dupkey-b) echo '{"title":"b","title":"b2","network":"preprod",'"$b"',"n":1,"expected":"Pass"}' >"$fx/scenarios/00002-pass-b.json" ;;
        cred) echo 'bb' >"$fx/test-credentials/k.vkey" ;;
        extra) echo 'x' >"$fx/test-credentials/extra.json" ;;
        prefix) cp "$fx/scenarios/00001-pass-a.json" "$fx/scenarios/00001-pass-a-again.json" ;;
    esac
}

write_amaru() { # DIR
    local t="$1/crates/t"
    mkdir -p "$t/scenarios" "$t/common/era-history" "$t/common/protocol-parameters"
    echo '{"title":"a","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/p.json"},"initial_state":{"utxo":[],"pools":[],"accounts":[]},"expected":"Pass"}' >"$t/scenarios/00001-pass-a.json"
    echo '{"title":"b","network":"preprod","era_history":{"$ref":"common/era-history/e.json"},"protocol_parameters":{"$ref":"common/protocol-parameters/m.json"},"initial_state":{"utxo":[]},"n":1,"expected":"Pass"}' >"$t/scenarios/00002-pass-b.json"
    mkdir -p "$t/common/test-credentials"
    echo 'aa' >"$t/common/test-credentials/k.vkey"
    echo '{"era":1}' >"$t/common/era-history/e.json"
    echo '{"p":1}' >"$t/common/protocol-parameters/p.json"
    echo '{"m":1}' >"$t/common/protocol-parameters/m.json"
    echo '{"$schema":"x"}' >"$t/schema.json"
}

mkdir "$WORK/ndc" "$WORK/amaru" "$WORK/cbor" "$WORK/art"
write_ndc "$WORK/ndc"
variants=(ships-m edit-b bool-b float-b dupkey-b cred extra prefix)
for v in "${variants[@]}"; do
    mkdir "$WORK/ndc-$v"
    write_ndc "$WORK/ndc-$v" "$v"
done
write_amaru "$WORK/amaru"
echo '{"cbor":1}' >"$WORK/cbor/x.json"
NDC=$(make_repo "$WORK/ndc")
declare -A NDC_V
for v in "${variants[@]}"; do NDC_V[$v]=$(make_repo "$WORK/ndc-$v"); done
NDC_SHIPS=${NDC_V[ships-m]}
NDC_EDIT=${NDC_V[edit-b]}
AMARU=$(make_repo "$WORK/amaru")
CBOR=$(make_repo "$WORK/cbor")
M_SHA=$(sha256_of "$WORK/amaru/crates/t/common/protocol-parameters/m.json")

mkdir "$WORK/art/good" "$WORK/art/evil" "$WORK/art/link"
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
echo hello >"$WORK/art/link/a.txt"
ln -s a.txt "$WORK/art/link/b.txt"
tar -c -C "$WORK/art/link" . | zstd -q -o "$WORK/link.tar.zst"
LINK_SHA=$(sha256_of "$WORK/link.tar.zst")
GOOD_SHA=$(sha256_of "$WORK/good.tar.zst")
EVIL_SHA=$(sha256_of "$WORK/evil.tar.zst")

# write_pins FILE KEY=VALUE...  (defaults describe a consistent corpus)
write_pins() {
    local file=$1
    shift
    local ndc=$NDC ndc_repo="file://$WORK/ndc" amaru=$AMARU cbor=$CBOR m_sha=$M_SHA uplc_sha=$GOOD_SHA uplc_url="file://$WORK/good.tar.zst"
    local only='' known='' missing_count=1 turbo_sha=$GOOD_SHA
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
            only=*) only=${kv#*=} ;;
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
[ledger_rules]
ndc_only_files = [$only]
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
write_pins "$WORK/p8.toml" "uplc_sha=$LINK_SHA" "uplc_url=file://$WORK/link.tar.zst"
expect_fail archive-with-a-symlink 'something other than files and directories' "$WORK/p8.toml" uplc
write_pins "$WORK/p9.toml" "cbor=$CBOR"
sed -i.bak 's|file://|http://|' "$WORK/p9.toml" && rm "$WORK/p9.toml.bak"
code=0
out=$(DUGITE_NDC_ALLOW_LOCAL=0 DUGITE_NDC_PINS="$WORK/p9.toml" DUGITE_NDC_DIR="$WORK/run-p9" "$FETCH" cbor 2>&1) || code=$?
[[ $code -ne 0 ]] && grep -qF 'is not an https URL' <<<"$out" || fail "a non-https pin was accepted: $out"
passed=$((passed + 1))
echo "ok   non-https-url"
write_pins "$WORK/p7.toml" "uplc_sha=nothex"
expect_fail malformed-digest 'is not' "$WORK/p7.toml" uplc

# ── the ledger-rules checks ───────────────────────────────────────────────────────────────
write_pins "$WORK/l1.toml" "ndc=$NDC_SHIPS" "ndc_repo=file://$WORK/ndc-ships-m"
expect_fail ndc-ships-a-pinned-file 'now in node-diversity-conformance' "$WORK/l1.toml" ledger-rules
write_pins "$WORK/l2.toml" "ndc=$NDC_EDIT" "ndc_repo=file://$WORK/ndc-edit-b"
expect_fail unlisted-divergence 'corpora.toml does not list it' "$WORK/l2.toml" ledger-rules
write_pins "$WORK/l3.toml" 'known="00001" = ["title"]'
expect_fail stale-divergence-entry 'remove the entry' "$WORK/l3.toml" ledger-rules
write_pins "$WORK/l4.toml" 'missing_count=5'
expect_fail wrong-fixture-count 'the pin says 5' "$WORK/l4.toml" ledger-rules
write_pins "$WORK/l6.toml" "ndc=${NDC_V[bool-b]}" "ndc_repo=file://$WORK/ndc-bool-b"
expect_fail true-is-not-1 "NDC differs from Amaru at ['n']" "$WORK/l6.toml" ledger-rules
write_pins "$WORK/l7.toml" "ndc=${NDC_V[float-b]}" "ndc_repo=file://$WORK/ndc-float-b"
expect_fail 1.0-is-not-1 "NDC differs from Amaru at ['n']" "$WORK/l7.toml" ledger-rules
write_pins "$WORK/l8.toml" "ndc=${NDC_V[dupkey-b]}" "ndc_repo=file://$WORK/ndc-dupkey-b"
expect_fail duplicate-json-key "duplicate JSON key 'title'" "$WORK/l8.toml" ledger-rules
write_pins "$WORK/l9.toml" "ndc=${NDC_V[cred]}" "ndc_repo=file://$WORK/ndc-cred"
expect_fail test-credentials-differ 'common/test-credentials/k.vkey: differs from Amaru' "$WORK/l9.toml" ledger-rules
write_pins "$WORK/l10.toml" "ndc=${NDC_V[extra]}" "ndc_repo=file://$WORK/ndc-extra"
expect_fail unlisted-ndc-only-file 'not listed in ndc_only_files' "$WORK/l10.toml" ledger-rules
write_pins "$WORK/l11.toml" 'only="test-credentials/gone.json"'
expect_fail stale-ndc-only-entry 'listed in ndc_only_files' "$WORK/l11.toml" ledger-rules
write_pins "$WORK/l12.toml" "ndc=${NDC_V[prefix]}" "ndc_repo=file://$WORK/ndc-prefix"
expect_fail shared-id-prefix 'share the id 00001' "$WORK/l12.toml" ledger-rules
# a leaf path that is listed must match exactly: listing 'title' does not excuse a change of 'n' or 'description'
write_pins "$WORK/l13.toml" 'known="00002" = ["title", "extra.path"]' "ndc=$NDC_EDIT" "ndc_repo=file://$WORK/ndc-edit-b"
expect_fail stale-leaf-path "'extra.path' is listed in known_divergences but no longer differs" "$WORK/l13.toml" ledger-rules
write_pins "$WORK/l5.toml" 'known="00002" = ["title"]' "ndc=$NDC_EDIT" "ndc_repo=file://$WORK/ndc-edit-b"
expect_ok listed-divergence-accepted "$WORK/l5.toml" ledger-rules

# A failed compare must not leave fixtures from older pins in place.
mkdir "$WORK/stale"
DUGITE_NDC_PINS="$WORK/l5.toml" DUGITE_NDC_DIR="$WORK/stale" "$FETCH" ledger-rules >/dev/null || fail "setup of stale-fixture case failed"
[[ -d "$WORK/stale/ledger-rules" ]] || fail "stale-fixture case: ledger-rules/ not built"
code=0
out=$(DUGITE_NDC_PINS="$WORK/l3.toml" DUGITE_NDC_DIR="$WORK/stale" "$FETCH" ledger-rules 2>&1) || code=$?
[[ $code -ne 0 ]] || fail "stale-fixture case: the failing compare was accepted"
[[ ! -e "$WORK/stale/ledger-rules" ]] || fail "a failed compare left the old ledger-rules/ in place"
passed=$((passed + 1))
echo "ok   failed-compare-removes-old-fixtures"

echo "fetch-corpora self-test: $passed checks passed"
