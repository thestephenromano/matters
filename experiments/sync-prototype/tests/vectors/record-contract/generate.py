"""Independent public-data oracle. Requires cbor2==5.6.5, cryptography==46.0.3.

Run with --check to compare against committed bytes; regeneration is explicit.
Zero signatures are STRUCTURAL placeholders, not valid signed records.
"""
import copy
import hashlib
import importlib.metadata
import json
from pathlib import Path
import sys

import cbor2
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.kdf.hkdf import HKDF, HKDFExpand
from cryptography.hazmat.primitives import hmac
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

assert importlib.metadata.version("cbor2") == "5.6.5"
assert importlib.metadata.version("cryptography") == "46.0.3"
enc = lambda value: cbor2.dumps(value, canonical=True)
SUITE = "matters-sync-pq-v0"
SIGNATURE = bytes(3309)
CEK = bytes(range(32))


def header(kind="data", epoch=23, previous=None):
    return [0, SUITE, bytes([1])*16, bytes([2])*16, bytes([3])*16,
            bytes([4])*16, kind, epoch, bytes([5])*48, bytes([6])*48,
            bytes([7])*32, bytes([8])*32, bytes([9])*32, previous]


def base(h, ciphertext):
    return [0, SUITE, h[6], h[2], h[3], h[4], h[9], [h, ciphertext, h[9]]]


def vector(name, h, plain):
    aad = enc(h)
    info = enc(["matters-sync/v0/record-key", h])
    mac = hmac.HMAC(h[11], hashes.SHA256())
    mac.update(CEK)
    prk = mac.finalize()
    key = HKDFExpand(algorithm=hashes.SHA256(), length=32, info=info).derive(prk)
    assert key == HKDF(algorithm=hashes.SHA256(), length=32, salt=h[11], info=info).derive(CEK)
    cipher = AESGCM(key).encrypt(bytes(12), plain, aad)
    b = base(h, cipher)
    return dict(name=name, kind=h[6], epoch=h[7], previous_hex=None if h[13] is None else h[13].hex(),
                header_hex=aad.hex(), info_hex=info.hex(), cek_hex=CEK.hex(),
                prk_hex=prk.hex(), key_hex=key.hex(), plaintext_hex=plain.hex(),
                ciphertext_hex=cipher.hex(), signed_hex=enc(b).hex(),
                structural_wire_hex=enc([b, SIGNATURE]).hex())


vectors = [vector("data", header(), b"public synthetic CRDT change bytes"),
           vector("snapshot", header("snapshot", 2**64-1, bytes([10])*48), b"public snapshot bytes")]
wire = bytes.fromhex(vectors[0]["structural_wire_hex"])
root = cbor2.loads(wire)
negative = []


def reject(name, changed, error):
    # Store a compact deterministic byte splice, not another 3 KiB placeholder signature.
    prefix = 0
    while prefix < min(len(wire), len(changed)) and wire[prefix] == changed[prefix]:
        prefix += 1
    suffix = 0
    while suffix < min(len(wire), len(changed))-prefix and wire[-1-suffix] == changed[-1-suffix]:
        suffix += 1
    negative.append(dict(name=name, offset=prefix, remove=len(wire)-prefix-suffix,
                         insert_hex=changed[prefix:len(changed)-suffix if suffix else None].hex(), error=error))


def mutate(name, path, value, error):
    obj = copy.deepcopy(root)
    target = obj
    for index in path[:-1]:
        target = target[index]
    target[path[-1]] = value
    reject(name, enc(obj), error)


