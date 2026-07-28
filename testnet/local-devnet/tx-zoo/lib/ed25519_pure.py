#!/usr/bin/env python3
"""Pure-python Ed25519 (RFC 8032) — stdlib only, no pip dependencies.

Vendored so tx-zoo can sign a transaction body that `cardano-cli transaction
sign` REFUSES to touch. cardano-cli decodes the body before signing it, and
cardano-ledger's Conway decoder hard-fails on a duplicate set element
("Final number of elements: 1 does not match the total count that was decoded:
2"), so a duplicate-input body can never be signed through the CLI.

This is the RFC 8032 reference construction with extended (X, Y, Z, T)
coordinates so a signature costs milliseconds rather than seconds.

SAFETY NOTE FOR TEST AUTHORS: never trust this to be correct on its own. Any
caller must first sign a body that cardano-cli CAN sign and byte-compare the
two signatures (Ed25519 is deterministic, so they must be identical). tx-zoo's
08f does exactly that and records an env-skip if they diverge — otherwise a
broken signer would produce a rejected-for-the-wrong-reason transaction and the
negative test would pass vacuously.
"""

import hashlib

P = 2 ** 255 - 19
L = 2 ** 252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)


def _point_add(p1, p2):
    a = (p1[1] - p1[0]) * (p2[1] - p2[0]) % P
    b = (p1[1] + p1[0]) * (p2[1] + p2[0]) % P
    c = 2 * p1[3] * p2[3] * D % P
    dd = 2 * p1[2] * p2[2] % P
    e, f, g, h = b - a, dd - c, dd + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _point_mul(s, p1):
    acc = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            acc = _point_add(acc, p1)
        p1 = _point_add(p1, p1)
        s >>= 1
    return acc


def _recover_x(y, sign):
    if y >= P:
        return None
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P != 0:
        x = x * SQRT_M1 % P
    if (x * x - x2) % P != 0:
        return None
    if (x & 1) != sign:
        x = P - x
    return x


_G_Y = 4 * pow(5, P - 2, P) % P
_G_X = _recover_x(_G_Y, 0)
G = (_G_X, _G_Y, 1, _G_X * _G_Y % P)


def _compress(point):
    zinv = pow(point[2], P - 2, P)
    x = point[0] * zinv % P
    y = point[1] * zinv % P
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def _sha512_modl(data):
    return int.from_bytes(hashlib.sha512(data).digest(), "little") % L


def _expand(seed):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def public_key(seed):
    """32-byte public key for a 32-byte Ed25519 seed (the Cardano skey payload)."""
    if len(seed) != 32:
        raise ValueError("ed25519 seed must be 32 bytes, got %d" % len(seed))
    a, _ = _expand(seed)
    return _compress(_point_mul(a, G))


def sign(seed, message):
    """Deterministic 64-byte Ed25519 signature over `message`."""
    if len(seed) != 32:
        raise ValueError("ed25519 seed must be 32 bytes, got %d" % len(seed))
    a, prefix = _expand(seed)
    pub = _compress(_point_mul(a, G))
    r = _sha512_modl(prefix + message)
    big_r = _compress(_point_mul(r, G))
    k = _sha512_modl(big_r + pub + message)
    s = (r + k * a) % L
    return big_r + int.to_bytes(s, 32, "little")


if __name__ == "__main__":
    # RFC 8032 section 7.1 test vector 1 — a self-check that does not need a
    # network, a node, or cardano-cli.
    seed = bytes.fromhex(
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
    )
    pub = bytes.fromhex(
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    )
    sig = bytes.fromhex(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
        "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
    )
    assert public_key(seed) == pub, "RFC 8032 pubkey vector failed"
    assert sign(seed, b"") == sig, "RFC 8032 signature vector failed"
    print("ed25519_pure: RFC 8032 vector 1 OK")
