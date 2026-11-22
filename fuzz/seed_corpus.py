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
print("seeded", sum(1 for _ in CORPUS.rglob("*") if _.is_file()), "inputs")
