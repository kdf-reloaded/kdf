# Chapter 26 — Cross-Platform Build Substrate and WebAssembly Adaptation

**Status:** driving-spec.

> **One-sentence claim:** the project shall target a nine-row
> build-target matrix out of a single source tree without
> forking the source code, via a two-macro plus point-of-use
> attribute platform-guard discipline, a two-crate platform-
> shim split, per-crate target-table dependency-graph
> separation, dual storage and asynchronous-runtime and
> transport substrates, and a filesystem-and-operating-system
> isolation substrate.

The chapter binds the substrate by which the workspace targets
the nine-row build-target matrix out of a single
source tree without forking the source code: a two-macro plus point-of-use attribute platform-guard discipline,
a two-crate platform-shim split, a target-table per-crate dependency-graph separation, a
storage-backend duality substrate, a dual
asynchronous-runtime substrate, a dual transport
substrate, a filesystem-and-operating-system
isolation substrate, a native-only-stack
enumeration, a WebAssembly-only-stack identifier,
and a continuous-integration matrix.

## 26.1 Executive Summary

The substrate occupies the structural seam between
the shared workspace source tree and a
nine-row build-target matrix:

| Bound family | Bound target triple | Bound build artefact |
| -------------- | -------------------------------------------------- | ----------------------------------------------- |
| Linux desktop | `x86_64-unknown-linux-gnu` | A native binary. |
| macOS desktop | `x86_64-apple-darwin` | A native binary. |
| macOS desktop | `aarch64-apple-darwin` | A native binary. |
| macOS desktop | Universal binary merged via the Apple `lipo` tool. | A merged native binary. |
| Windows desktop | `x86_64-pc-windows-msvc` | A native executable. |
| iOS mobile | `aarch64-apple-ios` | A static library. |
| Android mobile | `aarch64-linux-android` | A shared library. |
| Android mobile | `armv7-linux-androideabi` | A shared library. |
| Browser | `wasm32-unknown-unknown` | A `wasm-pack` package. |

The chapter-02-anchored baseline tree carried a partial form of the macro substrate (R2) and the target-table substrate (R5), a ancestor of the
dual-executor substrate (R10–R12), and a early
IndexedDB layer (R8–R9). The substrate at landing extends the
pattern so that every asynchronous
transport, every persistence component, and every
executor entry point exposes the dual shape; adding a target therefore becomes a
single-cfg change at the boundary,
not a fork.

The build-tooling changes themselves (the
toolchain pin, the workspace-member registry
of chapter 02 R4, the continuous-integration matrix
expansion) are bound in chapters 02 and 03. This chapter binds
what the source code does in response.

Bound rules R1–R5 cover the compilation-guard
substrate; R6–R7 cover the two-crate platform-
shim split; R8–R9 cover the storage-backend
duality; R10–R12 cover the dual asynchronous-
runtime substrate; R13–R14 cover the dual
transport substrate; R15 covers the filesystem-
and-operating-system isolation substrate; R16–R17 cover the
native-only-stack enumeration and the
WebAssembly-only-stack identifier; R18 covers the
continuous-integration matrix.

## 26.2 Subsystem Shape

The substrate occupies a horizontal seam across
every workspace member of chapter 02 R4. Three
seam classes are bound:

| Bound seam class | Bound substrate locus |
| --------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| In-source compilation guards (R1–R3) | Per source file, at module or item granularity. |
| Per-crate dependency separation (R4–R5) | Per crate manifest. |
| Per-subsystem dual-implementation behind common contracts (R8–R14) | Per persistence consumer, executor entry point, and transport entry point. |

The substrate does *not* modify the configuration
surface of chapter 02 R6, the request-and-response
surface of chapter 02 R7, the license posture of
chapter 02 R9, or the build-target surface of
chapter 02 R8.

## 26.3 Bound Compilation-Guard Substrate

**R1.** The compilation-guard substrate consists
of three permitted forms; consumers MUST select
exactly one per site:

| Bound form | Bound use case |
| ---------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| The declarative macros `cfg_native!` and `cfg_wasm32!` of R2. | A group of three to thirty `use` statements, an `impl` block, or a multi-item top-of-file branching region. |
| The point-of-use attribute pair `#[cfg(not(target_arch = "wasm32"))]` and `#[cfg(target_arch = "wasm32")]` of R3. | A single item: one function, one match arm, one enum variant, one struct field, one `impl`, or one whole-module declaration. |
| The per-crate manifest target-table pattern `[target.'cfg(target_arch = "wasm32")']` and its non-WebAssembly negation, of R4–R5. | A dependency whose transitive graph does not exist on the other side of the platform fence. |

