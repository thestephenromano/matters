#!/usr/bin/env python3
"""Independent known answers for invitation codes, lookups and offers (#37).

Python standard library only: HKDF (RFC 5869) and HMAC over SHA-384, and the
few deterministic-CBOR items the formats use, written out by hand. Run with
`python3 -I invitation_kat.py`; authority_tests.rs embeds its output.
"""
import hashlib
import hmac


def head(major, n):
    if n < 24:
        return bytes([major << 5 | n])
    for size, code in ((1, 24), (2, 25), (4, 26), (8, 27)):
        if n < 1 << (8 * size):
            return bytes([major << 5 | code]) + n.to_bytes(size, "big")
    raise ValueError(n)


def uint(n):
    return head(0, n)


def bstr(b):
    return head(2, len(b)) + b


def tstr(s):
    return head(3, len(s.encode())) + s.encode()


def array(*items):
    return head(4, len(items)) + b"".join(items)


def hkdf_sha384(salt, ikm, info, length):
    prk = hmac.new(salt, ikm, hashlib.sha384).digest()
    out, block, i = b"", b"", 1
    while len(out) < length:
        block = hmac.new(prk, block + info + bytes([i]), hashlib.sha384).digest()
        out += block
        i += 1
    return out[:length]


SUITE = "matters-sync-pq-v0"
APP, TENANT, COLLECTION = bytes([1]) * 16, bytes([2]) * 16, bytes([3]) * 16
SECRET = bytes(range(16))
ROOT, MANIFEST = bytes([0x11]) * 48, bytes([0x22]) * 48


def key(label, length):
    info = array(tstr(label), bstr(APP), bstr(TENANT), bstr(COLLECTION))
    return hkdf_sha384(SUITE.encode(), SECRET, info, length)


def head_items(kind):
    return [uint(0), tstr(SUITE), tstr(kind), bstr(APP), bstr(TENANT),
            bstr(COLLECTION), bstr(ROOT), bstr(MANIFEST)]


CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
value = int.from_bytes(SECRET, "big")
code = "".join(CROCKFORD[(value >> (5 * i)) & 31] for i in reversed(range(26)))

tag = hmac.new(key("matters-sync/v0/invitation-offer", 48),
               array(*head_items("invitation_offer")), hashlib.sha384).digest()
print("code      ", code)
print("lookup    ", key("matters-sync/v0/invitation-lookup", 16).hex())
print("invitation", array(*head_items("invitation"), bstr(SECRET)).hex())
print("offer     ", array(*head_items("invitation_offer"), bstr(tag)).hex())
