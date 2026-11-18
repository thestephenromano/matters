"""Independent CBOR/SHA-384 oracle; public patterned keys and zero signatures.

These are structural vectors, NOT valid keys/signatures or HPKE interoperability.
Uses the pinned cbor2 requirement from ../record-contract/requirements.txt.
"""
import hashlib
import importlib.metadata
import json
from pathlib import Path
import sys
import cbor2

if importlib.metadata.version("cbor2") != "5.9.0":
    raise SystemExit("oracle requires cbor2 5.9.0")
encode = lambda x: cbor2.dumps(x, canonical=True)
digest = lambda x: hashlib.sha384(x).digest()
SUITE = "matters-sync-pq-v0"
KEM = "MLKEM768-P256/draft-ietf-hpke-pq-05"
route = [bytes([i])*16 for i in [1,2,3]]
fp = lambda alg,key: digest(encode([alg,key]))
# Epoch content-key commitment carried in genesis/membership bodies.
commit = lambda generation,epoch,key: digest(encode(["matters-sync/v0/epoch-key-commitment",SUITE,*route,generation,epoch,key]))
key0, key1 = bytes([14])*32, bytes([15])*32


def member(i, admin=False):
    sig=bytes([30+i])*1952
    kem=bytes([60+i])*1249
    device=digest(encode(["matters-sync/v0/device",fp("ML-DSA-65",sig),fp(KEM,kem)]))
    return [device,sig,kem,int(admin)]


def signed(kind,signer,body):
    return [[0,SUITE,kind,*route,signer,body],bytes(3309)]


root=bytes([11])*1952
recovery=bytes([12])*1249
root_id=fp("ML-DSA-65",root)
a,b=member(0,True),member(1)
roster=sorted([a,b],key=lambda x:x[0])
genesis=signed("genesis",root_id,[root,recovery,0,roster,a[0],0,None,commit(0,0,key0)])
manifest=digest(encode(genesis))
membership=signed("membership",a[0],[0,1,manifest,roster,a[0],1,recovery,[],commit(0,1,key1)])
writer=signed("writer_start",a[0],[manifest,bytes([4])*16,a[0],bytes([7])*32,71,None])
envelope=signed("key_envelope",a[0],[manifest,0,b[0],fp(KEM,b[2]),"content-key",bytes([13])*1153,bytes(69),a[0]])
aad=[*envelope[0][:7],[manifest,0,b[0],fp(KEM,b[2]),"content-key",a[0]]]
info=[SUITE,*route,"content-key",fp(KEM,b[2])]
vectors=[dict(kind=v[0][2],wire_hex=encode(v).hex(),signed_hex=encode(v[0]).hex(),hash_hex=digest(encode(v)).hex()) for v in [genesis,membership,writer,envelope]]
maximum=sorted([member(i,i==0) for i in range(16)],key=lambda x:x[0])
max_genesis=signed("genesis",root_id,[root,recovery,0,maximum,a[0],0,None,commit(0,0,key0)])
max_bytes=encode(max_genesis)
# Compact rejection patches against the short writer-start vector.
base=encode(writer)
negative=[]
def reject(name,wire,error):
    i=0
    while i<min(len(base),len(wire)) and base[i]==wire[i]:i+=1
    j=0
    while j<min(len(base),len(wire))-i and base[-1-j]==wire[-1-j]:j+=1
    negative.append(dict(name=name,offset=i,remove=len(base)-i-j,insert_hex=wire[i:len(wire)-j if j else None].hex(),error=error))
reject("nonminimal_outer",b"\x98\x02"+base[1:],"NonCanonical")
reject("nonminimal_version",base[:2]+b"\x18\x00"+base[3:],"NonCanonical")
reject("indefinite_outer",b"\x9f"+base[1:]+b"\xff","Format")
reject("tagged_outer",b"\xd9\xd9\xf7"+base,"Format")
reject("trailing_item",base+b"\x00","Format")
reject("extra_field",encode([writer[0],writer[1],0]),"Format")
reject("short_signature",encode([writer[0],writer[1][:-1]]),"Format")
for field,value,name in [(0,1,"future_version"),(1,"unknown","unknown_suite"),(2,"recovery_transition","unsupported_type")]:
    changed=list(writer[0]);changed[field]=value
    reject(name,encode([changed,writer[1]]),"Unsupported")
result=dict(provenance="cbor2 5.9.0 + Python hashlib SHA-384; patterned public keys and invalid zero signatures",root_id_hex=root_id.hex(),admin_id_hex=a[0].hex(),recipient_id_hex=b[0].hex(),recipient_key_fingerprint_hex=fp(KEM,b[2]).hex(),info_hex=encode(info).hex(),aad_hex=encode(aad).hex(),key_plaintext_hex=encode([0,key0,"whole-collection"]).hex(),key_commitments=[dict(generation=0,epoch=e,key_hex=k.hex(),commitment_hex=commit(0,e,k).hex()) for e,k in [(0,key0),(1,key1)]],vectors=vectors,rejection_splices=negative,max_genesis_bytes=len(max_bytes),max_genesis_sha384=digest(max_bytes).hex())
output=json.dumps(result,indent=2)+"\n"
path=Path(__file__).with_name("golden.json")
if "--check" in sys.argv:
    if path.read_text() != output:
        raise SystemExit("key-distribution fixture drift")
else:
    path.write_text(output)
print(f"4 structural vectors; {len(negative)} rejection cases; 16-member genesis {len(max_bytes)} bytes")
