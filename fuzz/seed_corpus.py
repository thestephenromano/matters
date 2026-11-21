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
print("seeded", sum(1 for _ in CORPUS.rglob("*") if _.is_file()), "inputs")
