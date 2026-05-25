#!/usr/bin/env bash
# Refresh the vendored utxorpc/spec proto files in crates/dugite-rpc/proto/
# to a specific upstream tag.
#
# Usage: bump-utxorpc-spec.sh <tag>
#   <tag>   utxorpc/spec git tag (e.g. v0.19.2, v0.20.0)
#
# Side effects:
#   * Clones the tag into a tempdir, copies cardano-only .proto files
#     (cardano + sync + query + submit + watch) for both v1alpha and
#     v1beta into crates/dugite-rpc/proto/utxorpc/. Bitcoin + handshake
#     are intentionally skipped — Dugite is Cardano-only.
#   * Rewrites crates/dugite-rpc/proto/VERSION with the new tag, the
#     resolved commit sha, and today's date.
#   * Runs `cargo build -p dugite-rpc` to surface codegen breakage.
#   * Runs `cargo nextest run -p dugite-rpc` so golden tests catch
#     protobuf shape drift before the resulting commit is pushed.
#
# After this script succeeds, review the diff under
# `crates/dugite-rpc/proto/` and commit the changes as a focused PR
# titled e.g. "chore(rpc): bump utxorpc/spec → vX.Y.Z (#672)".

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <tag>" >&2
    echo "       e.g. $0 v0.19.2" >&2
    exit 2
fi

TAG="$1"
REPO_ROOT="$(git rev-parse --show-toplevel)"
PROTO_DST="$REPO_ROOT/crates/dugite-rpc/proto"
TMP_DIR="$(mktemp -d -t utxorpc-spec-XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "==> Cloning utxorpc/spec @ $TAG into $TMP_DIR"
git -C "$TMP_DIR" init -q
git -C "$TMP_DIR" remote add origin https://github.com/utxorpc/spec.git
git -C "$TMP_DIR" fetch --depth 1 origin "tag" "$TAG" -q
git -C "$TMP_DIR" checkout "$TAG" -q
COMMIT="$(git -C "$TMP_DIR" rev-parse HEAD)"
echo "    resolved commit: $COMMIT"

if [[ ! -d "$TMP_DIR/proto/utxorpc" ]]; then
    echo "ERROR: $TMP_DIR/proto/utxorpc not found in tag $TAG" >&2
    exit 1
fi

echo "==> Refreshing $PROTO_DST"
# Wipe the cardano-only subset and re-copy. Don't touch VERSION (handled
# below), don't touch bitcoin/handshake (intentionally omitted).
for v in v1alpha v1beta; do
    for pkg in cardano sync query submit watch; do
        if [[ -f "$TMP_DIR/proto/utxorpc/$v/$pkg/$pkg.proto" ]]; then
            mkdir -p "$PROTO_DST/utxorpc/$v/$pkg"
            cp "$TMP_DIR/proto/utxorpc/$v/$pkg/$pkg.proto" \
                "$PROTO_DST/utxorpc/$v/$pkg/$pkg.proto"
            echo "    $v/$pkg/$pkg.proto"
        else
            echo "    SKIP $v/$pkg (not in upstream at $TAG)"
        fi
    done
done

echo "==> Rewriting VERSION"
cat > "$PROTO_DST/VERSION" <<EOF
source       = "https://github.com/utxorpc/spec"
tag          = "$TAG"
commit       = "$COMMIT"
fetched_at   = "$(date -u +%Y-%m-%d)"

# Vendored subset: Cardano chain only (v1alpha + v1beta of cardano, sync,
# query, submit, watch). Bitcoin and Handshake .proto are intentionally
# omitted — Dugite is a Cardano-only node and the unused codegen would add
# build cost without consumer.
#
# Refresh via: just bump-utxorpc-spec <tag>
# Provenance: this file is the single source of truth for spec pinning;
# bumping spec without bumping this file (or vice versa) is a CI failure.
EOF

echo "==> Building dugite-rpc to surface codegen breakage"
(cd "$REPO_ROOT" && cargo build -p dugite-rpc 1>&2)

echo "==> Running dugite-rpc tests"
(cd "$REPO_ROOT" && cargo nextest run -p dugite-rpc 1>&2)

echo
echo "==> Done. Review the diff under crates/dugite-rpc/proto/ and commit"
echo "    as e.g.  chore(rpc): bump utxorpc/spec → $TAG (#672)"
