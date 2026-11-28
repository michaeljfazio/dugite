#!/usr/bin/env bash
# Fetch and verify every corpus of the node-diversity conformance harness (#1114, #1116).
#
#   fetch-corpora.sh [component ...]
#
# Components: ndc amaru ledger-rules cbor uplc turbo.  No argument means all of them.
# `ledger-rules` needs `ndc` and `amaru` and runs them first.
#
# Every pin comes from tests/conformance/node-diversity/corpora.toml, and every pin is checked:
# git checkouts must land on the pinned commit, tarballs must match their SHA-256.  Any mismatch
# exits non-zero.  Reporting never relies on a corpus that was not verified.
#
# Layout under $DUGITE_NDC_DIR (default target/node-diversity/corpora, gitignored):
#   ndc/             NDC at its pinned commit: schemas, site builder, ledger-rules fixtures
#   amaru/           Amaru at amaru.dataset_commit, sparse: crates/.../transaction only
#   ledger-rules/    the fixtures a runner reads: NDC's, plus the protocol-parameter files NDC
#                    lacks, taken from amaru/ and checked by SHA-256
#   cbor-dataset/    r2rationality/cardano-cbor-dataset at its pinned commit
#   uplc-conformance/, uplc-turbo/    the extracted tarballs, each checked by SHA-256
#
# ledger-rules additionally checks that NDC's fixtures EQUAL Amaru's at the pinned commit (the
# value a report names as `dataset_commit` is otherwise one nobody verified), and FAILS when NDC
# starts shipping a protocol-parameter file this script adds, so the workaround gets removed.
#
# Downloaded data is untrusted: it is only fetched, hashed, parsed as JSON and read.  Nothing in it
# is executed.  Idempotent: a component already at its pin is kept; one at a different revision is
# never replaced (the script stops and names it).
#
# Env: DUGITE_NDC_PINS  path of the pin file (default: the one in the repo; the self-test overrides it)
#      DUGITE_NDC_DIR   target directory
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PINS=${DUGITE_NDC_PINS:-$ROOT/tests/conformance/node-diversity/corpora.toml}
DIR=${DUGITE_NDC_DIR:-$ROOT/target/node-diversity/corpora}

die() {
    echo "fetch-corpora: $*" >&2
    exit 1
}

# shellcheck disable=SC2154  # assigned by eval
eval "$(python3 -I "$HERE/pins.py" "$PINS")" || die "cannot read pins from $PINS"

mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd)
export GIT_TERMINAL_PROMPT=0

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# checkout REPOSITORY COMMIT TARGET [SPARSE_DIR]
checkout() {
    local repository=$1 commit=$2 target=$3 sparse=${4:-} actual
    if [[ -e "$target" ]]; then
        actual=$(git -C "$target" rev-parse HEAD 2>/dev/null || true)
        [[ "$actual" == "$commit" ]] || die "$target is at ${actual:-no git revision}, pin is $commit: remove it"
        [[ -z "$(git -C "$target" status --porcelain 2>/dev/null)" ]] || die "$target has local changes: remove it"
        echo "$target: at $commit"
        return
    fi
    local partial="$target.partial"
    rm -rf "$partial"
    git init -q "$partial"
    if [[ -n "$sparse" ]]; then
        git -C "$partial" sparse-checkout set "$sparse"
        git -C "$partial" fetch -q --depth 1 --filter=blob:none "$repository" "$commit" || die "cannot fetch $commit from $repository"
    else
        git -C "$partial" fetch -q --depth 1 "$repository" "$commit" || die "cannot fetch $commit from $repository"
    fi
    git -C "$partial" checkout -q FETCH_HEAD
    actual=$(git -C "$partial" rev-parse HEAD)
    [[ "$actual" == "$commit" ]] || die "$repository: fetched $actual, pin is $commit"
    mv "$partial" "$target"
    echo "$target: checked out $commit"
}

