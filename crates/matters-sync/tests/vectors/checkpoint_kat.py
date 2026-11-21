#!/usr/bin/env python3
"""Independent known answers for the checkpoint encoding (#40,
sync-checkpoint-v0.cddl): a coverage statement, its ID, and the parts of a
one-part and a two-part checkpoint.

Python standard library only: SHA-384 and the few deterministic-CBOR items
the format uses, written out by hand. Run with `python3 -I
checkpoint_kat.py`; checkpoint_tests.rs embeds the digests of its output,
and fuzz/seed_corpus.py seeds the checkpoint corpus from it.
"""
import hashlib

CHUNK = 512 * 1024


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
    b = s.encode()
    return head(3, len(b)) + b


def array(items):
    return head(4, len(items)) + b"".join(items)


NULL = b"\xf6"


def sha384(b):
    return hashlib.sha384(b).digest()


def checkpoint(snapshot, pending):
    pending = sorted(set(pending), key=sha384)
    payload = array([bstr(snapshot), array([bstr(p) for p in pending])])
    writers = [  # ascending by incarnation
        array([bstr(bytes([1] * 48)), bstr(bytes([1] * 32)), uint(9), bstr(bytes([101] * 48))]),
        array([bstr(bytes([2] * 48)), bstr(bytes([2] * 32)), uint(5), bstr(bytes([102] * 48))]),
    ]
    parts = -(-len(payload) // CHUNK)
    coverage = array([
        tstr("matters-checkpoint"), uint(0), tstr("loro-1"), uint(17), uint(3),
        array(writers), bstr(bytes([1, 2, 3])), bstr(bytes([4, 5])),
        array([bstr(sha384(p)) for p in pending]),
        array([bstr(bytes([7] * 16)), uint(42)]),
        bstr(sha384(payload)), uint(len(payload)), uint(parts),
    ])
    cid = sha384(coverage)
    out = []
    for i in range(parts):
        chunk = payload[i * CHUNK:(i + 1) * CHUNK]
        out.append(array([
            tstr("matters-checkpoint-part"), uint(0), bstr(cid), uint(i), bstr(chunk),
            bstr(coverage) if i == 0 else NULL,
        ]))
    return coverage, cid, out


def snapshot(n):
    return bytes(i % 251 for i in range(n))


if __name__ == "__main__":
    coverage, cid, parts = checkpoint(snapshot(1000), [b"pending b", b"pending a"])
    print("small-coverage", coverage.hex())
    print("small-id", cid.hex())
    print("small-part0", parts[0].hex())
    print("small-part0-sha384", sha384(parts[0]).hex())
    _, cid, parts = checkpoint(snapshot(CHUNK + 77), [])
    print("large-id", cid.hex())
    for i, p in enumerate(parts):
        print(f"large-part{i}-sha384", sha384(p).hex())
