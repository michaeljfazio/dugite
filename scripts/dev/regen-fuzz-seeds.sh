#!/usr/bin/env bash
#
# Regenerate fuzz/seeds/<target>/ from material already committed to the repo.
#
# Why a script rather than hand-copied blobs: seeds are derived data. Every
# input here exists somewhere else in the tree as a test fixture or a network
# config, and this script is the record of which one. Re-run it after adding a
# fixture worth fuzzing from.
#
# Why fuzz/seeds/ rather than fuzz/corpus/ (issue #972): cargo-fuzz owns
# corpus/fuzz_<target>/ and writes minimised inputs into it during a run, so it
# is gitignored. The three real-block seeds this repo shipped for months lived
# in corpus/decode_block/ — a directory cargo-fuzz never reads, because it
# derives the path from the BIN name (fuzz_decode_block). They were loaded by
# nothing. seeds/ is tracked; CI copies it into corpus/fuzz_<target>/ before
# fuzzing.
#
# Usage: scripts/dev/regen-fuzz-seeds.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

SEEDS="fuzz/seeds"

# Targets declared as [[bin]] in fuzz/Cargo.toml, without the fuzz_ prefix.
#
# Seeds are only written for targets that exist. A seed directory naming a
# target that does not exist is never read by anything — that is #972's
# original defect (corpus/decode_block/ held three real blocks for months while
# cargo-fuzz read corpus/fuzz_decode_block/). xtask/tests/fuzz_matrix_coverage.rs
# asserts the same property from the other side.
DECLARED="$(sed -n 's/^name = "fuzz_\(.*\)"$/\1/p' fuzz/Cargo.toml)"

declared() {
    printf '%s\n' "$DECLARED" | grep -qx "$1"
}

# hex fixture -> raw bytes
unhex() {
    local src="$1" dst="$2"
    tr -d '[:space:]' < "$src" | xxd -r -p > "$dst"
}

# Copy a file verbatim into one or more target seed dirs.
seed_raw() {
    local src="$1" name="$2"
    shift 2
    for target in "$@"; do
        declared "$target" || continue
        mkdir -p "$SEEDS/$target"
        cp "$src" "$SEEDS/$target/$name"
    done
}

# Decode a hex fixture into one or more target seed dirs.
seed_hex() {
    local src="$1" name="$2"
    shift 2
    for target in "$@"; do
        declared "$target" || continue
        mkdir -p "$SEEDS/$target"
        unhex "$src" "$SEEDS/$target/$name"
    done
}

echo "==> regenerating $SEEDS from repo fixtures"
rm -rf "$SEEDS"

# ---------------------------------------------------------------------------
# Which targets are worth seeding with real CBOR
#
# Only targets whose input IS the wire bytes. `tx_validation`, `block_apply`
# and `mempool_admission` build their structures from a fixed byte-struct
# layout (hash at [0..32], fee at [32..40], control bytes, ...) rather than
# decoding CBOR, so real transaction bytes are no better than random there —
# seeding them would just inflate the corpus.
#
# `body_hash` reads a 33-byte prefix (32-byte claimed hash + control byte)
# before the block bytes, so its seeds are prefixed to stay aligned.
# ---------------------------------------------------------------------------
BLOCK_TARGETS=(decode_block encode_roundtrip)
TX_TARGETS=(decode_transaction encode_roundtrip)

# body_hash seed = 32 zero bytes + control 0x03 (round_trip | test_extract,
# so the recomputed hash is used and both code paths run) + block CBOR.
seed_body_hash() {
    local src="$1" name="$2"
    declared body_hash || return 0
    mkdir -p "$SEEDS/body_hash"
    {
        head -c 32 /dev/zero
        printf '\x03'
        cat "$src"
    } > "$SEEDS/body_hash/$name"
}

# ---------------------------------------------------------------------------
# Blocks — real on-chain CBOR, one per era plus the awkward-encoding fixtures.
# ---------------------------------------------------------------------------
for era in shelley mary alonzo babbage conway; do
    src="crates/dugite-serialization/tests/test_vectors/$era.hex"
    [ -f "$src" ] || continue
    seed_hex "$src" "block-$era" "${BLOCK_TARGETS[@]}"
    tmp="$(mktemp)"; unhex "$src" "$tmp"; seed_body_hash "$tmp" "block-$era"; rm -f "$tmp"
done