**R2.** The declarative-macro substrate MUST expose
exactly two macros, both crate-exported from the
shared-utility crate `common`:

| Bound macro | Bound expansion |
| --------------- | ---------------------------------------------------------------------------------------------------------------- |
| `cfg_native!` | The declarative substrate routed through the sibling-allowlist crate `cfg_if` under the `not(target_arch = "wasm32")` arm. |
| `cfg_wasm32!` | The declarative substrate routed through the sibling-allowlist crate `cfg_if` under the `target_arch = "wasm32"` arm. |

Both macros are ergonomic
substrates only: a block of three-to-thirty
import statements (or a helper
function or a impl block) wrapped in `cfg_native!
{ ... }` is more readable than peppering every
line with the point-of-use attribute of R3.

**R3.** The point-of-use attribute pair MUST be
the canonical form for finer-than-
multi-item granularity: single-item, single-function, single-match-arm, single-enum-variant, single-struct-field, 
single-impl, and whole-module declarations.

The convention is bound as: macros
for multi-item groups and top-of-
file branching; attributes for everything else.

## 26.4 Bound Per-Crate Dependency Separation

**R4.** Where a dependency makes no sense on the
other side of the platform fence,
the gate MUST move out of the source
into the per-crate manifest via the Cargo target-table substrate `[target.'cfg(...)'.dependencies]`.

**R5.** A canonical worked instance is the
hardware-wallet crate `trezor`: the binding-crate consumed by the non-WebAssembly
build (which transitively pulls in a C library
that does not cross-compile to the browser target
triple) is bound under the non-WebAssembly target
table; the browser-interoperability crates
`js-sys` and `wasm-bindgen` are bound under the WebAssembly target table. Consumers that follow the
pattern include the chapter-25-bound storage crate
`db_common`, the browser-wallet integration crate
`mm2_metamask`, the chapter-22-bound WalletConnect substrate
`kdf_walletconnect`, and several others.

## 26.5 Bound Two-Crate Platform-Shim Split

**R6.** The substrate MUST carry exactly two binary-shaped crates, each owning a subset of the
nine-row build-target matrix:

| Bound shim crate | Bound covered targets | Bound build-artefact-emitting workflow |
| -------------------- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| The application-entry crate `mm2_main` | The desktop targets (Linux, macOS single-arch, macOS Universal merged via the Apple `lipo` tool, Windows) and the browser target. | A `[[bin]]` target named `mm2` for desktop; a `[lib]` target with the `cdylib`/`staticlib`/`lib` crate-type tuple consumed by the `wasm-pack` browser invocation. |
| The mobile-bindings crate `mm2_bin_lib` | The mobile targets (iOS, Android double-ABI). | A `[[bin]]` target named `mm2_reloaded` plus a `[lib]` target with the `cdylib`/`rlib` crate-type pair. The iOS workflow runs the Cargo build against the iOS target triple and uploads the static-library artefact. The Android workflow runs the `cargo ndk` per Android target triple and uploads the shared-library artefact. |

**R7.** The rationale for the two-
crate split is threefold:

- The desktop binary is invoked by the
  Cargo run command and by packaging steps
  (the Docker image, the Homebrew
  formula, the Windows installer); a clean binary-target shape is the easiest way to
  keep that workflow unchanged.
- Mobile builds need a library artefact for the
  mobile-host application (Swift / Kotlin) to
  link, not an executable. A separate crate keeps
  the mobile dependency surface (historically the
  most fragile substrate to cross-compile)
  isolated from desktop dependency upgrades.
- The browser build is consumed via the
  `wasm-pack` substrate, which itself runs the
  Cargo build against the browser target
  triple with the appropriate crate-type and
  post-processes the result. The `mm2_main`
  library entry exposes everything the JS shim
  needs, so a separate shim crate is unnecessary
  on the browser target.

The mobile crate's binary
`mm2_reloaded` MUST be selected via a `default-run` manifest entry so that it does not collide with
the desktop binary `mm2` built out of `mm2_main`.
The mobile-crate library module MUST re-export
the public application-entry surface
(`lp_main`, `mm2_status`, `MainStatus`) from `mm2_main` plus
the non-WebAssembly-gated re-export of the
`mm2_main`-side application-entry accessor and
the run-entry accessor. This split
is the only place in the workspace where
target-specific binary surfaces exist; everything
below `mm2_main` is library-shaped.

## 26.6 Bound Storage-Backend Duality

**R8.** The pattern bound by this substrate for
every persistence consumer in the workspace MUST
be the four-step shape:

