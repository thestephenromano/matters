# Content-record conformance fixtures

These public synthetic fixtures bind the [content-record contract](../../../../../docs/SYNC_RECORD_CONTRACT.md) to exact bytes. They contain no real application data or secret material. They are not published standards vectors or a security certification.

`golden.json` contains two records (`data`, `snapshot`), ten epoch-width boundary headers, and 58 compact rejection splices. The record signatures are **all-zero structural placeholders**. The Rust crypto tests generate separate signatures with public fixed seeds/randomness and verify the exact original transcript; they never treat the placeholders as authenticated records. Device IDs in these fixtures are synthetic roster inputs, not independently verified key fingerprints.

`generate.py` uses cbor2 5.6.5 for RFC 8949 deterministic encoding and cryptography 46.0.3 for HKDF-SHA256/AES-256-GCM. It emits header/AAD, KDF info, PRK, derived key, ciphertext/tag, signed-body and complete structural-record bytes. This oracle is independent of the Rust minicbor/libcrux path. cddl-cat 0.7.1, using Ciborium 0.2.2, separately validates the CDDL shape. CDDL does not enforce canonical serialization, context equality or cryptographic validity; those have separate checks.

Reproduce from the repository root (the Python dependencies are oracle tools only):

```sh
uv run --python 3.13 --with cbor2==5.6.5 --with cryptography==46.0.3 \
  experiments/sync-prototype/tests/vectors/record-contract/generate.py --check
CARGO_TARGET_DIR=target/sync-prototype cargo test \
  --manifest-path experiments/sync-prototype/Cargo.toml --locked \
  --features crypto-probe,record-contract --test record_contract
```

Omit `--check` only when intentionally changing the contract and reviewing the new bytes. Negative fixtures are byte splices against the data record, with an explicit expected parser error. The generator, committed bytes, CDDL and Rust tests must agree; do not regenerate expectations merely to satisfy a failing implementation.

The maximum fixture has a 1,048,592-byte ciphertext (1 MiB plaintext plus the 16-byte tag), the longer `snapshot` label, u64-max epoch, and a 48-byte predecessor. Its complete length is **1,052,451 bytes**, giving **3,859 bytes** of maximum framing/signature overhead. The maximum ciphertext's CBOR length prefix is five bytes. This independent serialization derives the Rust input cap; the test accepts that boundary and rejects the next byte. The full maximum vector is represented by length and SHA-256 to avoid committing a megabyte of zeros; its construction is in the generator.

The provider tests check the independent PRK/key/ciphertext values and reject changes to every variable header field, ciphertext, author key and signature context. Version/suite changes reject at the codec boundary. Every proper prefix of both complete golden records is rejected. Tests also cover nonminimal widths, indefinite items, forbidden types, lengths/arity, repeated context disagreement, and consistently substituted route context.

This establishes content encoding and finite test behavior on the recorded host. It does not validate device enrollment, writer admission, recovery, durable nonce reservations, cryptographic usage bounds, snapshot coverage, a relay, or independent interoperability of the approved hybrid HPKE suite. Those remain their owning issues' gates. The replica journal remains plaintext and is not wired to this adapter.
