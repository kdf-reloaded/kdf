# Release artifacts

Every signed release (see [`RELEASE.md`](RELEASE.md)) attaches the files below. All names carry the
version tag, so `<tag>` is for example `v0.4.0`.

## Binaries (`kdf`)

| File | Target triple | Runs on |
|------|---------------|---------|
| `kdf-<tag>-x86_64-unknown-linux-gnu` | `x86_64-unknown-linux-gnu` | 64-bit Intel/AMD Linux, glibc 2.31 or newer |
| `kdf-<tag>-aarch64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu` | 64-bit ARM Linux (Raspberry Pi 3/4/5 with a 64-bit OS, ARM servers), glibc 2.31 or newer |
| `kdf-<tag>-armv7-unknown-linux-gnueabihf` | `armv7-unknown-linux-gnueabihf` | 32-bit ARM Linux, ARMv7-A with hard-float (VFPv3-D16); Raspberry Pi 2 and newer with a 32-bit OS |
| `kdf-<tag>-x86_64-apple-darwin` | `x86_64-apple-darwin` | Intel Macs |
| `kdf-<tag>-aarch64-apple-darwin` | `aarch64-apple-darwin` | Apple silicon Macs |
| `kdf-<tag>-universal-apple-darwin` | both of the above | One macOS binary for Intel and Apple silicon |
| `kdf-<tag>-x86_64-pc-windows-msvc.exe` | `x86_64-pc-windows-msvc` | 64-bit Windows |

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
| `libkdf-<tag>-x86_64-linux-android.a` | `x86_64-linux-android` | Android ABI `x86_64` (emulators, Chromebooks), NDK API level 21 |
| `libkdf-<tag>-aarch64-apple-ios.a` | `aarch64-apple-ios` | iOS devices, deployment target iOS 14.0 |
| `libkdf-<tag>-aarch64-apple-ios-sim.a` | `aarch64-apple-ios-sim` | iOS simulator on Apple silicon Macs, deployment target iOS 14.0 |
| `libkdf-<tag>-x86_64-apple-ios.a` | `x86_64-apple-ios` | iOS simulator on Intel Macs, deployment target iOS 14.0 |

The libraries are the daemon as a static archive for the host application to link, not a standalone program.
Each iOS slice is a separate static library (no XCFramework); Android 32-bit x86 (`i686`) is not built.

## C header (`kdf-<tag>.h`)

`kdf-<tag>.h` declares the C interface of the libraries above, with the exact prototypes and the callback type.
It needs only `<stdint.h>`, has include guards and works from C++.

The host application calls four functions:

- `int8_t mm2_main(const char *conf, kdf_log_callback log_cb)` starts the daemon in a background thread from a
  JSON configuration string and returns at once (0 started, 1 already running, 2 no configuration, 3 the
  configuration is not UTF-8, 5 the thread could not be started);
- `int8_t mm2_main_status(void)` (0 not running, 1 starting, 2 context ready, 3 RPC up);
- `int8_t mm2_stop(void)` (0 stopped, 1 not running, 2 error while stopping, 3 already stopping);
- `int32_t mm2_test(int32_t torch, kdf_log_callback log_cb)` runs the built-in self tests.

**The log callback** is `typedef void (*kdf_log_callback)(const char *line)`. It uses the C calling convention (the
platform default), must not be NULL, receives a NUL-terminated line that is valid only during the call (copy it if you
need it later), and is called from whichever thread logs, so it can run on any thread and must be thread-safe.

The archive also exports `log_stacktrace` and `is_loopback_ip`. The header declares them in a separate block marked
"exported for internal use, not a supported API, may change without notice". The archive also contains the C symbols
of the libraries built into it, among them SQLite (`sqlite3_*`) and libusb (`libusb_*`) under their usual names.

CI checks the header on every release: it builds the Linux library, compiles a C program against the header with
strict warnings, links and runs it, and compares the KDF symbols the header declares with the ones the library
exports.

## Checksums and signature

- `SHA256SUMS` lists every file above, the C header, plus the license files `COPYING`, `LICENSE` and `THIRDPARTY-LICENSES`.
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
`SHA256SUMS` and signs it. The release fails if a Linux binary, the macOS universal binary or a mobile library is missing.

The unsigned snapshots from `dev-build.yml` (manual) and `staging-build.yml` (every push to `staging`)
use the same build workflows, so they carry the same targets and the same floors.

Building the armv7 binary yourself: the older `cross`-based route in
[`RASPBERRY_PI4_CROSS.md`](RASPBERRY_PI4_CROSS.md) is kept for reference; the supported route is the
Debian 11 container and cross toolchains used by `build-linux.yml`.
