#!/usr/bin/env python3
"""Byte-level surgery on cardano-cli text-envelope transactions.

Dependency-free (stdlib only) on purpose: tx-zoo already requires python3 for
anchor hashing and the anchor HTTP server, and adding a pip dependency would
make the harness unrunnable on a fresh host — exactly the failure mode that
kept 08r SKIPped behind `socat`.

Why byte-level: `cardano-cli conway transaction build-raw` silently collapses
repeated `--tx-in` arguments (the ledger's `inputs` field is a `Set TxIn`), so
a duplicate-input transaction cannot be produced through the CLI at all. This
tool splices the duplicate straight into the body CBOR, and re-serialises the
envelope so `cardano-cli transaction sign` can sign the MODIFIED body (it
hashes what it is given; the memoised body bytes round-trip verbatim).

Subcommands
-----------
  show      --in FILE                 dump structure: input count, distinct count
  body-hash --in FILE                 blake2b-256 of the tx BODY span (== the txid)
  dup-input --in FILE --out FILE      duplicate one entry of the tx-body input set
                                      [--index N] [--copies K]
  show-certs --in FILE                dump each certificate's own tag integer
  splice-cert-tag --in FILE --out FILE --tag T [--index N]
                                      overwrite one certificate's tag integer
                                      (its first array element) with T,
                                      leaving the rest of the certificate byte-
                                      for-byte untouched — arity intentionally
                                      does not match T's real shape (#1034)
  sign      --in FILE --out FILE --signing-key-file K [-k K2 ...]
                                      attach vkey witnesses, replacing the
                                      witness set, leaving every other byte of
                                      the transaction untouched

All file arguments accept either a text-envelope JSON file
(``{"type":..,"cborHex":..}``) or a file containing bare hex. Output mirrors
the input format.
"""

import argparse
import hashlib
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ed25519_pure  # noqa: E402  (path shim must precede the import)

# ---------------------------------------------------------------------------
# Minimal CBOR span parser.
#
# We do not need values, we need BYTE SPANS: to duplicate an input we copy the
# raw encoding of one array element and re-emit the array head with a bumped
# count. Decoding to Python objects and re-encoding would be wrong — it would
# canonicalise the rest of the body and change the txid for reasons unrelated
# to the test.
# ---------------------------------------------------------------------------

MT_UINT, MT_NINT, MT_BYTES, MT_TEXT, MT_ARRAY, MT_MAP, MT_TAG, MT_SIMPLE = range(8)

INDEFINITE = -1


class CborError(ValueError):
    pass


def _head(buf, pos):
    """Return (major, arg, next_pos). arg is INDEFINITE for indefinite length."""
    if pos >= len(buf):
        raise CborError("truncated CBOR at offset %d" % pos)
    ib = buf[pos]
    major = ib >> 5
    ai = ib & 0x1F
    pos += 1
    if ai < 24:
        return major, ai, pos
    if ai == 24:
        return major, buf[pos], pos + 1
    if ai == 25:
        return major, int.from_bytes(buf[pos:pos + 2], "big"), pos + 2
    if ai == 26:
        return major, int.from_bytes(buf[pos:pos + 4], "big"), pos + 4
    if ai == 27:
        return major, int.from_bytes(buf[pos:pos + 8], "big"), pos + 8
    if ai == 31:
        return major, INDEFINITE, pos
    raise CborError("reserved additional-info %d at offset %d" % (ai, pos - 1))


def skip(buf, pos):
    """Return the offset just past the complete CBOR item starting at pos."""
    major, arg, pos = _head(buf, pos)
    if major in (MT_UINT, MT_NINT):
        return pos
    if major in (MT_BYTES, MT_TEXT):
        if arg == INDEFINITE:
            while buf[pos] != 0xFF:
                pos = skip(buf, pos)
            return pos + 1
        return pos + arg
    if major == MT_ARRAY:
        if arg == INDEFINITE:
            while buf[pos] != 0xFF:
                pos = skip(buf, pos)
            return pos + 1
        for _ in range(arg):
            pos = skip(buf, pos)
        return pos
    if major == MT_MAP:
        if arg == INDEFINITE:
            while buf[pos] != 0xFF:
                pos = skip(buf, pos)
                pos = skip(buf, pos)
            return pos + 1
        for _ in range(arg):
            pos = skip(buf, pos)
            pos = skip(buf, pos)
        return pos
    if major == MT_TAG:
        return skip(buf, pos)
    if major == MT_SIMPLE:
        if arg == INDEFINITE:
            raise CborError("unexpected break at offset %d" % pos)
        return pos
    raise CborError("unhandled major type %d" % major)


