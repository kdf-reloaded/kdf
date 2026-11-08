# Release artifacts

Every signed release (see [`RELEASE.md`](RELEASE.md)) attaches the files below. All names carry the
version tag, so `<tag>` is for example `v0.4.0`.

## Binaries (`kdf`)

| File | Target triple | Runs on |
|------|---------------|---------|
| `kdf-<tag>-x86_64-unknown-linux-gnu` | `x86_64-unknown-linux-gnu` | 64-bit Intel/AMD Linux, glibc 2.31 or newer |
| `kdf-<tag>-aarch64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu` | 64-bit ARM Linux (Raspberry Pi 3/4/5 with a 64-bit OS, ARM servers), glibc 2.31 or newer |
| `kdf-<tag>-armv7-unknown-linux-gnueabihf` | `armv7-unknown-linux-gnueabihf` | 32-bit ARM Linux, ARMv7-A with hard-float (VFPv3-D16); Raspberry Pi 2 and newer with a 32-bit OS |
| macOS and Windows binaries | see the release page | |

**glibc floor.** The three Linux binaries are built inside a pinned Debian 11 container and cross-compiled
there with the Debian 11 cross toolchains (`gcc-aarch64-linux-gnu`, `gcc-arm-linux-gnueabihf`), so each
needs glibc 2.31 or newer at run time. That covers Debian 11 and 12, Ubuntu 20.04 and newer, and RHEL/Rocky 9.
The build checks this: it fails if a binary asks for a newer glibc symbol version than 2.31.

**Not covered.** ARMv6 devices (Raspberry Pi 1 and Zero) and soft-float ARM Linux are not supported by
the armv7 binary. musl-based systems (Alpine) are not covered either.

## Static libraries (`libkdf.a`) for mobile apps

| File | Target triple | Notes |
|------|---------------|-------|
| `libkdf-<tag>-aarch64-linux-android.a` | `aarch64-linux-android` | Android ABI `arm64-v8a`, NDK API level 21 |
| `libkdf-<tag>-armv7-linux-androideabi.a` | `armv7-linux-androideabi` | Android ABI `armeabi-v7a`, NDK API level 21 |
| `libkdf-<tag>-aarch64-apple-ios.a` | `aarch64-apple-ios` | iOS devices, deployment target iOS 14.0 |

The libraries are the daemon as a static archive for the host application to link, not a standalone program.
Simulator targets (`aarch64-apple-ios-sim`, `x86_64-apple-ios`) and Android `x86_64` are not built.

The library exports four C functions: `mm2_main` (start the daemon in a background thread from a JSON
configuration string and a log callback), `mm2_main_status` (0 not running, 1 starting, 2 context ready,
3 RPC up), `mm2_stop` and `mm2_test`. No C header is shipped yet.

## Checksums and signature

- `SHA256SUMS` lists every file above plus the license files `COPYING`, `LICENSE` and `THIRDPARTY-LICENSES`.
- `SHA256SUMS.asc` is the detached GPG signature of `SHA256SUMS`, made with the maintainer key
  `FEE1ACA52C65FF3EBF31818CB5595E1752BC2A82` (public key: [`keys/takologi.asc`](keys/takologi.asc)).

```sh
gpg --import docs/keys/takologi.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS
```

## How they are built

[`.github/workflows/release.yml`](../.github/workflows/release.yml) runs [`build-linux.yml`](../.github/workflows/build-linux.yml)
(all three Linux targets), [`build-android.yml`](../.github/workflows/build-android.yml),
[`build-ios.yml`](../.github/workflows/build-ios.yml), [`build-macos.yml`](../.github/workflows/build-macos.yml)
and [`build-windows.yml`](../.github/workflows/build-windows.yml), then collects the results, writes
`SHA256SUMS` and signs it. The release fails if a Linux binary or a mobile library is missing.

The unsigned snapshots from `dev-build.yml` (manual) and `staging-build.yml` (every push to `staging`)
use the same build workflows, so they carry the same targets and the same floors.

Building the armv7 binary yourself: the older `cross`-based route in
[`RASPBERRY_PI4_CROSS.md`](RASPBERRY_PI4_CROSS.md) is kept for reference; the supported route is the
Debian 11 container and cross toolchains used by `build-linux.yml`.
