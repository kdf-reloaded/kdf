# Security Policy

## Supported versions

KDF Reloaded is in public alpha. Only the latest tagged alpha release receives security fixes.

| Version          | Supported |
|------------------|-----------|
| `0.1.0-alpha.x`  | ✅        |
| Anything older   | ❌        |

## Reporting a vulnerability

If you believe you have found a security vulnerability in KDF Reloaded — particularly anything affecting swap atomicity, key handling, networking, or RPC authorisation — **please do not open a public issue**.

> Contact maintainers via the channels listed in [`CONTRIBUTING.md`](CONTRIBUTING.md) and explicitly mark the message as a security report. Sensitive reports may be encrypted to the maintainer PGP key (see [Maintainer signing key](#maintainer-signing-key) below). A dedicated vulnerability mailbox may be added in a future release cycle.

When reporting, please include:

- A clear description of the issue and its impact.
- Steps to reproduce, ideally with a minimal test case.
- Affected commit hash or release tag.
- Any proposed mitigation.

We aim to acknowledge reports within 5 business days and to provide a remediation timeline within 14 days.

## Disclosure policy

We follow coordinated disclosure. Once a fix is available we will:

1. Publish a patched release.
2. Issue a security advisory in this repository.
3. Credit the reporter (unless they request otherwise).

## Maintainer signing key

From June 2026 onward, Git commits and release tags are GPG-signed by the project maintainer. The public key is committed to this repository at [`docs/keys/takologi.asc`](docs/keys/takologi.asc).

- **Identity:** `Takologi <takologi@proton.me>`
- **Key type:** RSA 4096
- **Fingerprint:** `FEE1 ACA5 2C65 FF3E BF31  818C B559 5E17 52BC 2A82`

Import the key and verify provenance:

```sh
# Import from the in-repo copy...
gpg --import docs/keys/takologi.asc
# ...or from a keyserver:
gpg --recv-keys FEE1ACA52C65FF3EBF31818CB5595E1752BC2A82

# Verify the current commit and a release tag:
git verify-commit HEAD
git verify-tag <tag>
```

A *Good signature* from fingerprint `FEE1ACA52C65FF3EBF31818CB5595E1752BC2A82` confirms maintainer provenance. Treat any release commit or tag that is **not** signed by this key as unverified.

## Release artifact verification

> GPG and/or minisign signatures on release **binaries** are planned for the alpha cycle and will be produced with the [maintainer signing key](#maintainer-signing-key) above; a per-artifact verification procedure will be published alongside the first signed release.
>
> Until then, the authoritative provenance signal is the maintainer's commit/tag signature (above), and the only authoritative source of KDF Reloaded code is this repository. Do not trust binaries received through any other channel.

DEX fee receiver addresses are inherited from the upstream Komodo DeFi Framework configuration by design and are not under the control of the KDF Reloaded maintainers.

## Pre-release gating

Release readiness is controlled by the repository checklist and release-governance process.
See [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md) for the full pre-release gating list.

## Out of scope

- Issues affecting the upstream Komodo DeFi Framework that are not present in KDF Reloaded — please report those upstream.
- Vulnerabilities in third-party coin protocols themselves (rather than this software's handling of them).
- Denial-of-service from a malicious peer that can already drop your traffic at the network layer.

## Hardening notes for operators

- Run `kdf` as a non-root user. Restrict access to the RPC port (`7783` by default).
- Use a strong, unique `rpc_password`.
- Treat `MM2.json` as secret material — it contains your mnemonic.
- Take regular backups of your seed phrase via a secure offline channel.