def encode_head(major, arg):
    """Minimal-length head encoding for (major, arg). Mirrors canonical CBOR."""
    if arg < 24:
        return bytes([(major << 5) | arg])
    if arg < 0x100:
        return bytes([(major << 5) | 24, arg])
    if arg < 0x10000:
        return bytes([(major << 5) | 25]) + arg.to_bytes(2, "big")
    if arg < 0x100000000:
        return bytes([(major << 5) | 26]) + arg.to_bytes(4, "big")
    return bytes([(major << 5) | 27]) + arg.to_bytes(8, "big")


# ---------------------------------------------------------------------------
# Transaction navigation
# ---------------------------------------------------------------------------

def body_span(buf):
    """Return (start, end) of the transaction BODY map.

    Accepts either a full transaction (array whose first element is the body
    map, which is what a text envelope holds for both `Tx` and `TxBody` types
    in cardano-cli 9+) or a bare body map.
    """
    major, arg, pos = _head(buf, 0)
    if major == MT_MAP:
        return 0, len(buf)
    if major != MT_ARRAY:
        raise CborError("expected transaction array or body map, got major %d" % major)
    start = pos
    return start, skip(buf, start)


def input_set_span(buf, bstart, bend):
    """Locate the tx-body `inputs` field (key 0).

    Returns (head_start, items_start, end, count, is_tagged) where head_start is
    the offset of the ARRAY head (after any tag 258 wrapper), items_start the
    first element, end one past the last. `count` is INDEFINITE for an
    indefinite-length array — cardano-ledger encodes a set of more than 23
    elements that way, so any transaction with 24+ inputs takes that branch.
    """
    major, arg, pos = _head(buf, bstart)
    if major != MT_MAP:
        raise CborError("tx body is not a map (major %d)" % major)
    if arg == INDEFINITE:
        raise CborError("indefinite-length tx body map is not supported")
    for _ in range(arg):
        kstart = pos
        kmajor, karg, kpos = _head(buf, kstart)
        vstart = skip(buf, kstart)
        vend = skip(buf, vstart)
        if kmajor == MT_UINT and karg == 0:
            vpos = vstart
            tagged = False
            vmajor, varg, vnext = _head(buf, vpos)
            if vmajor == MT_TAG:
                tagged = True
                vpos = vnext
                vmajor, varg, vnext = _head(buf, vpos)
            if vmajor != MT_ARRAY:
                raise CborError("tx-body inputs is not an array (major %d)" % vmajor)
            return vpos, vnext, vend, varg, tagged
        pos = vend
    raise CborError("tx body has no inputs field (key 0)")


def element_spans(buf, items_start, count):
    """Byte spans of each array element. `count` may be INDEFINITE."""
    spans = []
    pos = items_start
    if count == INDEFINITE:
        while buf[pos] != 0xFF:
            nxt = skip(buf, pos)
            spans.append((pos, nxt))
            pos = nxt
        return spans
    for _ in range(count):
        nxt = skip(buf, pos)
        spans.append((pos, nxt))
        pos = nxt
    return spans


def dup_input(buf, index=0, copies=2):
    """Return new tx bytes with input `index` present `copies` times."""
    bstart, bend = body_span(buf)
    head_start, items_start, _end, count, _tagged = input_set_span(buf, bstart, bend)
    spans = element_spans(buf, items_start, count)
    n = len(spans)
    if n == 0:
        raise CborError("tx has no inputs to duplicate")
    if index >= n:
        raise CborError("input index %d out of range (count=%d)" % (index, n))
    extra = copies - 1
    if extra < 1:
        raise CborError("--copies must be >= 2")
    estart, eend = spans[index]
    entry = buf[estart:eend]
    if count == INDEFINITE:
        # No length to rewrite — splice the copies in before the break byte.
        return buf[:eend] + entry * extra + buf[eend:]
    new_head = encode_head(MT_ARRAY, n + extra)
    return (
        buf[:head_start]
        + new_head
        + buf[items_start:eend]
        + entry * extra
        + buf[eend:]
    )


