# Key-distribution structural oracle

`generate.py` uses pinned Python cbor2 5.9.0 and standard-library SHA-384. It does not call the Rust implementation. Install the existing [oracle requirements](../record-contract/requirements.txt), then run from the prototype directory:

```sh
python tests/vectors/key-distribution/generate.py --check
```

Without `--check`, the generator writes `golden.json`. CI runs the check on every pull request and main push. Both fixture generators reject drift and wrong oracle versions even under `python -O`; `../check_oracles.py` executes those failure cases in isolated temporary directories. The active [execution manifest](../../../evidence/record-contract-2026-11-18.json) binds the generator, output, schema and consuming tests to their source revision.

Four fixed arrays represent genesis, membership, writer-start and a key envelope. They include complete signed-input bytes and complete-record SHA-384 hashes, plus independently derived root/device IDs, recipient fingerprint, HPKE info/AAD, key plaintext and the epoch content-key commitments carried in the genesis and membership bodies (epochs 0 and 1). Ten negative cases splice the writer-start bytes. Rust tests also reject every truncated prefix of all four records, check the original signed slice points into input, validate CDDL using cddl-cat, and compare both length and SHA-384 of the independently generated 16-member genesis (58,899 bytes).

All patterned keys, encapsulations and zero signatures here are **structural placeholders**, not valid cryptographic objects. The [actual cryptographic integration tests](../../key_distribution.rs) generate fresh keys using the approved provider. This oracle establishes CBOR/hash transcript agreement, not independent HPKE or signature interoperability. The [contract](../../../../../docs/SYNC_KEY_DISTRIBUTION.md) states the authority rules and limits.

The `writer_handoff` vectors cover populated and null closing heads with their own signed kind. Existing four control vectors remain byte-for-byte unchanged by this extension.
