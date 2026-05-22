#!/usr/bin/env bash
# Download the official UPLC conformance test vectors maintained by
# the Plutus project (IntersectMBO/plutus) into
# crates/dugite-uplc/tests/conformance/.
#
# By default fetches the *latest stable* Plutus release; override with
#   PLUTUS_VERSION=1.65.0.0 ./scripts/dev/download-plutus-conformance.sh
#
# Mirrors pragma-org/uplc's `download-plutus-tests` Justfile recipe but
# pins to a release tag (not master) and records the resolved tag in a
# PLUTUS_VERSION file so future runs can detect drift.

set -euo pipefail

cd "$(dirname "$0")/../.."

DEST="crates/dugite-uplc/tests/conformance"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Resolve target version. Allow override via env; otherwise query the
# GitHub API for the latest release.
if [[ -z "${PLUTUS_VERSION:-}" ]]; then
  echo "==> Querying IntersectMBO/plutus for the latest release..."
  if command -v gh >/dev/null 2>&1; then
    PLUTUS_VERSION=$(gh release view --repo IntersectMBO/plutus --json tagName --jq .tagName)
  else
    PLUTUS_VERSION=$(curl -fsSL https://api.github.com/repos/IntersectMBO/plutus/releases/latest \
      | python3 -c 'import sys,json;print(json.load(sys.stdin)["tag_name"])')
  fi
fi

echo "==> Plutus release tag: $PLUTUS_VERSION"

URL="https://github.com/IntersectMBO/plutus/archive/refs/tags/${PLUTUS_VERSION}.tar.gz"
ARCHIVE="$WORK/plutus.tar.gz"

echo "==> Downloading $URL"
curl -fL --progress-bar "$URL" -o "$ARCHIVE"

echo "==> Extracting plutus-conformance/test-cases/uplc/evaluation/"
SUBDIR="plutus-${PLUTUS_VERSION}/plutus-conformance/test-cases/uplc/evaluation"
tar -xzf "$ARCHIVE" -C "$WORK" "$SUBDIR"

if [[ ! -d "$WORK/$SUBDIR" ]]; then
  echo "ERROR: archive did not contain $SUBDIR" >&2
  exit 1
fi

echo "==> Installing into $DEST"
rm -rf "$DEST"
mkdir -p "$DEST"
# Copy the *contents* of evaluation/ into the destination (so the
# top-level dirs there become 'builtin', 'example', 'term', ...).
cp -R "$WORK/$SUBDIR/." "$DEST/"

# Record the resolved version so build.rs / tests can sanity-check
# against the skip list.
printf '%s\n' "$PLUTUS_VERSION" > "$DEST/PLUTUS_VERSION"

# Quick inventory.
COUNT=$(find "$DEST" -name '*.uplc' -not -name '*.expected' | wc -l | tr -d ' ')
echo "==> Installed $COUNT .uplc test vectors from plutus $PLUTUS_VERSION"
echo "    Skip list: crates/dugite-uplc/tests/conformance_skip.txt"
echo "    Run:       cargo test -p dugite-uplc --features conformance --test conformance"