def body_field_span(buf, bstart, bend, key):
    """Generalised `input_set_span`: locate ANY integer-keyed tx-body array
    field (e.g. key 4 = certificates), not just key 0 (inputs).

    Returns (head_start, items_start, end, count, is_tagged) — the EXACT
    same 5-tuple shape and field order as `input_set_span` (`head_start` is
    the array head's own position, after any tag(258) wrapper; `items_start`
    is the position of the first element), so callers can reuse
    `element_spans(buf, items_start, count)` unchanged.

    Added for #1034 (19-era-negatives): splicing a certificate's own tag
    byte needs to find the *certs* field (key 4), which is otherwise byte-
    for-byte identical machinery to `input_set_span`'s key-0 walk. Kept as
    a separate function rather than rewriting `input_set_span` in terms of
    it, so `dup-input` (relied on by 08f) cannot regress from this change.
    """
    major, arg, pos = _head(buf, bstart)
    if major != MT_MAP:
        raise CborError("tx body is not a map (major %d)" % major)
    if arg == INDEFINITE:
        raise CborError("indefinite-length tx body map is not supported")
    for _ in range(arg):
        kstart = pos
        kmajor, karg, kpos = _head(buf, kstart)
        vstart = skip(buf, kstart)
        vend = skip(buf, vstart)
        if kmajor == MT_UINT and karg == key:
            vpos = vstart
            tagged = False
            vmajor, varg, vnext = _head(buf, vpos)
            if vmajor == MT_TAG:
                tagged = True
                vpos = vnext
                vmajor, varg, vnext = _head(buf, vpos)
            if vmajor != MT_ARRAY:
                raise CborError(
                    "tx-body field %d is not an array (major %d)" % (key, vmajor)
                )
            return vpos, vnext, vend, varg, tagged
        pos = vend
    raise CborError("tx body has no field (key %d)" % key)


CERT_FIELD_KEY = 4


def splice_cert_tag(buf, index=0, new_tag=6):
    """Return new tx bytes with certificate `index`'s own array-tag integer
    (its FIRST element — the constructor discriminator, e.g. 0 = StakeReg,
    7 = ConwayRegCert) overwritten with `new_tag`.

    Deliberately does NOT touch anything else about the certificate entry:
    not its arity, not its remaining fields. cardano-ledger's Conway
    certificate decoder (see era_conway.rs, #1023) dispatches on the tag
    integer FIRST and hard-fails immediately for tag 5 (GenesisKeyDelegation)
    or 6 (MIR) before it would ever look at the rest of the array — so a
    donor certificate whose remaining fields don't match the target tag's
    real shape (e.g. splicing a 3-element `reg_deposit_cert` [7, cred,
    deposit] to tag 6, whose real shape is 2-element [6, [pot, target]]) is
    exactly the point: the decoder must reject at the TAG, before arity is
    ever examined. This is #1034's regression pin for #1023.
    """
    bstart, bend = body_span(buf)
    _head_start, items_start, _end, count, _tagged = body_field_span(
        buf, bstart, bend, CERT_FIELD_KEY
    )
    spans = element_spans(buf, items_start, count)
    n = len(spans)
    if n == 0:
        raise CborError("tx has no certificates to splice")
    if index >= n:
        raise CborError("cert index %d out of range (count=%d)" % (index, n))
    cstart, cend = spans[index]
    cmajor, carg, cpos = _head(buf, cstart)
    if cmajor != MT_ARRAY:
        raise CborError(
            "certificate entry %d is not an array (major %d)" % (index, cmajor)
        )
    if carg == INDEFINITE or carg == 0:
        raise CborError("certificate entry %d has no tag element to splice" % index)
    tag_major, _tag_val, tag_end = _head(buf, cpos)
    if tag_major != MT_UINT:
        raise CborError(
            "certificate %d tag is not a uint (major %d)" % (index, tag_major)
        )
    new_tag_bytes = encode_head(MT_UINT, new_tag)
    return buf[:cpos] + new_tag_bytes + buf[tag_end:]


TAG_SET = 258


