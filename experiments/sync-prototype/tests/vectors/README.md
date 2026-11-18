# HPKE probe fixture provenance

`mlkem768-p256-hkdfsha256-aes128gcm.json` contains the receiver derivation and first encryption from the Base-mode `(KEM=80, KDF=1, AEAD=1)` vector in the pinned provider's [PQ HPKE vector file](https://github.com/celabshq/libcrux/blob/42d68bd49b244b09bd02626c6ffa95e54dc64f91/crates/protocols/hpke/tests/test_vectors_hpke_pq.json). This corresponds to [draft-ietf-hpke-pq-05, Appendix A.4](https://datatracker.ietf.org/doc/html/draft-ietf-hpke-pq-05#appendix-A.4).

- Original vector file SHA-256: `35c59f4a0132e5631e50ac039d8ca3a72e99f5e92dfd94d45338d6ae243f613c`.
- Extracted fixture SHA-256: `eec1db0669500730a9aaa697b0cbb3d9477bd996e5f788ec57cbade84fa5b8fb`.
- Extracted fields: mode, algorithm IDs, `ikmR`, `pkRm`, `enc`, `info`, and the first `encryptions` element. Formatting is JSON with two-space indentation and a final newline.

These are public test values, not application secrets. The test checks receiver key derivation against the published public key and decryption against the published plaintext. It does not test the sender's deterministic encapsulation or later sequence numbers. This vector uses **AES-128-GCM**, whereas the selected application candidate uses **AES-256-GCM**; only a same-provider round trip is currently supplied for the exact selected suite. Independent interoperability for that complete combination remains outstanding. The package/source license is MPL-2.0; retain this provenance with the fixture.