reject("nonminimal_outer_array", b"\x98\x02" + wire[1:], "NonCanonical")
reject("indefinite_outer_array", b"\x9f" + wire[1:] + b"\xff", "NonCanonical")
reject("nonminimal_body_array", wire[:1] + b"\x98\x08" + wire[2:], "NonCanonical")
reject("nonminimal_version", wire[:2] + b"\x18\x00" + wire[3:], "NonCanonical")
reject("nonminimal_suite_length", wire[:3] + b"\x78\x12" + wire[4:], "NonCanonical")
header_start = wire.index(enc(root[0][7][0]))
reject("nonminimal_header_array", wire[:header_start] + b"\x98\x0e" + wire[header_start+1:], "NonCanonical")
app_start = header_start + 1 + len(enc(0)) + len(enc(SUITE))
reject("nonminimal_byte_length", wire[:app_start] + b"\x58\x10" + wire[app_start+1:], "NonCanonical")
reject("indefinite_bytes", wire[:app_start] + b"\x5f" + wire[app_start:app_start+17] + b"\xff" + wire[app_start+17:], "Malformed")
reject("overflowing_byte_length", wire[:app_start] + b"\x5b" + b"\xff"*8, "Malformed")
reject("overflowing_text_length", wire[:3] + b"\x7b" + b"\xff"*8, "Malformed")
epoch_start = app_start + 4*17 + len(enc("data"))
reject("nonminimal_epoch", wire[:epoch_start] + b"\x18\x17" + wire[epoch_start+1:], "NonCanonical")
previous_start = header_start + len(enc(root[0][7][0])) - 1
reject("alternate_null", wire[:previous_start] + b"\xf8\x16" + wire[previous_start+1:], "Malformed")
reject("trailing_item", wire + b"\x00", "Trailing")
reject("tagged_outer", b"\xd9\xd9\xf7" + wire, "Malformed")
reject("map_outer", b"\xa2" + wire[1:], "Malformed")
reject("unbounded_array_claim", b"\x9b" + b"\xff"*8, "Length")
reject("extra_outer_field", enc(root + [0]), "Length")
reject("omitted_outer_field", enc(root[:1]), "Length")
mutate("extra_signed_field", [0], root[0]+[0], "Length")
mutate("extra_body_field", [0, 7], root[0][7]+[0], "Length")
mutate("extra_header_field", [0, 7, 0], root[0][7][0]+[0], "Length")
mutate("future_version", [0, 0], 1, "UnsupportedVersion")
mutate("future_header_version", [0, 7, 0, 0], 1, "UnsupportedVersion")
mutate("unknown_suite", [0, 1], "unknown", "UnsupportedSuite")
mutate("unknown_header_suite", [0, 7, 0, 1], "unknown", "UnsupportedSuite")
mutate("unknown_type", [0, 2], "key_envelope", "UnsupportedType")
mutate("unknown_header_type", [0, 7, 0, 6], "key_envelope", "UnsupportedType")
for field in [2, 3, 4, 5, 8, 9, 10, 11, 12]:
    mutate(f"header_length_{field}", [0, 7, 0, field], root[0][7][0][field][:-1], "Length")
for field in [3, 4, 5, 6]:
    mutate(f"outer_length_{field}", [0, field], root[0][field][:-1], "Length")
mutate("bad_previous_length", [0, 7, 0, 13], bytes(47), "Length")
mutate("short_signature", [1], SIGNATURE[:-1], "Length")
mutate("short_tag", [0, 7, 1], bytes(15), "Length")
mutate("negative_epoch", [0, 7, 0, 7], -1, "Malformed")
mutate("float_epoch", [0, 7, 0, 7], 23.0, "Malformed")
mutate("boolean_epoch", [0, 7, 0, 7], True, "Malformed")
mutate("text_identifier", [0, 7, 0, 2], "1"*16, "Malformed")
mutate("recursive_identifier", [0, 7, 0, 2], [[[[[0]]]]], "Malformed")
mutate("outer_kind_disagreement", [0, 2], "snapshot", "Inconsistent")
for field in [3, 4, 5, 6]:
    mutate(f"outer_context_disagreement_{field}", [0, field], bytes([99])*len(root[0][field]), "Inconsistent")
mutate("body_author_disagreement", [0, 7, 2], bytes([99])*48, "Inconsistent")
mutate("wrong_document", [0, 7, 0, 5], bytes([99])*16, "ContextMismatch")
for outer, inner in [(3, 2), (4, 3), (5, 4)]:
    obj = copy.deepcopy(root)
    obj[0][outer] = obj[0][7][0][inner] = bytes([99])*16
    reject(f"consistent_wrong_route_{inner}", enc(obj), "ContextMismatch")

max_h = header("snapshot", 2**64-1, bytes([10])*48)
max_wire = enc([base(max_h, bytes(1048592)), SIGNATURE])
result = dict(schema="matters-sync-pq-v0/content-record/2026-11-18",
              provenance={"cbor2": "5.6.5", "cryptography": "46.0.3", "signature": "all-zero structural placeholder; never an authenticated vector"},
              max_wire_bytes=len(max_wire), max_overhead_bytes=len(max_wire)-1048592,
              max_wire_sha256=hashlib.sha256(max_wire).hexdigest(),
              vectors=vectors,
              epoch_headers=[dict(epoch=n, header_hex=enc(header(epoch=n)).hex()) for n in
                             [0,23,24,255,256,65535,65536,2**32-1,2**32,2**64-1]],
              rejection_splices=negative)
path = Path(__file__).with_name("golden.json")
output = json.dumps(result, indent=2) + "\n"
if "--check" in sys.argv:
    assert path.read_text() == output, "fixture drift: review before regenerating"
else:
    path.write_text(output)
print(f"2 golden records; {len(negative)} rejection splices; max wire {len(max_wire)}; overhead {len(max_wire)-1048592}")