def tx_element_spans(buf):
    """Spans of the top-level transaction array elements [body, wits, valid, aux]."""
    major, arg, pos = _head(buf, 0)
    if major != MT_ARRAY or arg == INDEFINITE:
        raise CborError("expected a definite-length transaction array")
    return element_spans(buf, pos, arg)


def body_hash(buf):
    """blake2b-256 over the BODY span — this is the transaction id."""
    bstart, bend = body_span(buf)
    return hashlib.blake2b(buf[bstart:bend], digest_size=32).digest()


def _read_key_payload(path):
    """Return the raw key bytes from a cardano-cli key text envelope."""
    buf, _env = read_envelope(path)
    major, arg, pos = _head(buf, 0)
    if major != MT_BYTES or arg == INDEFINITE:
        raise CborError("%s: key payload is not a definite-length byte string" % path)
    return buf[pos:pos + arg]


def encode_witness_set(witnesses):
    """CBOR for `{0: 258([[vkey, sig], ...])}` — the vkey-witness-only witness set."""
    out = bytearray()
    out += encode_head(MT_MAP, 1)
    out += encode_head(MT_UINT, 0)
    out += encode_head(MT_TAG, TAG_SET)
    out += encode_head(MT_ARRAY, len(witnesses))
    for vkey, sig in witnesses:
        out += encode_head(MT_ARRAY, 2)
        out += encode_head(MT_BYTES, len(vkey)) + vkey
        out += encode_head(MT_BYTES, len(sig)) + sig
    return bytes(out)


def sign_tx(buf, key_paths):
    """Replace the witness set with vkey witnesses over the CURRENT body bytes.

    Every other byte of the transaction is preserved verbatim — in particular
    the body, so the txid stays whatever the (possibly hand-edited) body
    hashes to.
    """
    spans = tx_element_spans(buf)
    if len(spans) < 2:
        raise CborError("transaction array has no witness-set element")
    msg = body_hash(buf)
    witnesses = []
    for path in key_paths:
        seed = _read_key_payload(path)
        if len(seed) != 32:
            raise CborError(
                "%s: expected a 32-byte Ed25519 seed, got %d bytes "
                "(extended/BIP32 keys are not supported)" % (path, len(seed))
            )
        witnesses.append((ed25519_pure.public_key(seed), ed25519_pure.sign(seed, msg)))
    # `witnessSet.vkeywitness` is a `Set (WitVKey w)` whose Ord instance keys on
    # the KeyHash (blake2b-224 of the vkey), NOT on the raw vkey bytes. Sorting
    # the same way makes multi-key output byte-identical to
    # `cardano-cli transaction sign`.
    witnesses.sort(key=lambda w: hashlib.blake2b(w[0], digest_size=28).digest())
    wstart, wend = spans[1]
    return buf[:wstart] + encode_witness_set(witnesses) + buf[wend:]


PURPOSE_NAMES = {
    0: "Spending", 1: "Minting", 2: "Certifying",
    3: "Rewarding", 4: "Voting", 5: "Proposing",
}


def redeemer_purposes(buf):
    """Return [(tag, index, purpose_name), ...] for every redeemer in the tx.

    Why this exists (#955): a test that submits a certificate guarded by a
    script and merely checks the tx was accepted does NOT prove the Certifying
    ScriptPurpose was constructed — cardano-cli might have built something else
    entirely, or the credential might not have been script-based at all. Reading
    the redeemer tags back off the wire turns "we think we exercised this
    purpose" into "the bytes we submitted contained purpose N".

    Conway (PV>=9) encodes redeemers as a MAP  {[tag, index] => [data, exunits]}.
    Pre-Conway they are an ARRAY of [tag, index, data, exunits]. Both are
    handled: the era gate is exactly the kind of thing this harness exists to
    catch, so refusing to parse one of them would be self-defeating.
    """
    spans = tx_element_spans(buf)
    if len(spans) < 2:
        return []
    wstart, wend = spans[1]
    major, arg, pos = _head(buf, wstart)
    if major != MT_MAP:
        return []
    out = []
    n = arg
    indefinite = (arg == INDEFINITE)
    i = 0
    while True:
        if indefinite:
            if buf[pos] == 0xFF:
                break
        elif i >= n:
            break
        i += 1
        kmaj, karg, kpos = _head(buf, pos)
        after_key = skip(buf, pos)
        vpos = after_key
        if kmaj == MT_UINT and karg == 5:
            out.extend(_parse_redeemers(buf, vpos))
        pos = skip(buf, vpos)
    return out