# extract NAME URL SHA256 TARGET
extract() {
    local name=$1 url=$2 digest=$3 target=$4 marker="$4.sha256"
    if [[ -d "$target" && -f "$marker" && "$(cat "$marker")" == "$digest" ]]; then
        echo "$target: extracted ($digest)"
        return
    fi
    local work="$DIR/.work-$name"
    rm -rf "$work"
    mkdir -p "$work"
    curl --fail --silent --show-error --location --retry 3 -o "$work/archive.tar.zst" "$url" || die "$name: cannot download $url"
    local actual
    actual=$(sha256_of "$work/archive.tar.zst")
    if [[ "$actual" != "$digest" ]]; then
        rm -rf "$work"
        die "$name: $url has sha256 $actual, pin is $digest"
    fi
    # Refuse an archive that could write outside its directory, or that holds links or devices.
    local listing
    listing=$(zstd -dcq "$work/archive.tar.zst" | tar -t) || die "$name: archive does not unpack"
    if grep -Eq '^/|(^|/)\.\.(/|$)' <<<"$listing"; then
        rm -rf "$work"
        die "$name: archive has an absolute or parent-relative path"
    fi
    if zstd -dcq "$work/archive.tar.zst" | tar -tv | cut -c1 | grep -qv '[-d]'; then
        rm -rf "$work"
        die "$name: archive holds something other than files and directories"
    fi
    mkdir "$work/out"
    zstd -dcq "$work/archive.tar.zst" | tar -x -C "$work/out"
    rm -rf "$target"
    mv "$work/out" "$target"
    echo "$digest" >"$marker"
    rm -rf "$work"
    echo "$target: extracted $url"
}

do_ndc() {
    checkout "$NDC_REPO" "$NDC_COMMIT" "$DIR/ndc"
}

do_amaru() {
    checkout "$AMARU_REPO" "$AMARU_DATASET_COMMIT" "$DIR/amaru" "${AMARU_SCENARIOS_DIR%/*}"
}

do_ledger_rules() {
    local ndc_fixtures="$DIR/ndc/scenarios/ledger-rules/fixtures" target="$DIR/ledger-rules" partial="$DIR/ledger-rules.partial"
    [[ -d "$ndc_fixtures" && -d "$DIR/amaru" ]] || die "ledger-rules needs the ndc and amaru components"
    # 1. NDC must lack exactly the pinned files, with the pinned fixture counts.
    python3 -I "$HERE/ledger_rules_check.py" unresolved "$PINS" "$ndc_fixtures"
    # 2. Assemble the fixture root: NDC's fixtures plus the missing parameter files, from Amaru.
    rm -rf "$partial"
    cp -R "$ndc_fixtures" "$partial"
    local entry file digest rest source
    for entry in ${MISSING_PARAMS[@]+"${MISSING_PARAMS[@]}"}; do
        file=${entry%%:*}
        rest=${entry#*:}
        digest=${rest%%:*}
        source="$DIR/amaru/$AMARU_COMMON_DIR/protocol-parameters/$file"
        [[ -f "$source" ]] || die "$file: not in Amaru at $AMARU_DATASET_COMMIT"
        if [[ "$(sha256_of "$source")" != "$digest" ]]; then
            rm -rf "$partial"
            die "$file: Amaru has sha256 $(sha256_of "$source"), pin is $digest"
        fi
        cp "$source" "$partial/protocol-parameters/$file"
        echo "$file: from Amaru ($digest)"
    done
    # 3. NDC's fixtures must equal Amaru's, apart from the listed divergences; every $ref resolves.
    if ! python3 -I "$HERE/ledger_rules_check.py" compare "$PINS" "$partial" "$DIR/amaru"; then
        rm -rf "$partial"
        die "ledger-rules fixtures do not match Amaru at $AMARU_DATASET_COMMIT"
    fi
    rm -rf "$target"
    mv "$partial" "$target"
}

do_cbor() {
    checkout "$CBOR_REPO" "$CBOR_COMMIT" "$DIR/cbor-dataset"
}

do_uplc() {
    extract uplc "$UPLC_URL" "$UPLC_SHA256" "$DIR/uplc-conformance"
}

do_turbo() {
    extract turbo "$TURBO_URL" "$TURBO_SHA256" "$DIR/uplc-turbo"
}

components=("$@")
if [[ ${#components[@]} -eq 0 ]]; then
    components=(ndc amaru ledger-rules cbor uplc turbo)
fi
for component in "${components[@]}"; do
    case "$component" in
        ndc | amaru | ledger-rules | cbor | uplc | turbo) ;;
        *) die "unknown component '$component' (ndc amaru ledger-rules cbor uplc turbo)" ;;
    esac
done
wants() {
    local component
    for component in "${components[@]}"; do
        [[ "$component" == "$1" ]] && return 0
    done
    return 1
}

if wants ndc || wants ledger-rules; then do_ndc; fi
if wants amaru || wants ledger-rules; then do_amaru; fi
if wants ledger-rules; then do_ledger_rules; fi
if wants cbor; then do_cbor; fi
if wants uplc; then do_uplc; fi
if wants turbo; then do_turbo; fi
echo "fetch-corpora: OK (${components[*]}) in $DIR"