# Indefinite-length and bignum encodings — the shapes that produced #932/#937.
for f in crates/dugite-serialization/tests/fixtures/*.cbor; do
    [ -f "$f" ] || continue
    name="block-$(basename "$f" .cbor)"
    seed_raw "$f" "$name" "${BLOCK_TARGETS[@]}"
    seed_body_hash "$f" "$name"
done

# ---------------------------------------------------------------------------
# Transactions — standalone tx CBOR.
#
# tx-96ae78f7 is the 324-asset preprod transaction pinned by #930: it is the
# only fixture in the tree that straddles the encodeMap 255/256 header-width
# boundary, which is exactly the boundary #930 and #938 got wrong.
# ---------------------------------------------------------------------------
for f in crates/dugite-ledger/src/validation/fixtures/tx-*.hex; do
    [ -f "$f" ] || continue
    seed_hex "$f" "$(basename "$f" .hex)" "${TX_TARGETS[@]}"
done

for f in crates/dugite-serialization/test_data/*.hex \
         crates/dugite-serialization/tests/fixtures/*.hex; do
    [ -f "$f" ] || continue
    seed_hex "$f" "tx-$(basename "$f" .hex)" "${TX_TARGETS[@]}"
done

# ---------------------------------------------------------------------------
# Genesis + topology JSON (#975).
#
# Every network's real genesis, so the fuzzer mutates from a document that
# parses rather than from random bytes. The float->rational fields that broke
# in v2.2.0 (priceSteps, a0, rho, tau, the governance thresholds) only appear
# in a document this shape.
# ---------------------------------------------------------------------------
for net in mainnet preview preprod; do
    for kind in shelley alonzo conway; do
        src="config/$net/$kind-genesis.json"
        [ -f "$src" ] && seed_raw "$src" "$net-$kind-genesis.json" genesis_parse
    done
    for topo in topology cn-topology; do
        src="config/$net/$topo.json"
        # fuzz_topology_parse reads the first 8 bytes as a current-slot value
        # and the rest as the document, so a bare JSON seed is misaligned — the
        # opening `{"Producers"` gets eaten as the slot and the parse fails.
        # Prefix 8 zero bytes to match the layout. (Same class as body_hash's
        # 33-byte prefix; a seed that does not match the target's input layout
        # is not a seed.)
        if [ -f "$src" ] && declared topology_parse; then
            mkdir -p "$SEEDS/topology_parse"
            { head -c 8 /dev/zero; cat "$src"; } > "$SEEDS/topology_parse/$net-$topo.json"
        fi
    done
done

# ---------------------------------------------------------------------------
# CLI key material (#975).
#
# Synthesised, not copied: the repo holds no committed key material and must
# not start. These are structurally real key encodings over all-zero bytes.
#
# fuzz_cli_envelope reads byte 0 as a width selector and everything after it as
# the payload, so each seed carries that prefix. Index 3 of the target's WIDTHS
# table is 32 — the Ed25519 key width.
# ---------------------------------------------------------------------------
seed_cli() {
    local name="$1" hexbody="$2"
    declared cli_envelope || return 0
    mkdir -p "$SEEDS/cli_envelope"
    { printf '\x03'; printf '%s' "$hexbody" | xxd -r -p; } > "$SEEDS/cli_envelope/$name"
}
seed_cli_text() {
    local name="$1" text="$2"
    declared cli_envelope || return 0
    mkdir -p "$SEEDS/cli_envelope"
    { printf '\x03'; printf '%s' "$text"; } > "$SEEDS/cli_envelope/$name"
}
ZERO32="0000000000000000000000000000000000000000000000000000000000000000"
# 0x58 0x20 — the canonical 2-byte CBOR header for a 32-byte payload, the shape
# unwrap_key_bytes must accept.
seed_cli "cbor-wrapped-32" "5820$ZERO32"
# The same 32 bytes with NO CBOR wrapper. Its first byte is 0x00 here, but the
# pre-#935 `& 0xe0` heuristic ate the first byte of any raw key starting
# 0x40..=0x5f — so mutation around this seed walks straight into that range.
seed_cli "raw-32" "$ZERO32"
# A raw key that BEGINS 0x58, i.e. one that looks like a CBOR header and is not.
seed_cli "raw-32-leading-0x58" "58${ZERO32:2}"
# Text forms for parse_inline_verification_key: hex and bech32.
seed_cli_text "hex-32.txt" "$ZERO32"
seed_cli_text "bech32-vkey.txt" \
    "ed25519_pk1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqz5sqce"

# ---------------------------------------------------------------------------
echo
printf '%-34s %s\n' "TARGET" "SEEDS"
for d in "$SEEDS"/*/; do
    printf '%-34s %s\n' "$(basename "$d")" "$(find "$d" -type f | wc -l | tr -d ' ')"
done
echo
echo "total: $(find "$SEEDS" -type f | wc -l | tr -d ' ') files, $(du -sh "$SEEDS" | cut -f1)"