def _parse_redeemers(buf, pos):
    """Parse the value of witness-set key 5 into [(tag, index, name), ...]."""
    found = []
    major, arg, p = _head(buf, pos)
    if major == MT_MAP:
        # Conway: {[tag, index] => [data, exunits]}
        indefinite = (arg == INDEFINITE)
        i = 0
        while True:
            if indefinite:
                if buf[p] == 0xFF:
                    break
            elif i >= arg:
                break
            i += 1
            # key is a 2-element array [tag, index]
            kmaj, karg, kp = _head(buf, p)
            if kmaj == MT_ARRAY:
                tmaj, tag, tp = _head(buf, kp)
                imaj, idx, ip = _head(buf, tp)
                found.append((tag, idx, PURPOSE_NAMES.get(tag, "Unknown")))
            p = skip(buf, p)      # past key
            p = skip(buf, p)      # past value
    elif major == MT_ARRAY:
        # Pre-Conway: [ [tag, index, data, exunits], ... ]
        indefinite = (arg == INDEFINITE)
        i = 0
        while True:
            if indefinite:
                if buf[p] == 0xFF:
                    break
            elif i >= arg:
                break
            i += 1
            emaj, earg, ep = _head(buf, p)
            if emaj == MT_ARRAY:
                tmaj, tag, tp = _head(buf, ep)
                imaj, idx, ip = _head(buf, tp)
                found.append((tag, idx, PURPOSE_NAMES.get(tag, "Unknown")))
            p = skip(buf, p)
    return found


def describe(buf):
    bstart, bend = body_span(buf)
    _head_start, items_start, _end, count, tagged = input_set_span(buf, bstart, bend)
    spans = element_spans(buf, items_start, count)
    inputs = []
    for estart, eend in spans:
        # Each entry is [ txid_bytes32, index_uint ].
        major, arg, pos = _head(buf, estart)
        if major != MT_ARRAY or arg != 2:
            inputs.append(buf[estart:eend].hex())
            continue
        tmajor, targ, tpos = _head(buf, pos)
        txid = buf[tpos:tpos + targ].hex()
        _imajor, iarg, _ipos = _head(buf, skip(buf, pos))
        inputs.append("%s#%d" % (txid, iarg))
    return {
        "body_span": [bstart, bend],
        "input_set_tagged_258": tagged,
        "input_set_indefinite": count == INDEFINITE,
        "input_count": len(spans),
        "inputs": inputs,
        "distinct_inputs": len(set(inputs)),
    }


def describe_certs(buf):
    """List each certificate's own tag integer (the constructor
    discriminator). Used by #1034 (19-era-negatives) to confirm a splice
    landed on the intended tag before the modified body is re-signed and
    submitted — asserting on the wire bytes rather than trusting the splice
    silently did the right thing.
    """
    bstart, bend = body_span(buf)
    try:
        _head_start, items_start, _end, count, tagged = body_field_span(
            buf, bstart, bend, CERT_FIELD_KEY
        )
    except CborError:
        return {"cert_count": 0, "cert_tags": [], "certs_tagged_258": False}
    spans = element_spans(buf, items_start, count)
    tags = []
    for cstart, cend in spans:
        cmajor, carg, cpos = _head(buf, cstart)
        if cmajor == MT_ARRAY and carg not in (INDEFINITE, 0):
            tmajor, tval, _tp = _head(buf, cpos)
            tags.append(tval if tmajor == MT_UINT else None)
        else:
            tags.append(None)
    return {
        "cert_count": len(spans),
        "cert_tags": tags,
        "certs_tagged_258": tagged,
    }


# ---------------------------------------------------------------------------
# Envelope I/O
# ---------------------------------------------------------------------------

def read_envelope(path):
    with open(path, "r") as f:
        raw = f.read().strip()
    if raw.startswith("{"):
        env = json.loads(raw)
        return bytes.fromhex(env["cborHex"]), env
    return bytes.fromhex(raw), None


