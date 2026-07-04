# Cutting a Release

This is the operational runbook for producing an official, signed KDF Reloaded
release. It complements [`RELEASE_CHECKLIST.md`](../RELEASE_CHECKLIST.md) (the
gate that must be fully ticked) — this file describes the *mechanics*.

## Overview

Releases are **tag-driven**. Pushing an annotated, GPG-signed `v*` tag triggers
[`.github/workflows/release.yml`](../.github/workflows/release.yml), which:

1. builds the Linux x86-64 binary inside a pinned **Debian 11** container
   (glibc 2.31 floor → runs on Debian 11/12, Ubuntu 20.04+, RHEL 9, …);
2. builds macOS (x86_64 / aarch64 / universal) and Windows binaries via the
   existing per-platform workflows;
3. writes a `SHA256SUMS` manifest over all binaries;
4. **GPG-signs** `SHA256SUMS` with the maintainer key
   (`FEE1ACA52C65FF3EBF31818CB5595E1752BC2A82`) inside the protected `release`
   environment, producing `SHA256SUMS.asc`;
5. drafts a **GitHub Release** with every binary + `SHA256SUMS` + `SHA256SUMS.asc`
   attached.

The `gate` job in `release.yml` decides how to treat the tag (GitHub tag
triggers cannot be branch-scoped):

- **Final release** — tag `vX.Y.Z` (no pre-release suffix) whose commit is on
  **`main`** → published as the signed, **latest** GitHub Release.
- **Pre-release** — tag `vX.Y.Z-alpha.N` / `-beta.N` / `-rc.N` whose commit is
  on **`staging`** (or `main`) → published as a signed GitHub **pre-release**
  (not marked "Latest"). The optional DockerHub `:latest` push is skipped for
  pre-releases.
- A `v*` tag whose commit is on none of those (e.g. a stray dev tag) self-skips.

Signing happens **only** in `release.yml`. Unsigned, untagged branch snapshots
are produced separately by [`dev-build.yml`](../.github/workflows/dev-build.yml)
(manual) and [`staging-build.yml`](../.github/workflows/staging-build.yml)
(automatic on push to `staging`); both reuse the same Debian 11
[`build-linux.yml`](../.github/workflows/build-linux.yml), so every artifact
carries the same glibc 2.31 floor.

## Steps for the maintainer

### Pre-release (beta / rc) from `staging`

1. Promote `dev → staging` and bump the version to the pre-release (e.g.
   `0.1.0-beta.1`); update `CHANGELOG.md`. Commit and push `staging`
   (this also fires the unsigned `staging-build.yml` snapshot).
2. Tag from `staging` and push the tag:
   ```sh
   git checkout staging && git pull
   git tag -s v0.1.0-beta.1 -m "v0.1.0-beta.1"
   git push origin v0.1.0-beta.1
   ```
3. Watch **Release**; it drafts a signed **pre-release**. Review the notes and
   assets, then **publish** it in the GitHub UI (it will be marked "Pre-release",
   not "Latest").

### Final release from `main`

1. Complete every box in [`RELEASE_CHECKLIST.md`](../RELEASE_CHECKLIST.md).
2. Promote `staging → main`, set the final version (e.g. `0.1.0`), and update
   `CHANGELOG.md`. Push `main`.
3. Create and push an annotated, GPG-signed tag from `main`:
   ```sh
   git checkout main && git pull
   git tag -s v0.1.0 -m "v0.1.0"
   git push origin v0.1.0
   ```
4. Watch the **Release** workflow. When it finishes, a **draft** (latest)
   release exists.
5. Review the drafted notes and the attached assets, then **publish** the
   release in the GitHub UI.

## CI configuration

- **`release` environment** holds `GPG_PRIVATE_KEY` (the maintainer signing
  key, no passphrase). It is the only place the key is exposed; restrict the
  environment to tag refs (`v*`). See the "GPG key → CI secret" note below.
- **Optional DockerHub image**: set repository variable `PUBLISH_DOCKERHUB=true`
  and provide `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` secrets and a
  `DOCKERHUB_REPO` variable; the `docker` job then builds
  [`Dockerfile.release`](../Dockerfile.release) (Ubuntu 24.04 runtime) from the
  Linux binary and pushes `:<tag>` and `:latest`.

### Rotating / installing the signing key

Do this on a trusted workstation — never paste the private key anywhere but the
GitHub Secrets store:

```sh
# Export a dedicated signing subkey (preferred over the primary key):
gpg --export-secret-subkeys --armor <FPR> | base64 -w0 > kdf-sign.b64
gh secret set GPG_PRIVATE_KEY --env release < kdf-sign.b64
shred -u kdf-sign.b64
```

The import step in `release.yml` accepts the secret either base64-encoded or as
a raw armored key block.

## Verifying a release (for users)

```sh
# Import the maintainer public key (shipped in-repo):
gpg --import docs/keys/takologi.asc

# Verify the signed checksum manifest, then the binary:
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS
```

A good signature from `FEE1ACA52C65FF3EBF31818CB5595E1752BC2A82` plus a matching
checksum authenticates the download.
