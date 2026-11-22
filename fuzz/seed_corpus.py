#!/usr/bin/env python3
"""Seed the fuzz corpora (#36) from the independent golden vectors and known
answers, so the fuzzer starts from valid structures. Run with
`python3 -I fuzz/seed_corpus.py` from the repository root; the output is
committed, and tests/fuzz_replay.rs replays it on stable.
"""
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
VECTORS = ROOT / "experiments/sync-prototype/tests/vectors"
CORPUS = ROOT / "fuzz/corpus"


def write(target, name, data):
    path = CORPUS / target / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


records = json.loads((VECTORS / "record-contract/golden.json").read_text())
for v in records["vectors"]:
    write("record", v["name"], bytes.fromhex(v["structural_wire_hex"]))

controls = json.loads((VECTORS / "key-distribution/golden.json").read_text())
for i, v in enumerate(controls["vectors"]):
    write("control", f"{i}-{v['kind']}", bytes.fromhex(v["wire_hex"]))

kat = subprocess.run(
    [sys.executable, "-I", str(ROOT / "crates/matters-sync/tests/vectors/invitation_kat.py")],
    check=True, capture_output=True, text=True,
).stdout
answers = dict(line.split(None, 1) for line in kat.splitlines() if line.strip())
write("invitation", "invitation", bytes.fromhex(answers["invitation"].strip()))
write("invitation", "offer", bytes.fromhex(answers["offer"].strip()))
write("typed", "code", answers["code"].strip().encode())
write("typed", "code-grouped", b"0004-1061-050r-3gg2-8a1c-60t3-gf")
for name, words in [
    ("zeros", "abandon " * 23 + "art"),
    ("legal", "legal winner thank year wave sausage worth useful " * 2
     + "legal winner thank year wave sausage worth title"),
    ("zoo", "zoo " * 23 + "vote"),
]:
    write("typed", name, words.encode())
kat = subprocess.run(
    [sys.executable, "-I", str(ROOT / "crates/matters-sync/tests/vectors/checkpoint_kat.py")],
    check=True, capture_output=True, text=True,
).stdout
answers = dict(line.split(None, 1) for line in kat.splitlines() if line.strip())
write("checkpoint", "part0", bytes.fromhex(answers["small-part0"].strip()))
write("checkpoint", "coverage", bytes.fromhex(answers["small-coverage"].strip()))


# The client's parsers of relay responses (#45): pages and snapshot lists,
# in the contract's shapes (docs/SYNC_SERVICE_CONTRACT.md), carrying the
# golden records.
def cbor_head(major, n):
    if n < 24:
        return bytes([major << 5 | n])
    for info, width in ((24, 1), (25, 2), (26, 4), (27, 8)):
        if n < 1 << (8 * width):
            return bytes([major << 5 | info]) + n.to_bytes(width, "big")


def cbor(value):
    if isinstance(value, bool):
        return b"\xf5" if value else b"\xf4"
    if isinstance(value, int):
        return cbor_head(0, value)
    if isinstance(value, bytes):
        return cbor_head(2, len(value)) + value
    if isinstance(value, str):
        return cbor_head(3, len(value.encode())) + value.encode()
    if isinstance(value, list):
        return cbor_head(4, len(value)) + b"".join(cbor(v) for v in value)
    if isinstance(value, dict):
        return cbor_head(5, len(value)) + b"".join(
            cbor(k) + cbor(v) for k, v in value.items())
    raise TypeError(value)


generation = bytes(range(16))
wires = [bytes.fromhex(v["structural_wire_hex"]) for v in records["vectors"]]


def page(entries, last, high_water):
    return cbor({
        "generation": generation,
        "high-water": high_water,
        "records": [[s, w] for s, w in entries],
        "next": cbor([generation, last]),
        "complete": last == high_water,
    })


write("relay_page", "complete", page(list(enumerate(wires, 1)), len(wires), len(wires)))
write("relay_page", "partial", page([(1, wires[0])], 1, 7))
write("relay_page", "empty", page([], 0, 0))
write("relay_page", "refusal", cbor({"code": "rate-limited", "retry": True,
                                     "retry-after-ms": 2 ** 64 - 1}))
write("relay_snapshots", "one", cbor({"generation": generation, "records": [[3, wires[-1]]]}))
write("relay_snapshots", "none", cbor({"generation": generation, "records": []}))


# The relay's parsers of outside input (#45), from hand-encoded CBOR of the
# shapes in docs/protocol/sync-relay-v0.cddl.
def canonical_head(major, n):
    if n < 24:
        return bytes([major << 5 | n])
    for extra, size in ((24, 1), (25, 2), (26, 4), (27, 8)):
        if n < 1 << (8 * size):
            return bytes([major << 5 | extra]) + n.to_bytes(size, "big")


def canonical_cbor(value):
    if isinstance(value, int):
        return canonical_head(0, value)
    if isinstance(value, bytes):
        return canonical_head(2, len(value)) + value
    if isinstance(value, str):
        return canonical_head(3, len(value.encode())) + value.encode()
    if isinstance(value, list):
        return canonical_head(4, len(value)) + b"".join(canonical_cbor(v) for v in value)
    if isinstance(value, dict):
        keys = sorted(value, key=lambda k: (len(k.encode()), k.encode()))
        return canonical_head(5, len(value)) + b"".join(canonical_cbor(k) + canonical_cbor(value[k]) for k in keys)
    raise TypeError(value)


route = [bytes([1] * 16), bytes([2] * 16), bytes([3] * 16)]
write("relay-request", "challenge", canonical_cbor({"route": route, "device": bytes([4] * 48)}))
write("relay-request", "session", canonical_cbor({
    "route": route, "device": bytes([4] * 48), "challenge": bytes([5] * 32),
    "signature": bytes([6] * 3309),
}))
write("relay-cursor", "start", canonical_cbor([bytes([7] * 16), 0]))
write("relay-cursor", "later", canonical_cbor([bytes([7] * 16), 300]))
write("relay-export", "empty", canonical_cbor(["matters-sync-export", 0, route, bytes([7] * 16), [], []]))
write("relay-export", "records", canonical_cbor([
    "matters-sync-export", 0, route, bytes([7] * 16),
    [bytes.fromhex(controls["vectors"][0]["wire_hex"])],
    [[1, bytes.fromhex(records["vectors"][0]["structural_wire_hex"])], [3, b"x"]],
]))
write("relay-hint", "hint", b"7:42:" + b"ab" * 16)
write("relay-hint", "first", b"1:1:" + b"00" * 16)


# Structure-aware targets (#45): they sign what they are given, so their
# seeds are the choices they read, not signed bytes.
part0 = bytes.fromhex(answers["small-part0"].strip())
write("signed_record", "data", b"\x00" + b"an update's plaintext")
write("signed_record", "data-after", b"\x02" + bytes(range(48)) + b"after a predecessor")
write("signed_record", "part", b"\x01" + part0)
write("signed_record", "part-after", b"\x03" + bytes(48) + part0)
for i, v in enumerate(controls["vectors"]):
    write("signed_control", f"{i}-{v['kind']}", bytes.fromhex(v["wire_hex"]))
writer = bytes([2]) + bytes([1] * 48) + bytes([2] * 32) + bytes([3] * 48)
write("built_checkpoint", "no-writers", b"\x00\x00" + b"a snapshot's bytes")
write("built_checkpoint", "one-writer", b"\x01" + writer + b"\x01" + b"a snapshot's bytes")
print("seeded", sum(1 for _ in CORPUS.rglob("*") if _.is_file()), "inputs")