def write_envelope(path, buf, env):
    if env is None:
        with open(path, "w") as f:
            f.write(buf.hex() + "\n")
        return
    out = dict(env)
    out["cborHex"] = buf.hex()
    with open(path, "w") as f:
        json.dump(out, f, indent=4)
        f.write("\n")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_show = sub.add_parser("show", help="print input-set structure as JSON")
    p_show.add_argument("--in", dest="inp", required=True)

    p_hash = sub.add_parser("body-hash", help="print blake2b-256 of the tx body (txid)")
    p_hash.add_argument("--in", dest="inp", required=True)

    p_rdmr = sub.add_parser(
        "redeemers",
        help="list the ScriptPurpose tag of every redeemer in the witness set",
    )
    p_rdmr.add_argument("--in", dest="inp", required=True)
    p_rdmr.add_argument(
        "--require",
        dest="require",
        default=None,
        help="purpose name that MUST be present (Spending|Minting|Certifying|"
             "Rewarding|Voting|Proposing); exit 1 if absent",
    )

    p_dup = sub.add_parser("dup-input", help="duplicate one tx-body input entry")
    p_dup.add_argument("--in", dest="inp", required=True)
    p_dup.add_argument("--out", dest="out", required=True)
    p_dup.add_argument("--index", type=int, default=0)
    p_dup.add_argument("--copies", type=int, default=2)

    p_certs = sub.add_parser(
        "show-certs", help="print each certificate's own tag integer as JSON"
    )
    p_certs.add_argument("--in", dest="inp", required=True)

    p_splice = sub.add_parser(
        "splice-cert-tag",
        help="overwrite one certificate's tag integer with an arbitrary "
             "value, leaving its remaining fields (and their arity) "
             "untouched — see #1034 (19-era-negatives)",
    )
    p_splice.add_argument("--in", dest="inp", required=True)
    p_splice.add_argument("--out", dest="out", required=True)
    p_splice.add_argument("--index", type=int, default=0)
    p_splice.add_argument(
        "--tag", type=int, required=True,
        help="new certificate tag, e.g. 6 (MIR) or 5 (GenesisKeyDelegation)",
    )

    p_sign = sub.add_parser("sign", help="attach vkey witnesses to the given body")
    p_sign.add_argument("--in", dest="inp", required=True)
    p_sign.add_argument("--out", dest="out", required=True)
    p_sign.add_argument(
        "--signing-key-file", "-k", dest="keys", action="append", required=True
    )
    p_sign.add_argument(
        "--type",
        dest="env_type",
        default="Tx ConwayEra",
        help="text-envelope type written to --out (default: Tx ConwayEra)",
    )

    args = ap.parse_args(argv)

    try:
        buf, env = read_envelope(args.inp)
        if args.cmd == "show":
            print(json.dumps(describe(buf), indent=2))
            return 0
        if args.cmd == "body-hash":
            print(body_hash(buf).hex())
            return 0
        if args.cmd == "show-certs":
            print(json.dumps(describe_certs(buf), indent=2))
            return 0
        if args.cmd == "splice-cert-tag":
            new = splice_cert_tag(buf, index=args.index, new_tag=args.tag)
            write_envelope(args.out, new, env)
            print(json.dumps(describe_certs(new), indent=2))
            return 0
        if args.cmd == "redeemers":
            rs = redeemer_purposes(buf)
            for tag, idx, name in rs:
                print("%d %d %s" % (tag, idx, name))
            if args.require:
                if not any(name == args.require for _, _, name in rs):
                    print(
                        "tx-cbor-tool: no %s redeemer in this transaction "
                        "(found: %s)"
                        % (args.require,
                           ", ".join(n for _, _, n in rs) or "none"),
                        file=sys.stderr,
                    )
                    return 1
            return 0
        if args.cmd == "sign":
            new = sign_tx(buf, args.keys)
            if env is not None:
                env = dict(env)
                env["type"] = args.env_type
                env["description"] = "Ledger Cddl Format"
            write_envelope(args.out, new, env)
            print(body_hash(new).hex())
            return 0
        new = dup_input(buf, index=args.index, copies=args.copies)
        write_envelope(args.out, new, env)
        print(json.dumps(describe(new), indent=2))
        return 0
    except (CborError, OSError, ValueError) as exc:
        print("tx-cbor-tool: %s" % exc, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
