# Sync dependency research evidence

The [2026-11-18 manifest](sync-dependencies-2026-11-18.json) records the packages inspected for [ADR 0001](../decisions/0001-crdt-adoption.md) and [ADR 0002](../decisions/0002-post-quantum-sync.md). It is a source-inspection record, not a lockfile, SBOM, benchmark, security audit or target-compatibility result.

For each crate, the downloaded `.crate` archive's SHA-256 matched the checksum returned by the version-specific crates.io API. The recorded source revision comes from the packaged `.cargo_vcs_info.json`. Selected manifest/source files were inspected and hashed. Archives and extracted sources were temporary research inputs and are not vendored into Matters.

To reproduce an individual entry:

1. Fetch its exact `api_metadata` and `source_archive` URLs. Compare the archive SHA-256 with both the saved manifest and API response.
2. List the archive before extracting into a disposable directory. Extract only regular files under the expected `<name>-<version>/` prefix; reject traversal paths and links.
3. Read `.cargo_vcs_info.json`, packaged `Cargo.toml`, `Cargo.toml.orig` and the listed `inspected_files`. Compare file hashes. Workspace-inherited values in `Cargo.toml.orig` are resolved in packaged `Cargo.toml`.
4. Follow the ADR's source/API references and verify the stated algorithms, feature gates and API limitations in these exact sources. Resolve any difference explicitly rather than substituting `latest`.
5. Remove the disposable downloads/extraction directory when finished. Before actual adoption, resolve a Cargo lockfile, inspect the complete graph and licenses, build the required targets, and produce interoperability/correctness measurements separately.

Known limits: the Automerge package reports a Git revision that returned 404 in a GitHub contents lookup; the checksummed package remains the inspected artifact. SQLite-Sync evidence is release metadata and reported license/assets, not a downloaded/built binary. Direct crypto pins do not establish the versions or assurance of every transitive implementation. Publication dates indicate releases, not security-review completion.