| Bound step | Bound contract |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1 | Define a behaviour-only trait in the consumer crate. The trait MUST be asynchronous, MUST be generic over the result types, and MUST NOT carry SQL or IndexedDB types in its signature. |
| 2 | Provide a native implementation backed by the asynchronous connection facade of chapter 25 R11 in a module named `sqlite_storage.rs` or under a `sqlite/` sub-module. |
| 3 | Provide a WebAssembly implementation backed by the IndexedDB substrate of R9 in a module named `wasm_storage.rs`. |
| 4 | Branch in the consumer-crate's module-roots module on the WebAssembly target predicate so that exactly one of the two implementations compiles per target. |

A canonical worked instance is the chapter-05-
bound hierarchical-deterministic-wallet storage substrate.
Chapter-bound consumers of the pattern include
the chapter-24-bound graphical-user-interface account-state
substrate, the chapter-12 / chapter-14 / chapter-15-bound swap
state stores, the chapter-19-bound non-fungible-token table
substrate, and the chapter-22-bound WalletConnect session
store.

**R9.** The WebAssembly persistence crate `mm2_db`
MUST be entirely browser-oriented: its 
module substrate consists of a IndexedDB
driver module, a lock module, a driver-submodule directory, a cursor module, and
a IndexedDB public-accessor module. Its 
manifest dependency-table substrate MUST consist almost
entirely of the WebAssembly target block (the
browser-interface crate `web-sys`, the
browser-interoperability crate `js-sys`, the
asynchronous browser-future-adaptor crate `wasm-bindgen-
futures`, et cetera). On the native target the
crate compiles to nothing of substance and is
brought in only so downstream crates need not
carry their own conditional-compilation-guarded
import.

## 26.7 Bound Dual Asynchronous-Runtime Substrate

**R10.** The asynchronous-runtime substrate
exposes two parallel modules under the
shared-utility crate's `executor` sub-
module, selected by point-of-use attribute (R3)
in the module-roots module:

| Bound module | Bound primary accessor contract |
| --------------------- | -------------------------------------------------------------------------------------------------------- |
| `native_executor` | Forwards to a work-stealing native asynchronous runtime; the spawn accessor requires the `Send` bound on the spawned future. |
| `wasm_executor` | Forwards to the sibling-allowlist asynchronous browser-future-adaptor accessor `wasm_bindgen_futures::spawn_local`; the spawn accessor drops the `Send` bound. |

The `Send`-bound problem is the central reason why browser-target asynchronous
code differs from native-target asynchronous
code: native asynchronous tasks spawned on a
work-stealing executor MUST be `Send`-bounded, but the browser event loop is
single-threaded and the browser-
future-adaptor accessor does not require it. Forcing the
`Send` bound on every future just
to satisfy the native path would prevent the
browser path from using any non-`Send` future from the browser-
interoperability or external-blockchain-client
ecosystems.

**R11.** The browser-side executor accessor set
MUST expose:

- a `spawn` accessor accepting a `'static`-lifetime non-`Send` future;
- a `spawn_local` accessor accepting the same;
- a `spawn_local_abortable` accessor returning a
  `AbortOnDropHandle` newtype around the
  sibling-allowlist asynchronous-abort-handle.

The `AbortOnDropHandle` newtype MUST implement a
drop substrate that calls the abort-handle's abort accessor; it exists so
fire-and-forget spawn patterns clean up their
continuations when the owning
struct is dropped. The spawn-after accessor MUST
bridge the browser timer accessors `setTimeout`
and `clearTimeout` through the browser-
interoperability crate.

**R12.** The wall-clock accessor `now_ms()` MUST
expose a dual implementation: on the
native target the substrate re-exports the
sibling-allowlist accessor `gstuff::now_ms()`
(routed through the POSIX accessor
`gettimeofday`); on the browser target the
substrate routes through the browser-interface
accessor `js_sys::Date::now()`. The seed for the
small-RNG substrate consumed in non-cryptographic contexts MUST likewise consume `now_ms()` on
both targets.

## 26.8 Bound Dual Transport Substrate

**R13.** The network-layer crate `mm2_net` MUST
expose a dual transport substrate organised as:

| Bound module | Bound contract |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `native_http` | A HTTP client routed through the sibling-allowlist asynchronous-HTTP-client crate over a TLS adaptor. |
| `wasm_http` | A HTTP client routed through the browser fetch API. |
| `wasm_ws` | A WebSocket client routed through the browser WebSocket interface via the browser-interface crate. |
| `transport` | A unified slurp accessor surface (`slurp_url`, `slurp_url_with_headers`, `slurp_post_json`) re-exported with identical signatures on both targets; the body picks the right module behind a point-of-use attribute (R3). |
| `grpc_web` | A gRPC-WEB client that consumes both R2 macros at the top of the file and a single shared decode/encode body below. |

