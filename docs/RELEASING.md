# Releasing Matters

A release is a tag. Pushing a tag `v*` runs `.github/workflows/release.yml`, which builds every release file on GitHub's hosted runners, attests where each was built, and publishes them. Nothing is built on a maintainer's machine, and the workflow needs no secret: it signs through GitHub's OIDC identity.

## What a release holds

| File | What it is |
| --- | --- |
| `matters-<tag>-x86_64.flatpak`, `matters-<tag>-aarch64.flatpak` | The Flatpak as a single-file bundle, one per architecture. Installing one fetches the KDE runtime from Flathub. |
| `SHA256SUMS` | The SHA-256 of each bundle. |
| `ghcr.io/thestephenromano/matters-relay:<tag>` | The relay's container image (`packaging/relay/Containerfile`) for linux/amd64 and linux/arm64. Its digest is in the release notes. |

Each bundle, `SHA256SUMS` and the image carry a [SLSA build provenance](https://slsa.dev/spec/v1.0/provenance) attestation: a statement, signed with a short-lived Sigstore certificate issued to the release workflow, of the repository, workflow, commit and run that built the file. The attestations are kept by GitHub, and the image's is also pushed beside it to ghcr.io.

## How the workflow builds

- **Bundles.** Each architecture builds natively on its own runner (`ubuntu-24.04`, `ubuntu-24.04-arm`) with `ci/flatpak.sh`, the script CI's Flatpak job runs. The bundle is the build that script then installs, starts in the sandbox, and updates; the job fails, and nothing is released, unless all of that passes.
- **Image.** Each platform builds natively from the Containerfile, whose base images are pinned by digest, with BuildKit pinned by digest too. The two images are pushed, then a multi-platform index is pushed under the tag.
- **Separation.** The jobs that compile hold a read-only token and no OIDC token, so no build script can sign or publish. Attestation and publishing run in later jobs that run no project code, with only the permissions each needs: `id-token` and `attestations` to attest, `packages` to push the image, `contents: write` to create the release.
- **Pinning.** Every action is pinned by commit SHA, as in CI.

## Cutting a release

1. Merge everything for the release into `main`, and check that CI passed on that commit.
2. Optionally, run the workflow on `main` without publishing: `gh workflow run release.yml --ref main`. It builds the bundles and the image and attests the bundles; the run's `release-files` artifact holds them. It creates no release and pushes no image.
3. Tag the commit and push the tag:

   ```sh
   git tag -s v0.1.0 -m "Matters 0.1.0"
   git push origin v0.1.0
   ```

   A tag with a hyphen (`v0.2.0-rc.1`) is published as a prerelease.
4. The workflow publishes the image, then the GitHub Release with the bundles, `SHA256SUMS`, the image digest and these checks.

A pull request that changes the workflow runs its builds and checksums, but attests and publishes nothing.

## Verifying a release

Download the bundle for your architecture and `SHA256SUMS` from the release, then:

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify matters-v0.1.0-x86_64.flatpak --repo thestephenromano/matters
```

The first checks the file against the published checksum. The second checks that the file's digest is attested by a workflow in this repository, signed by a Sigstore certificate GitHub issued to it. To also require that it came from the release workflow and the tag you expect:

```sh
gh attestation verify matters-v0.1.0-x86_64.flatpak --repo thestephenromano/matters \
  --signer-workflow thestephenromano/matters/.github/workflows/release.yml \
  --source-ref refs/tags/v0.1.0
```

Then install it:

```sh
flatpak install --user matters-v0.1.0-x86_64.flatpak
```

A bundle has no update channel: to update, download the next release's bundle, verify it the same way, and install it.

For the relay image, verify the digest from the release notes and run that digest, never the tag alone:

```sh
gh attestation verify oci://ghcr.io/thestephenromano/matters-relay@sha256:<digest> \
  --repo thestephenromano/matters
docker buildx imagetools inspect ghcr.io/thestephenromano/matters-relay:v0.1.0   # its digest must match
```

`--bundle-from-oci` reads the attestation from ghcr.io instead of GitHub's API. [SELF_HOSTING.md](SELF_HOSTING.md) shows how to run the image by digest.

## What this establishes, and what it does not

Provenance binds each file to the commit and workflow run that built it on GitHub's hosted runners. It does not show that the source was benign, and it rests on the security of the repository: who can push a tag, and who can change the workflow. The build is not yet checked to be reproducible: no gate builds a release twice and compares.

The following need a key held by the maintainer, and are maintainer steps:
- **GPG-signed Flatpak.** A Flatpak repository with GPG-signed OSTree commits and summary (`flatpak build-sign`, `flatpak build-update-repo --gpg-sign`), and bundles signed with `flatpak build-bundle --gpg-sign`, would let Flatpak itself check every install and update. The key belongs to the maintainer, kept offline or in a protected environment, never generated by CI. Until then, bundles are checked by hand as above.
- **Flathub.** Publishing on Flathub gives an update channel that Flathub builds from the manifest and signs with its own key. It takes a submission to Flathub and verification of the `io.github.thestephenromano` app ID.
- **Signed tags.** The tag the workflow builds from should be signed with the maintainer's key (`git tag -s`).

And settings only the maintainer can change:
- After the first release, make the `matters-relay` package on ghcr.io public: GitHub creates it private.
- Restrict who can create `v*` tags with a repository ruleset, since a tag publishes.