Consumers above the transport layer write one
code path. The native WebSocket
client MUST be routed through a sibling-allowlist
asynchronous WebSocket crate. The chapter-22-bound WalletConnect
substrate consumes this transport layer, so its
relay code is also target-agnostic.

**R14.** The network-layer crate's module substrate MUST stand on the storage-
backend duality of R8: persistence-bearing
network features (chapter-bound transport-history storage,
transport-bound session state) MUST route through
the trait substrate of R8 step 1
rather than direct database access.

## 26.9 Bound Browser-Local RPC Channel Substrate

The browser-hosted variant of the framework cannot
expose its RPC dispatcher over a sibling-network
listener, because the browser sandbox forbids in-page
listeners. The substrate MUST instead supply a
single in-process request-response channel that
crosses the JavaScript-boundary in one direction
and is dispatched by the asynchronous-runtime
substrate of R10 in the other. This channel is the
sole means by which a browser host invokes RPC
methods on the embedded framework.

**R15.** The RPC-types crate `mm2_rpc` MUST expose,
under a `target_arch = "wasm32"` point-of-use
attribute pair, a `wasm_rpc` module containing
exactly the following surface:

| Bound public item | Bound contract |
| --- | --- |
| `WasmRpcResponse` type alias | `Result<serde_json::Value, String>`. The error arm carries a human-readable message; the success arm carries the dispatcher's JSON reply verbatim. |
| `WasmRpcRequest` type alias | A pair `(serde_json::Value, oneshot::Sender<WasmRpcResponse>)` from the futures crate. The first element is the incoming request body; the second is the per-request reply channel. |
| `channel()` free function | Returns a `(WasmRpcSender, WasmRpcReceiver)` pair backed by a futures `mpsc` channel of bounded capacity (R15-A). The sender is wrapped in a `futures::lock::Mutex` so it is callable from concurrent JavaScript-boundary entrants without a `&mut self`. |
| `WasmRpcSender` struct | Public, opaque field set, single field: an async mutex over the `mpsc::Sender<WasmRpcRequest>`. Carries one method, `pub async fn request(&self, request_json: serde_json::Value) -> WasmRpcResponse`, whose contract is R15-B. |
| `WasmRpcReceiver` struct | Public, opaque field set, single field: the `mpsc::Receiver<WasmRpcRequest>`. Implements `futures::Stream<Item = WasmRpcRequest>` by delegation; the dispatcher loop of the application crate consumes it. |

**R15-A.** The `mpsc` channel capacity MUST be a
single named module-level constant. The chosen
value MUST balance two pressures: large enough that
ordinary front-end traffic does not back-pressure
the JavaScript caller, and bounded so a runaway
caller cannot exhaust browser memory. The baseline
value is one thousand and twenty-four.

**R15-B.** `WasmRpcSender::request` MUST execute
the following sequence in order:

1. Construct a oneshot reply channel.
2. Lock the inner async mutex over the
   `mpsc::Sender`.
3. `try_send` the pair `(request_json, oneshot_tx)`
   over the mpsc channel; on send error, return the
   error arm carrying the formatted send error.
4. Drop the mutex guard before awaiting the reply
   so a second concurrent caller may proceed.
5. Await the oneshot receiver. On receive success,
   return the inner `WasmRpcResponse` verbatim. On
   receiver-cancelled error, return the error arm
   carrying a formatted cancellation message.

The method MUST NOT panic on either send or receive
failure; both are reported through the error arm of
the return type.

**R15-C.** `WasmRpcReceiver` MUST implement
`futures::Stream<Item = WasmRpcRequest>` by
forwarding `poll_next` to the wrapped
`mpsc::Receiver`. No buffering, filtering, or
re-ordering is permitted; the dispatcher loop sees
requests in arrival order.

**R15-D.** The `wasm_rpc` module MUST be the only
crate-level entry point for the browser-local RPC
channel; the application-binary crate's WASM entry
point (chapter 27 R-bound `mm2_wasm_lib`) MUST
acquire the `WasmRpcSender` from the central
context and call `request` on it; the framework's
RPC dispatcher (chapter-bound RPC top-level loop)
MUST consume the matching `WasmRpcReceiver` as a
stream and reply on the per-request oneshot.

## 26.10 Bound Filesystem-and-Operating-System Isolation

**R16.** The browser target has no filesystem. The substrate MUST collect all
native-only filesystem code into the native-
filesystem crate `mm2_io`. That crate's manifest
target-table substrate MUST be such that its real
dependencies (the `gstuff` filesystem helpers and
the asynchronous-runtime filesystem accessor) appear
only in the non-WebAssembly target block; on the
browser target the crate compiles to
an empty shell. Code that needs to *store a thing* therefore MUST
NOT call the native-filesystem crate on the
browser target; it MUST call the relevant storage trait of R8, whose browser-target
implementation persists the thing to the
IndexedDB substrate of R9 instead.

## 26.11 Bound Native-Only Stack Enumeration

**R17.** The substrate MUST classify the following
stacks as entirely or essentially
native-only because the foreign-function-interface
chains they pull in do not cross-compile to the browser target triple, or the protocol the
stack implements has no browser
equivalent:

| Bound native-only stack | Bound gating substrate |
| -------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| The Lightning Network coin module (under the coins crate's `lightning` sub-module). | Gated by `cfg_native!` blocks (R2) inside the coin-platform-aggregator module of the coins crate. Chapter-bound browser builds skip the entire Lightning module. |
| The Z-coin Sapling sub-crate of the coins crate. | The Sapling cryptographic substrate consumes foreign-function-interface code that does not cross-compile to the browser target triple. |
| The Solana sub-crate of the coins crate (see chapter 27). | The modular Solana sibling-allowlist crate set replaced the earlier chained substrate (the `solana-remote-wallet` → `hidapi` → `libudev` chain) precisely so that the mobile cross-compiles of R18 could succeed. |
| The Trezor hardware-wallet crate. | Native uses the C `secp256k1-ffi` substrate and a USB human-interface-device transport; the WebAssembly target table ships `js-sys`/`wasm-bindgen` for a in-browser variant. |
| The Ledger hardware-wallet crate. | The crate at the substrate landing point has only the WebAssembly-side WebUSB transport in its dependency table; the native HID path is scaffolded but not integrated and the crate is not yet wired into the rest of the workspace. |

> **Status update (informative) -- Z-coin row.** The Z-coin row records the
> substrate as it stood when this table was bound. Chapter 39 §39.6.1 has
> since made the shielded coin buildable and activatable on the WebAssembly
> target; only shielded transaction *building* (the local Sapling prover)
> remains native-only. Chapter 39 R39.10.1 accordingly requires the
> `task::enable_z_coin::*` activation methods to be routed on WebAssembly
> builds as well. Where this row and chapter 39 disagree, chapter 39 governs
> for the Z-coin. In Reloaded the methods are routed on WebAssembly, but a WASM
> activation currently ends with `CoinCreationError`, because the WASM build has
> no shielded wallet database to compute a balance from (chapter 39 §39.10
> Reloaded status, item 1).

**R18.** Conversely, the browser-wallet integration
crate `mm2_metamask` is browser-only by definition
(the provider object bound by the EIP-1193 protocol only exists inside a browser).
Its dependency table is essentially: *everything
in the WebAssembly target block, nothing in the
native target block*.

### 26.11.1 Bound HID-Driver Substrate

The Trezor hardware-wallet stack of R17 reaches the
device through a native-only human-interface-device
(HID) transport. The transport substrate sits in the
`hw_common` infrastructure crate (chapter 27 §27.11)
as a single module gated `#[cfg(not(target_arch = "wasm32"))]`.
This sub-section binds its shape.

**R17-A.** The substrate MUST wrap exactly one
`hidapi::HidApi` instance per process. A
process-global atomic boolean MUST track whether the
wrapper is initialised; the constructor MUST fail
with a typed "initialised already" error if the flag
is set, and the wrapper's `Drop` MUST clear the flag
(warning at log-level if it was not set, i.e. the
wrapper was already released by another path).

**R17-B.** The transport MUST expose three logical roles plus a
typed error set, described here by behaviour only (concrete type
names, method names, and signatures are an implementation choice):

- a *singleton handle* obtained by asynchronous initialisation that
  owns the shared library context and offers exactly one operation:
  enumerate the currently attached devices;
- a *device handle* representing one enumerated device, carrying a
  clone of the shared context and the device's identity record, and
  offering: connect, an open-state query, write-one-chunk, and
  read-one-chunk;
- an *identity record* carrying vendor id, product id, interface
  number, the operating-system device path, and optional
  serial/manufacturer/product text. It MUST be cheaply cloneable and
  usable as a map key (it MUST support equality and hashing) and MUST
  be constructible from the underlying library's device-info value.

Internally, the shared context holds the underlying library handle
plus a map of currently-open devices keyed by identity record, and is
shared (reference-counted) across all handles behind a single
asynchronous mutex. The mutex MUST be a futures-aware async mutex,
because the lock is held across awaited I/O; a synchronous
standard-library mutex MUST NOT be used.

**R17-C.** Every device open MUST place the underlying library
device in non-blocking mode. Reads MUST therefore use the library's
plain non-blocking read and MUST NOT use its timeout-bearing read
(the two have undefined interaction when combined with non-blocking
mode in the underlying library; this is the chapter-21-bound
rationale). A read MUST return whatever the non-blocking read
produced, which MAY be **shorter than the requested length**,
including length zero. Retry and poll semantics are the caller's
responsibility (the Trezor session layer of chapter 28).

**R17-D.** Connecting to an enumerated device MUST select its open
strategy from the device's identity record: when the record carries a
usable operating-system path, the device MUST be opened by that path;
otherwise the transport MUST fall back to opening by the (vendor id,
product id, serial number) triple. If neither a usable path nor a
serial number is available, connection MUST fail with a typed
insufficient-identifying-information error.

**R17-E.** The transport MUST define a single typed error set,
surfaced only inside the process boundary (a display-only error; it
is never serialised across the RPC boundary) and returned through the
project's standard error wrapper. The set MUST distinguish at least
the following conditions:

- access to a device that is not present in the open-devices map;
- an attempt to open a device that is already open;
- a second initialisation of the process singleton;
- failures reported by the underlying library during initialisation,
  enumeration, open, write, and read;
- a partial-write interruption that reports both the requested chunk
  length and the number of bytes actually sent;
- an over-long read that reports both the actual and the expected
  length;
- insufficient information to connect;
- an internal catch-all carrying a message.

The not-present-device condition MUST be raised by both the write and
read operations when the device-info key is absent from the
open-devices map. The partial-write condition MUST be raised when the
underlying write reports fewer bytes than requested. The
over-long-read condition MUST be raised when the underlying read
reports more bytes than requested (which indicates a defective device
or a bug in the underlying library).

**R17-F.** The open-state query MUST be defined as: the device is
present in the open-devices map **and** still appears in a fresh
enumeration of system devices. The transport MUST document that the
second half of this predicate is expensive on some operating systems
(it triggers a USB enumeration) and that callers needing a hot-path
readiness check must cache it. The rationale is that the underlying
library exposes no disconnect notification, so a fresh enumeration is
the only available liveness signal.

**R17-G.** Enumerating devices MUST refresh the underlying library's
device list before reading it, and MUST map both a refresh failure and
a wrapper-construction failure to the enumeration-failure error of
R17-E. Each returned device handle MUST receive a clone of the shared
context.

**R17-H.** The transport MUST record (in module documentation) the
rationale for its shape: the underlying library context is neither
thread-safe nor asynchronous, so a wrapper must either dedicate a
thread to it or guard it with a mutex and use non-blocking I/O. This
substrate takes the second route (see R17-A–R17-C). The documentation
MUST also note the absence of a device-disconnect signal in the
underlying library, which motivates the enumeration-based liveness
check of R17-F.

## 26.12 Bound Continuous-Integration Matrix

**R19.** The continuous-integration substrate MUST
build the cartesian product of the target matrix and the (build / test / lint) job
axis under the workflows directory of the
continuous-integration substrate. The
workflow registry MUST be:

| Bound workflow | Bound covered targets |
| ---------------------- | -------------------------------------------------------------------------------- |
| `build-linux.yml` | `x86_64-unknown-linux-gnu` |
| `build-macos.yml` | `x86_64-apple-darwin` + `aarch64-apple-darwin` + `lipo`-merged Universal |
| `build-windows.yml` | `x86_64-pc-windows-msvc` |
| `build-wasm.yml` | `wasm32-unknown-unknown` |
| `build-ios.yml` | `aarch64-apple-ios` |
| `build-android.yml` | `aarch64-linux-android` + `armv7-linux-androideabi` |
| `dev-build.yml` | Chapter-bound orchestrator that calls all of the above. |
| `test.yml` | Chapter-bound unit + integration + container + WebAssembly test jobs. |

The Android workflow MUST consume the
sibling-allowlist Cargo sub-command `cargo-ndk` to wrap
NDK cross-compilation. The iOS
workflow MUST rely on the Apple toolchain on a
macOS runner; the macOS Universal
artefact MUST be produced by `lipo`-merging the
two single-arch builds. The ARMv7
Linux workflow MUST consume the cross-compilation
configuration of chapter 03 R11 with the project-specific container image (the standard
cross-compilation container image lacks several audio and human-interface-device headers that the workspace dependency tree needs even on a server
build).

The WebAssembly target MUST receive two 
continuous-integration safety nets: a `cargo check` invocation against the browser
target triple for fast feedback on target-table errors, plus a `wasm-pack build`
invocation that exercises the actual `wasm-
bindgen` code-generation path the browser package
goes through. Either failing MUST fail the build.

## 26.13 Tests

**T1.** *Per-target build invocation.* The continuous-integration substrate of R18 MUST be confirmed to
issue the per-target build invocation against
every row of the nine-row target
matrix on every merge against the default branch.

**T2.** *Compilation-guard discipline.* A regression test MUST grep the workspace for
point-of-use platform predicates and confirm that
they consume exactly the three permitted forms of
R1 — the `cfg_native!`/`cfg_wasm32!` macros, the
`#[cfg(target_arch = "wasm32")]` and `#[cfg(not(target_arch = "wasm32"))]` attribute pair, and the
per-crate manifest target-table substrate.

**T3.** *Dual-implementation symmetry.* A regression test MUST confirm that for every storage trait of R8 step 1 there exists a `sqlite_storage.rs` (or `sqlite/` sub-module) and
a `wasm_storage.rs` module, and that the
module-roots module branches on the R8
step 4 attribute pair.

**T4.** *Browser-target safety-net pair.* The continuous-integration substrate MUST confirm both R18 
browser-target safety nets (the check
invocation and the `wasm-pack` invocation) run on
every merge against the default
branch and that either failing fails the
build.

## 26.14 Deferred Work

**D1.** Chapter-bound automated cross-target test execution: the
browser-target continuous-integration job at the
substrate landing point runs only the check and
the `wasm-pack build` (R18); the mobile-target continuous-integration jobs run only the
build step. Chapter-bound run-time behaviour on those
targets is checked manually.

**D2.** A collapse of the three
permitted compilation-guard forms of R1 into a
single crate-level helper.

**D3.** A trait-on-trait WebAssembly erasure
substrate: a few sites at the substrate landing
point still require manual `Send`-stripping in
asynchronous traits to keep the browser target healthy (the sibling-allowlist
asynchronous-trait crate does not have a target-aware non-`Send` mode for some of the
trait shapes the substrate consumes).

**D4.** Explicit declaration of the mobile
foreign-function-interface surface. The mobile-
bindings crate of R6 exposes `lp_main`/`mm2_status` as 
plain Rust accessors; the mobile-host glue
(Swift / Kotlin) consumes them via the C
application-binary-interface surface of the `cdylib`/`staticlib` artefact. No `#[no_mangle] extern "C"` declaration is present in this crate
at the substrate landing point; the C application-
binary-interface surface is whatever the
public accessors in the application-entry
crate `mm2_main` happen to emit.

**D5.** A browser-side inter-page-process-
communication substrate. The browser build at
the substrate landing point assumes a single
browser-instance per page; no shared-worker or message-channel substrate is present, and a
consumer wanting multi-page state
sharing must implement it in the JavaScript
hosting layer.

**D6.** *A known R8 non-conformance.* R8 binds every persistence
consumer, the version-two swap state stores among them by name, to a
real native-SQLite-plus-WebAssembly-IndexedDB pair. The version-two
swap state stores do not meet this today: their WebAssembly
`StateMachineStorage` implementation is an unconditional no-op (empty
write, `false`/empty read), not a working IndexedDB backend. Full
detail, and the two directions considered for closing it, are recorded
against the owning chapter rather than duplicated here: [Chapter
52](52-swap-v2-state-machine.md) D7. Recorded here too because this is
the chapter that states the general rule this specific consumer
breaks — a reader auditing R8 conformance chapter-by-chapter should
find this gap from either direction.

## 26.15 Baseline Verifications

**V1.** The chapter-02-anchored baseline tree MUST be confirmed
to ship a earlier form of the `cfg_native!`/`cfg_wasm32!` macros (R2) inside the shared-utility crate `common`; a native-only main
plus a `cdylib` entry in a single
crate at the baseline workspace; and a
early IndexedDB layer (an ancestor of R9). The
baseline tree MUST NOT yet carry: the macOS
Universal `lipo` step (R18); the iOS static-
library target (R18); the Android cargo-ndk
workflow (R18); the modern WebAssembly 
continuous-integration safety nets (R18); the
dual asynchronous-runtime substrate's `AbortOnDropHandle` newtype (R11); the mobile-
bindings crate `mm2_bin_lib` (R6); the split
between the desktop binary in `mm2_main` and the
mobile-bindings shim (R6); the dual
`*_storage.rs` pattern in the new persistence
consumers added by later chapters (chapter 22, chapter 24); or
the Solana sibling-allowlist replacement that
unblocked mobile cross-compilation (chapter 27).

**V2.** The chapter-02 R8 baseline build-target surface MUST be
confirmed to contain a subset of the nine-row target matrix of R1; specifically, the target rows added by the substrate (the Universal
macOS row, the iOS row, the double
Android row) MUST be confirmed absent at the chapter-02-
anchored baseline.

**V3.** The chapter-02 R5 baseline patched-dependency substrate
MUST be confirmed not to contain a `solana-remote-wallet` patched-entry; the chapter-27-bound
sibling-allowlist replacement substrate the mobile cross-compilation depends on is bound under chapter 27,
not under chapter 02 R5.

## 26.16 External References

- The `wasm-bindgen` / `wasm-bindgen-futures` /
  `web-sys` / `js-sys` family of browser-
  interoperability crates (the sibling-allowlist
  origin under the `rustwasm` project).
- The `cfg_if` crate (the sibling-
  allowlist origin on the public Cargo registry).
- The `cross-rs` cross-compilation substrate and
  its container-image conventions.
- The Android NDK and the `cargo-
  ndk` Cargo sub-command (the sibling-allowlist
  origin).
- The Apple `lipo` tool (per its manual page on a macOS runner) for the
  Universal-binary merge.
- The `wasm-pack` build tool (the sibling-allowlist origin under the `rustwasm`
  project).
- The World-Wide-Web-Consortium *Indexed Database
  API* specification as the underlying browser
  store surfaced by the `mm2_db` IndexedDB
  substrate of R9.

## 26.17 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit of chapter 02 (covering V1, V2, V3); chapter 02 (the
  chapter-02 R4 workspace-member registry, the chapter-02 R5
  patched-dependency substrate, the chapter-02 R8 build-target
  surface); chapter 03 (the toolchain pin and the
  2021-edition migration; the Cross.toml byte-identical preservation of chapter 03 R11
  consumed by R18); chapter 05 (the canonical
  hierarchical-deterministic-wallet storage worked instance of
  R8); chapter 12, chapter 14, chapter 15 (the swap state-store consumers of R8); chapter 19 (the
  non-fungible-token consumer of R8); chapter 22 (the
  WalletConnect consumer of R8 and the
  transport-layer consumer of R13); chapter 24 (the
  graphical-user-interface account-state
  consumer of R8); chapter 25 (the asynchronous
  connection facade of chapter 25 R11 consumed by R8 step 2);
  chapter 27 (the Solana sibling-allowlist
  replacement substrate cited under R16 and V3); the
  public browser-interoperability documentation, the
  public conditional-compilation crate
  documentation, the public cross-compilation
  substrate documentation, the public NDK and
  cargo-ndk documentation, the public `lipo`
  documentation, the public `wasm-pack`
  documentation, and the World-Wide-Web-
  Consortium Indexed-Database-API specification.
- *Permitted-input classes used:* the baseline itself (chapter 01
  R1); external public specifications (chapter 01 R3, for the
  World-Wide-Web-Consortium Indexed-Database-API specification
  citation and for the EIP-1193 protocol
  citation); sibling open-source repositories under compatible
  licenses (chapter 01 R5, for the browser-
  interoperability family, the conditional-
  compilation crate, the cross-compilation
  substrate, the cargo-ndk crate, the
  `wasm-pack` crate, the async-trait crate,
  the asynchronous-runtime crate, the
  asynchronous WebSocket crate, the asynchronous-HTTP-client crate, the `gstuff`
  filesystem helpers, the asynchronous browser-
  future-adaptor crate, the abort-handle crate,
  and the modular Solana crate set).
- *Sibling-allowlist consultations:* the `wasm-
  bindgen` / `wasm-bindgen-futures` / `web-sys` / `js-sys`
  browser-interoperability family; the `cfg-if`
  conditional-compilation crate; the `cross-rs`
  cross-compilation substrate; the `cargo-ndk`
  Cargo sub-command; the `wasm-pack` build tool;
  the async-trait crate; the asynchronous-runtime crate; the asynchronous-
  WebSocket crate; the asynchronous-HTTP-client
  crate over the TLS adaptor; the `gstuff` filesystem helpers; the asynchronous
  browser-future-adaptor crate; the abort-handle
  crate.
- *Forbidden corpus:* not consulted.
