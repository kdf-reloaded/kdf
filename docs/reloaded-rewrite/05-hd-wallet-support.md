# Chapter 05 — Hierarchical-Deterministic Wallet Support

**Status:** driving-spec.

A modernised cryptographic-key substrate that binds a complete
BIP-39 / BIP-32 / SLIP-0010 / SLIP-0021 wallet stack on top of the
baseline single-passphrase model, retains the legacy model as a
coexisting first-class policy, and routes per-coin key access
through a single context-level discriminator.

## 5.1 Executive Summary

The baseline tree carries a single key-source model: a freeform
ASCII passphrase (the chapter-bound substrate identifier is
`Iguana` passphrase, the type-level identifier is `IguanaCtx`)
that is deterministically hashed into one secp256k1 secret per
running daemon and used by every coin for every signature. The
baseline carries the *shape* of hierarchical-deterministic
derivation (a typed BIP-32-child path, a strict BIP-44 derivation
path, a hardware-wallet context type), but no end-to-end BIP-39
mnemonic flow, no BIP-43 generality, no ed25519 derivation, no
encrypted-mnemonic format, no per-coin policy discriminator, and
no MetaMask integration.

The substrate bound by this chapter introduces a complete
hierarchical-deterministic wallet stack derived from public
specifications (BIP-32, BIP-39, BIP-43, BIP-44, BIP-49, BIP-84,
SLIP-0010, SLIP-0021, SLIP-0044) without removing the legacy
passphrase path. Concretely the substrate binds:

- a BIP-39 mnemonic generation, encryption, and decryption
  surface (R6–R8);
- an authenticated-encryption layer for the at-rest encrypted
  mnemonic with two parallel key-derivation strategies (R9–R12);
- a `GlobalHDAccountCtx` substrate that holds the 64-byte BIP-39
  seed plus two parallel master extended private keys (BIP-32
  secp256k1 for Bitcoin-family coins; SLIP-0010 ed25519 for
  Cosmos- and Solana-family coins) with seed-on-drop zeroisation
  (R13–R16);
- a `StandardHDPath` type generic over a `Bip43Purpose`
  enumeration so the same path machinery serves BIP-44, BIP-49,
  and BIP-84 (R17–R19);
- a `KeyPairPolicy` discriminator on the central
  cryptographic-context type so coins access keys through
  policy-neutral helpers regardless of mode (R20–R23);
- an `XPubConverter` cross-network base58 extended-public-key
  version-byte translator (R24);
- a hardware-wallet path retained verbatim from baseline (R25);
- a WebAssembly-only MetaMask context analogue (R26).

The wire-visible surface (RPC method names, RPC field names,
JSON-serialised derivation paths) is either unchanged from the
baseline or named after the public specifications the substrate
expresses. Bound rules R1–R5 cover the crate-level shape; R6–R12
cover the mnemonic and encryption layer; R13–R19 cover the
hierarchical-deterministic seed and path machinery; R20–R26 cover
the central context and the platform-gated extensions.

## 5.2 Subsystem Shape

The substrate is contained inside the cryptographic-key crate
(chapter-bound identifier: `crypto`). It expands the crate from
its baseline nine-source-file footprint to a twenty-source-file
footprint, retaining every baseline file (each either unchanged
or extended) and adding eleven new files (nine on every target,
two gated to the WebAssembly target).

The substrate sits between four neighbouring subsystems:

- the central-context substrate (chapter 7 binds the wallet-name
  store and the encrypted-mnemonic envelope; this chapter's
  encrypted-mnemonic format is the payload that envelope carries);
- the chapter-bound persistence subsystem (the encrypted mnemonic
  is read from the chapter-7 file store at daemon startup);
- the atomic-swap subsystem (the secret-hash-algorithm
  discriminator bound by R27 is consumed by the version-two swap
  state machines bound in chapter 14);
- the cross-platform-and-WebAssembly substrate (chapter 26
  documents the WebAssembly-target gating used by R26).

The substrate does *not* remove or repurpose any baseline
identifier. Every type-level name that callers depend on at
baseline (`IguanaCtx`, `Bip44DerivationPath`,
`HardwareWalletCtx`, the BIP-32-child types) MUST remain
re-exported.

## 5.3 Bound Crate Shape

**R1.** The substrate MUST extend the chapter-bound crate name
`crypto`. The crate MUST contain exactly the following module
surface after the substrate lands (baseline modules retained;
new modules added):

| Module                | Bound responsibility                                         |
| --------------------- | ------------------------------------------------------------ |
| `crypto_ctx`          | Central context plus `KeyPairPolicy` discriminator (R20).    |
| `key_pair_ctx`        | Baseline legacy-passphrase context (`IguanaCtx`).            |
| `privkey`             | Baseline passphrase-to-secp256k1 deterministic derivation.   |
| `bip32_child`         | Baseline typed BIP-32 child enumeration.                     |
| `bip44`               | Baseline strict BIP-44 path retained as backward-compat re-export (R2). |
| `hw_client` / `hw_ctx` / `hw_rpc_task` | Baseline hardware-wallet path retained (R25).        |
| `lib`                 | Re-export surface preserving all baseline names.             |
| `mnemonic`            | BIP-39 mnemonic generation, encryption, decryption (R6–R8).  |
| `encrypt` / `decrypt` | Authenticated-encryption pair (R9, R10).                     |
| `key_derivation`      | Argon2id and SLIP-0021 key-material derivation (R11, R12).   |
| `slip21`              | SLIP-0021 symmetric-key-tree node derivation (R12).          |
| `global_hd_ctx`       | `GlobalHDAccountCtx` substrate (R13–R16).                    |
| `standard_hd_path`    | `StandardHDPath` and `Bip43Purpose` machinery (R17–R19).     |
| `xpub`                | `XPubConverter` cross-network version-byte translator (R24). |
| `secret_hash_algo`    | Atomic-swap secret-hash algorithm discriminator (R27).        |
| `metamask_login` / `metamask_ctx` | WebAssembly-only MetaMask integration (R26).         |

**R2.** Every baseline-exported type-level identifier MUST remain
exported by the substrate. The bound list (each MUST be reachable
through the same path that worked at baseline):
`IguanaCtx`, `Bip44DerivationPath`, `Bip44PathToCoin`,
`Bip44PathToAccount`, `HardwareWalletArc`, `HardwareWalletCtx`,
`TrezorConnectProcessor`, `HwClient`, `HwError`,
`HwProcessingError`, `HwResult`, `HwWalletType`, plus the
typed-child types from the baseline BIP-32-child module. The
substrate MAY add new modules that supersede these for new call
sites, but MUST NOT rename or remove them.

**R3.** The substrate MUST add the following five new dependency
surfaces (and MUST NOT add others within the scope of this
chapter): a BIP-39 word-list crate (`bip39`); a BIP-32 extended
key crate (`bip32`); a SLIP-0010 ed25519 derivation crate
(`ed25519-dalek-bip32`); an Argon2 password-hashing crate
(`argon2`); a zeroisation crate (`zeroize`). AES, HMAC, and
SHA-256 primitives MUST come from the chapter-bound symmetric
primitives already present in the workspace.

**R4.** Every new identifier the substrate adds (type names,
function names, struct field names, enumeration variant names)
MUST be either (a) the name used by the public specification it
expresses (BIP/EIP/SLIP/RFC term, e.g. `Argon2Params`,
`Bip43Purpose`, `Bip39Seed`), (b) a neutral compositional name
constructed from the substrate's own conventions (e.g.
`KeyPairPolicy`, `GlobalHDAccountCtx`, `EncryptedMnemonicData`,
`KeyDerivationDetails`, `XPubConverter`), or (c) a baseline
identifier extended in place. Identifiers MUST NOT be borrowed
from a private source.

**R5.** The baseline-shared central context (chapter-bound name
`CryptoCtx`) MUST remain the only public façade through which the
rest of the daemon accesses key material. The substrate MUST NOT
introduce a parallel global accessor that bypasses the context.

## 5.4 Bound BIP-39 Mnemonic Surface

**R6.** The mnemonic module MUST expose exactly the following
public surface:

| Item                                            | Bound role                                                            |
| ----------------------------------------------- | --------------------------------------------------------------------- |
| `generate_mnemonic(word_count)`                | Generate a fresh BIP-39 mnemonic with the requested word count (R7).  |
| `encrypt_mnemonic(plaintext, derivation, ...)` | Encrypt a mnemonic at rest (R8, R9, R11).                            |
| `decrypt_mnemonic(envelope, ...)`              | Reverse the encrypted-at-rest operation.                              |
| `EncryptedMnemonicData`                        | The on-disk envelope (R8).                                            |
| `MnemonicError`                                | The substrate-wide mnemonic error enumeration.                        |

**R7.** `generate_mnemonic` MUST accept exactly the BIP-39
permitted word counts: 12, 15, 18, 21, 24 (corresponding to 128,
160, 192, 224, 256 bits of entropy respectively). Any other word
count MUST return the substrate's mnemonic error variant for
invalid length. Entropy MUST come from the operating-system
random-number generator (the chapter-bound accessor in the
shared-utilities crate).

**R8.** The on-disk envelope (`EncryptedMnemonicData`) MUST be a
struct with exactly two named fields (no extra metadata): the
authenticated-encryption ciphertext block (chapter-bound name
`EncryptedData` from R9) and the key-derivation parameters
(chapter-bound name `KeyDerivationDetails` from R11). The
envelope MUST round-trip through the serialisation derive macros
without consumer-supplied helpers, and the field names MUST be
`encrypted_data` and `key_derivation` exactly.

## 5.5 Bound Authenticated-Encryption Layer

**R9.** The encryption module MUST expose an
authenticated-encryption pair using AES in cipher-block-chaining
mode under a 256-bit key, with PKCS-7 padding and a per-encryption
16-byte initialisation vector. The serialised `EncryptedData`
struct MUST carry the initialisation vector, the ciphertext, and
an HMAC-SHA-256 tag in named fields with consumer-stable serde
output.

**R10.** The HMAC tag MUST be computed over the concatenation of
the initialisation vector and the ciphertext (encrypt-then-MAC),
and MUST be verified with a constant-time comparator *before*
decryption is attempted. A tag mismatch MUST short-circuit
decryption with the substrate's authentication-failure variant;
the substrate MUST NOT expose plaintext bytes derived from a
ciphertext whose tag did not verify.

**R11.** The key-derivation module MUST expose exactly two
derivation modes, both publicly specified:

| Mode      | Specification                                  | Bound parameter set                                                          |
| --------- | ---------------------------------------------- | ---------------------------------------------------------------------------- |
| Password  | Argon2id (RFC 9106 variant `Argon2id`)          | Salt, iteration count, memory cost (in KiB), parallelism — recorded in `Argon2Params` and serialised alongside the ciphertext so decryption is self-describing. |
| Seed      | SLIP-0021 symmetric-key-tree derivation         | Node path under the unlocked 64-byte BIP-39 seed (R13).                      |

The `KeyDerivationDetails` enumeration MUST tag which mode was
used and carry the corresponding parameter set. The substrate
MUST NOT introduce a third derivation mode in this chapter.

**R12.** The SLIP-0021 module MUST implement node derivation
verbatim from the specification: an HMAC-SHA-512 keyed by the
parent node's right half, with the input being the byte sequence
`0x00 || label_bytes`, producing a 64-byte child node whose left
half is the symmetric key and whose right half is the chain code
seed for further derivation.

## 5.6 Bound Seed-and-Master-Key Substrate

**R13.** The `GlobalHDAccountCtx` substrate MUST own exactly the
following state: a 64-byte BIP-39 seed (wrapped per R14); a
secp256k1 extended private key (BIP-32 master derived from the
seed); an ed25519 signing key (SLIP-0010 master derived from the
same seed); and a chapter-bound internal secp256k1 key pair
(R16). The substrate MUST NOT carry any other long-lived key
material in this context.

**R14.** The 64-byte seed MUST be wrapped in a `Bip39Seed`
newtype whose `Drop` implementation explicitly zeroises the
underlying byte array via the chapter-bound zeroisation crate.
The substrate MUST NOT expose the raw seed bytes through any
accessor; consumers MUST go through the two derivation helpers
of R15.

**R15.** The substrate MUST expose exactly two key-derivation
helpers, both spelled in terms of public types:

| Helper                                       | Bound role                                                                         |
| -------------------------------------------- | ---------------------------------------------------------------------------------- |
| `derive_secp256k1_secret(&derivation_path)` | Derive a secp256k1 secret at the given BIP-32 derivation path from the BIP-32 master. Returned type is the chapter-bound `Secp256k1Secret` newtype. |
| `derive_ed25519_signing_key(&derivation_path)` | Derive an ed25519 signing key at the given SLIP-0010 derivation path from the SLIP-0010 master. |

Coins call whichever helper matches their key family. The
substrate MUST NOT expose curve-aware logic to coins beyond these
two helpers; per-coin path construction is the responsibility of
the path machinery of R17.

**R16.** The substrate MUST derive one specially-named
secp256k1 key pair at construction time, used by the daemon
itself for peer-identity and internal signing. The derivation
path MUST be exactly `m/44'/141'/2147483647/0/0`. The path
encodes (BIP-44 notation): purpose 44, coin type 141 (the
SLIP-0044 registry entry for the chapter-bound platform-token
identifier `KMD`), account 2147483647 (the largest BIP-32
non-hardened index, chosen so it cannot collide with any
user-facing account), chain 0, address 0. The substrate MUST
expose this key through a chapter-bound accessor named
`mm2_internal_*` (the chapter-bound prefix marking that the key
is for internal daemon use, not user funds).

## 5.7 Bound Path Machinery

**R17.** The path module MUST introduce a `Bip43Purpose`
enumeration with exactly the variants `Bip32`, `Bip44`, `Bip49`,
`Bip84`, corresponding to purpose values 32, 44, 49, 84
respectively. The substrate MUST NOT add a fifth variant in
this chapter.

**R18.** The path module MUST introduce a `StandardHDPath`
generic over the `Bip43Purpose` value at the path's
purpose-level. The full five-level shape MUST be
`purpose'/coin_type'/account'/chain/address_index`, identical in
structural form to the baseline strict BIP-44 path but generic
over the purpose. Two convenience aliases MUST be exposed at
truncated levels:

| Alias              | Bound path levels                              |
| ------------------ | ---------------------------------------------- |
| `HDPathToCoin`     | `purpose'/coin_type'` (two levels).           |
| `HDPathToAccount`  | `purpose'/coin_type'/account'` (three levels). |

**R19.** The baseline strict BIP-44 path types
(`Bip44DerivationPath`, `Bip44PathToCoin`, `Bip44PathToAccount`)
MUST remain re-exported per R2. The new generic types MUST be
re-exported alongside them under their own names. Substrate MUST
NOT route old callers through the new types implicitly; type
parity is preserved by retention, not by aliasing.

## 5.8 Bound Central-Context Discriminator

**R20.** The central context module MUST grow a `KeyPairPolicy`
enumeration with exactly two variants:

| Variant             | Bound payload                                                                                                         |
| ------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `Iguana`            | The baseline `IguanaCtx` (or a reference-counted handle to it) containing the single secp256k1 key pair derived from the freeform passphrase. |
| `GlobalHDAccount`   | A reference-counted handle to `GlobalHDAccountCtx` (R13).                                                              |

The substrate MUST NOT add a third variant in this chapter. The
substrate MUST NOT remove the `Iguana` variant in this chapter;
the legacy passphrase model is a coexisting first-class policy
(R22).

**R21.** The central context MUST expose policy-neutral
derivation helpers (`derive_secp256k1_secret`,
`derive_ed25519_signing_key`) that dispatch internally based on
the active `KeyPairPolicy` variant. Coins MUST NOT branch on
`KeyPairPolicy` inside their derivation paths; only RPC handlers
that need to surface the mode to the operator MAY inspect the
discriminator.

**R22.** The legacy passphrase initialisation path MUST remain
fully supported: a daemon configured with the baseline-style
freeform passphrase field MUST start, derive the single
secp256k1 key pair, route every coin operation through it, and
inter-operate on the peer-to-peer network exactly as at
baseline. The substrate MUST NOT introduce a startup-time
deprecation gate on the legacy path.

**R23.** Renaming of baseline `iguana_ctx()` callers to the
chapter-bound `mm2_internal_*` accessor prefix is in scope only
for those callers that genuinely need the chapter-bound internal
key from R16; callers that need the user-facing legacy key MUST
continue to access it through the policy-neutral helpers of
R21.

## 5.9 Bound Extended-Public-Key Translation

**R24.** The substrate MUST expose a single helper named
`XPubConverter` that re-serialises an extended public key under
a different version-byte prefix. The bound translation MUST
mechanically parse the input under one of the BIP-32 / BIP-49 /
BIP-84 version-byte tables and re-emit under another. The
substrate MUST NOT introduce a project-specific version-byte
table; both source and destination tables MUST be the public
ones.

## 5.9A Bound Software Global-HD Account-Key Derivation

This section binds the **software** (non-hardware) path that makes
a global-HD account usable by Bitcoin-family (UTXO) coins without a
hardware device. It complements the per-curve derivation helpers of
R15 and the policy discriminator of R20–R21: where those bind the
in-memory master-key machinery, this section binds (a) the
context-level wallet-identity value that namespaces stored HD
accounts in software mode, (b) the canonical extended-public-key
that the in-memory master yields for an account derivation path,
and (c) the policy-driven selection between the hardware and
software extended-public-key sources. The coin-side account
bootstrap, new-address, and scan behaviour that consume this
surface are bound in Chapter 38 §38.8; this section is the crypto
substrate those requirements rest on.

**R29.** *Software-HD wallet identity.* The central cryptographic
context MUST expose, for the `GlobalHDAccount` key-pair policy
(R20), the 20-byte wallet-identity digest that namespaces
per-wallet hierarchical-deterministic storage, **derived from the
in-context software identity, not from a hardware device**. The
value MUST be the standard `RIPEMD160(SHA256(pubkey))` digest of
the global-HD identity's internal secp256k1 public key (the same
daemon-wide public-key-hash the context already derives from the
active key-pair policy at construction, i.e. the value Chapter 5
R16 binds for the internal key path and that the central
application context surfaces as its `rmd160` identity). For a
software global-HD account this digest is therefore equal to the
context-wide `mm2_rmd160` identity, matching the on-disk HD-wallet
identity contract that an HD wallet launched from a passphrase-
derived master key shares its `mm2_rmd160` and `hd_wallet_rmd160`
namespacing values. The value MUST be **stable across daemon
restarts for the same mnemonic** so that stored HD accounts re-bind
on re-login. When the policy is `Iguana`, no HD wallet-identity is
available and HD storage MUST remain refused (HD is unsupported in
Iguana mode); when a hardware-wallet context is active, the
hardware device's own identity digest is used unchanged (R25).
Acceptance: in software global-HD mode, an HD UTXO coin's HD
storage initialises successfully (it MUST NOT be refused on the
grounds that no hardware HD-wallet identity is present); in Iguana
mode the same request is refused.

**R30.** *Software account extended-public-key derivation and
canonical serialisation (dictated interop).* In software global-HD
mode the account-level extended **public** key for a coin MUST be
derived **from the in-memory BIP-32 secp256k1 master extended
private key** held by `GlobalHDAccountCtx` (the master `m` exposed
by `root_priv_key`, R13/R15), by walking the configured account
derivation path `purpose'/coin_type'/account'` (Chapter 5 R18
`HDPathToAccount`, generic over BIP-43 purpose per Chapter 38
R38.3.3) and taking the extended public key at that node. The
software path MUST NOT require any `trezor_coin` coins-config field
and MUST NOT contact a hardware device.

  The derived account extended public key MUST serialise to the
  **canonical BIP-32 extended-public-key (xpub) form**: a
  base58check string over the 78-byte BIP-32 serialization whose
  4-byte version prefix is the standard mainnet public version
  `0x0488B21E` (the `xpub` prefix). This is the value persisted as
  the on-disk `account_xpub` schema column (Chapter 44) and is
  **dictated interop**: it MUST equal, byte-for-byte, the `xpub` a
  conformant reference wallet computes for the same BIP-39 mnemonic
  at the same account derivation path. The stored/serialised
  account xpub MUST use the `xpub` version prefix uniformly and
  MUST NOT be re-versioned per coin network at this layer
  (per-network display re-versioning, when required elsewhere, is
  the separate `XPubConverter` concern of R24). A wrong version
  prefix or a non-canonical serialization is a conformance failure,
  because downstream address derivation depends on the exact bytes.

  > **Upstream divergence (informative).** The relicensed substrate
  > as received carried only a hardware (device) extended-public-key
  > source; the software derivation above was absent, so a software
  > global-HD account could not produce an account xpub and HD
  > activation/address derivation failed. This section binds the
  > software derivation as first-class. The behaviour is expressed
  > from the public BIP-32 specification (the master-to-account
  > public-key walk and the `0x0488B21E` `xpub` serialization are
  > dictated by BIP-32), not transcribed from any private source.

**R31.** *Extended-public-key source selection by key-pair policy.*
The extended-public-key source consumed by per-coin account
extraction MUST be chosen by the active key-pair policy, not by a
fixed assumption of hardware:

  - when a hardware-wallet context is active, the source is the
    hardware device (the device extractor of R25, which requires
    the coin's `trezor_coin` config and performs the device
    protocol);
  - when the policy is `GlobalHDAccount` and no hardware context is
    active, the source is the **in-context software derivation of
    R30** (the in-memory master), requiring no `trezor_coin` field.

  The per-coin extraction entry point MUST accept "no external
  (hardware) extractor" as a valid case and, in that case, resolve
  the account xpub through the software derivation of R30. The
  selection MUST NOT branch inside coin signing/derivation paths
  beyond choosing the source; the resulting account xpub is
  identical in shape (R30) regardless of source. (Per-coin and
  per-account UTXO consumption is bound in Chapter 38 §38.8;
  Chapter 5 D1's deferral of per-account activation plumbing is
  partially discharged here for the extended-public-key source and
  fully for software UTXO accounts by Chapter 38 §38.8.)

## 5.10 Bound Hardware-Wallet Path

**R25.** The substrate MUST retain the baseline hardware-wallet
substrate verbatim. The chapter-bound module set
(`hw_client`, `hw_ctx`, `hw_rpc_task`) and the type-level
identifiers listed in R2's hardware-wallet group MUST remain
present and re-exported. When the central context holds a
`HardwareWalletArc`, derivation requests MUST be routed to the
device through the existing hardware-wallet protocol; the HD-path
types from R17 MUST be the same — only the signature computation
moves off-process.

## 5.10A Bound Trezor Connection-Status Query

**R28.** *Trezor connection-status query — RPC contract.* The
substrate MUST bind the daemon RPC method
`trezor_connection_status` (mmrpc 2.0, flat method), routed
through the version-two RPC dispatcher where the hardware-wallet
path of R25 is available. The method reports the current
connection state of the already-initialised Trezor hardware-wallet
context held by the central cryptographic context (R5 / R25) and
MUST expose the following wire contract:

- *Request (interop).* The request object has one optional field,
  `device_pubkey`. When supplied, the value is a hex-encoded
  20-byte hardware-wallet public-key identifier (the
  RIPEMD-160-of-SHA-256 digest of the device's extended public
  key, identical to the identifier the daemon reports for the
  device elsewhere). The field MAY be omitted when the caller only
  wants to query the currently initialised Trezor context.
- *Response (interop).* A successful response object has one
  field, `status`, whose value is one of two wire-visible
  discriminant strings: `"Connected"` or `"Unreachable"`.
  `"Connected"` means the Trezor context is reachable for the
  daemon's purposes, including the case where it is already in use
  by a concurrent task. `"Unreachable"` means the initialised
  context is disconnected or in an incorrect state and SHOULD be
  re-initialised.
- *Bound error surface (interop).* The method exposes a
  type-tagged error enum whose `error_type` tokens are part of
  the wire contract: `TrezorNotInitialized` (no hardware-wallet
  context is initialised on the running daemon),
  `FoundUnexpectedDevice` (the request asserted a device
  identifier that does not match the initialised Trezor context),
  and `Internal` (the cryptographic context was unavailable or
  another internal failure occurred). `TrezorNotInitialized` maps
  to HTTP 400; `FoundUnexpectedDevice` and `Internal` map to
  HTTP 500.

**R28A.** *Optional device-identity assertion.* The
`device_pubkey` field is an assertion by the caller, not a
selector. Trigger condition: the request supplies a non-null
`device_pubkey` and the daemon has an initialised Trezor context.
Required behaviour: before reporting connection status, the method
MUST compare the supplied identifier with the identifier of the
initialised Trezor context. If they differ, the method MUST return
`FoundUnexpectedDevice` and MUST NOT perform a connectivity probe.
If they match, the method MUST continue to the status resolution
of R28B. Trigger condition: the request omits `device_pubkey`.
Required behaviour: the method MUST NOT perform this identity
assertion and MUST NOT return `FoundUnexpectedDevice` solely
because the caller did not supply an expected identifier. If no
Trezor context is initialised, the method returns
`TrezorNotInitialized` regardless of whether the optional field is
present.

**R28B.** *Connection-status resolution and concurrent-session
branch.* After the R28A identity rule has passed or has been
skipped, the method MUST resolve status from the already-initialised
hardware-wallet context; it MUST NOT perform initial device
acquisition, rediscovery, or reconnection. Trigger condition:
another task already owns the exclusive device session for the
same Trezor context. Required behaviour: return a successful
`"Connected"` status without waiting for, stealing, cancelling, or
otherwise contending for that session. Trigger condition: the
context is initialised and the exclusive device session is
available. Required behaviour: perform a non-mutating reachability
probe through the existing transport/device-call path and return
`"Connected"` if that probe succeeds or `"Unreachable"` if that
probe fails. The status RPC MUST NOT impose an additional short
RPC-level deadline on this probe; completion/failure timing is
governed by the underlying transport and device-call behaviour.

**R28C.** *Observational status query.* The
`trezor_connection_status` call MUST be observational with respect
to wallet/account/key state. It MUST NOT create, remove, or
rewrite wallet state; MUST NOT derive, sign, export, or display
user keys or addresses; and MUST NOT start, require, or complete a
user-confirmation-dependent hardware-wallet operation. The
reachability probe is limited to connection-state observation and
MUST NOT request a user confirmation on the device. A failed
reachability probe MAY update volatile connection-state bookkeeping
so subsequent calls can report `"Unreachable"` without probing a
handle already known to be unavailable.

## 5.11 Bound WebAssembly-Only MetaMask Path

**R26.** The substrate MUST add two new modules
(`metamask_login`, `metamask_ctx`) gated to the WebAssembly
target. The login flow MUST follow EIP-191 (Personal Sign) and
EIP-712 (typed-data signing) verbatim, with no substrate-specific
extensions to either standard. The deeper integration (the
WebAssembly-side bridge, the JSON envelopes sent to the browser
provider) is owned by a sibling crate (chapter-bound identifier
`mm2_metamask`) and is bound by a later chapter; this chapter
binds only the surface the cryptographic-key crate adds to host
the integration.

## 5.12 Bound Atomic-Swap Secret-Hash Discriminator

**R27.** The substrate MUST add a `secret_hash_algo` module
exposing an enumeration that discriminates between the two
chapter-bound secret-hash algorithms used by hash-time-locked
contracts: the double-SHA-256-then-RIPEMD-160 form
(chapter-bound identifier `DHASH160`) and the single-SHA-256 form
(chapter-bound identifier `SHA256`). The enumeration's bound
consumers are the version-two atomic-swap state machines bound
in chapter 14 (chapter 15 for the UTXO path and chapter 17 for
the EVM path).

## 5.13 Tests

**T1.** *Mnemonic round-trip — Argon2id mode.* A fresh
mnemonic is generated with each of the five permitted word
counts; each is encrypted with a password through the Argon2id
mode and decrypted; the test asserts the recovered mnemonic
matches the original byte-for-byte.

**T2.** *Mnemonic round-trip — SLIP-0021 mode.* The same as
T1 but the encryption is keyed by SLIP-0021 derivation under a
known seed; the test asserts the recovered mnemonic matches.

**T3.** *Authentication failure short-circuits decryption.* An
encrypted envelope's HMAC tag is mutated by one bit; the
substrate's decrypt path is invoked; the test asserts the
authentication-failure variant is returned and no plaintext
bytes are produced.

**T4.** *Dual-curve master derivation.* A `GlobalHDAccountCtx`
is constructed from a known BIP-39 mnemonic; the test asserts
both master keys (BIP-32 secp256k1 and SLIP-0010 ed25519) match
the public-specification test vectors for that mnemonic.

**T5.** *Bound internal derivation path.* The chapter-bound
internal key pair is materialised through R16's derivation
path; the test asserts the path's byte representation matches
`m/44'/141'/2147483647/0/0` exactly.

**T6.** *Seed-on-drop zeroisation.* A `Bip39Seed` is
constructed in a controlled scope and dropped; the test
asserts (via a borrow of the underlying allocation through a
test-only accessor or via a sanitiser harness) that the bytes
are all-zero after drop.

**T7.** *Policy-neutral dispatch.* A central context is
constructed in each of the two policy variants; the same
chapter-bound secp256k1 derivation request is issued through
the policy-neutral helper; the test asserts (a) both calls
return successfully, (b) the per-policy result is the policy's
expected derived key — single passphrase-derived key for
`Iguana`, BIP-32 child for `GlobalHDAccount`.

**T8.** *Extended-public-key translation.* A known
`xpub`-prefixed extended public key is round-tripped through
the translator into the `ypub` prefix and back; the test
asserts both intermediate and final byte representations match
the public version-byte tables.

**T9.** *Trezor connection-status query — RPC wire contract.* On
a native target, the version-two RPC dispatcher accepts the flat
method `trezor_connection_status`. A central context is
constructed with an initialised Trezor hardware-wallet context
backed by a test double; a request with no `device_pubkey` is
issued and the test asserts the successful response has exactly
the `status` field and that its value is one of the two bound
discriminant strings (`"Connected"` / `"Unreachable"`). A request
issued against a context with no hardware-wallet context asserts
the `TrezorNotInitialized` error token and HTTP 400 mapping.

**T9A.** *Trezor connection-status query — optional device
identity.* A central context is constructed with an initialised
Trezor context whose known device identifier is `D`. A request
that omits `device_pubkey` MUST proceed to status resolution and
MUST NOT produce `FoundUnexpectedDevice`. A request whose
`device_pubkey` equals `D` MUST also proceed to status resolution.
A request whose `device_pubkey` differs from `D` MUST return the
`FoundUnexpectedDevice` error token with HTTP 500 and the test
double MUST observe that no reachability probe was attempted after
the mismatch was detected.

**T9B.** *Trezor connection-status query — concurrent session is
connected.* A test double initialises a Trezor context and then
has a separate in-flight task hold the exclusive device session.
While that session remains owned by the in-flight task, a
`trezor_connection_status` request is issued. The test asserts the
request returns a successful `"Connected"` status without waiting
for the in-flight task to finish, without taking ownership of the
session, and without cancelling or otherwise affecting the
in-flight task.

**T9C.** *Trezor connection-status query — available-session
probe result and observational paths.* With an initialised Trezor
context whose exclusive device session is available, the test
double drives one successful reachability probe and one failed
reachability probe; the test asserts the respective `"Connected"`
and `"Unreachable"` statuses according to the probe result. The
test MUST NOT assert a short RPC-level timeout. Across all cases,
wallet identity, account storage, key material, and
user-confirmation counters exposed by the test harness remain
unchanged; only volatile connection-status bookkeeping may change
after a failed probe.

**T10.** *Software account xpub matches a reference wallet
(dictated interop).* A central context is constructed in the
`GlobalHDAccount` policy from a known BIP-39 mnemonic test vector;
the account-level extended public key is derived in software per
R30 at a known account derivation path (e.g. `m/84'/<coin_type>'/0'`
and `m/44'/<coin_type>'/0'`); the test asserts the serialised
`xpub` (version prefix `0x0488B21E`, base58check) equals,
byte-for-byte, the published `xpub` a reference wallet derives for
that mnemonic at that path. The derivation MUST succeed with no
`trezor_coin` config present.

**T11.** *Software-HD wallet identity is software-derived and
stable.* In `GlobalHDAccount` policy the wallet-identity digest of
R29 is materialised and the test asserts (a) it is produced without
any hardware-wallet handle, (b) it equals the context-wide
`mm2_rmd160` identity, and (c) two constructions from the same
mnemonic yield the identical digest (restart stability). In
`Iguana` policy the HD-storage-identity request is asserted to be
refused.

**T12.** *Source selection by policy.* With a `GlobalHDAccount`
policy and no hardware context, per-coin account extraction is
invoked with no external extractor and the test asserts it resolves
the account xpub via the software derivation of R30; with a
hardware context active, the same extraction is asserted to route
to the device source (R25).

## 5.14 Deferred Work

**D1.** Per-account hierarchical-deterministic activation
flows (the substrate binds the per-curve derivation surface and
the discriminator; the per-coin and per-account activation
plumbing that consumes them is owned by a sibling activation
chapter). *Partially discharged:* the software extended-public-key
source and identity for global-HD accounts are bound in §5.9A
(R29–R31); the UTXO per-account bootstrap, new-address, and scan
behaviour are bound in Chapter 38 §38.8.

**D2.** A second password-hashing scheme alongside Argon2id
(the substrate binds Argon2id as the only password-mode
algorithm in R11; a secondary scheme is deferred to a future
substrate chapter that would extend `KeyDerivationDetails`).

**D3.** A second hardware-wallet integration alongside the
baseline-retained chapter-bound one. The hardware-wallet path
of R25 is the only hardware integration in scope; the broader
hardware-wallet substrate evolution is owned by a sibling
chapter.

**D4.** Native-target MetaMask integration. R26 binds the
integration as WebAssembly-target-only; native parity is
deferred.

**D5.** Mnemonic-export RPC. Chapter 7 explicitly forbids a
mnemonic-export RPC at substrate landing time; lifting that
restriction is deferred and would require coordinated changes
across chapter 7 and this chapter.

## 5.15 Baseline Verifications

**V1.** The baseline tree MUST be confirmed to contain exactly
the nine cryptographic-key source files enumerated as the
baseline shape in R1 and no others. None of the eleven new
modules MUST be present at baseline.

**V2.** The baseline tree MUST be confirmed to expose neither a
BIP-39 mnemonic flow nor an ed25519 derivation surface. The
only baseline key-source path is the freeform passphrase model
underlying the `Iguana` variant of R20.

**V3.** The baseline `Bip44DerivationPath` and its truncated
aliases MUST be confirmed to be a strict BIP-44 path (no
purpose-level generality). The substrate's generic
`StandardHDPath` adds parallel types under R17 and R18; it
MUST NOT replace the strict types in the baseline-exported
surface.

## 5.16 External References

- *BIP-32 — Hierarchical Deterministic Wallets*,
  <https://github.com/bitcoin/bips/blob/master/bip-0032.mediawiki>.
- *BIP-39 — Mnemonic code for generating deterministic keys*,
  <https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki>.
- *BIP-43 — Purpose Field for Deterministic Wallets*,
  <https://github.com/bitcoin/bips/blob/master/bip-0043.mediawiki>.
- *BIP-44 — Multi-Account Hierarchy for Deterministic Wallets*,
  <https://github.com/bitcoin/bips/blob/master/bip-0044.mediawiki>.
- *BIP-49 — Derivation scheme for P2WPKH-nested-in-P2SH
  accounts*,
  <https://github.com/bitcoin/bips/blob/master/bip-0049.mediawiki>.
- *BIP-84 — Derivation scheme for P2WPKH-based accounts*,
  <https://github.com/bitcoin/bips/blob/master/bip-0084.mediawiki>.
- *SLIP-0010 — Universal private key derivation from master
  private key*,
  <https://github.com/satoshilabs/slips/blob/master/slip-0010.md>.
- *SLIP-0021 — Hierarchical derivation of symmetric keys*,
  <https://github.com/satoshilabs/slips/blob/master/slip-0021.md>.
- *SLIP-0044 — Registered coin types for BIP-44*,
  <https://github.com/satoshilabs/slips/blob/master/slip-0044.md>.
- *RFC 9106 — Argon2 Memory-Hard Function for Password Hashing*,
  <https://www.rfc-editor.org/rfc/rfc9106>.
- *RFC 2104 — HMAC: Keyed-Hashing for Message Authentication*,
  <https://www.rfc-editor.org/rfc/rfc2104>.
- *NIST FIPS 197 — Advanced Encryption Standard*,
  <https://csrc.nist.gov/pubs/fips/197/final>.
- *EIP-191 — Signed Data Standard*,
  <https://eips.ethereum.org/EIPS/eip-191>.
- *EIP-712 — Typed structured data hashing and signing*,
  <https://eips.ethereum.org/EIPS/eip-712>.
- *EIP-1193 — Ethereum Provider JavaScript API*,
  <https://eips.ethereum.org/EIPS/eip-1193>.

## 5.17 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit; chapter 01 (clean-room rules); chapter 07 (the
  encrypted-mnemonic envelope this chapter's format payloads);
  chapter 14 (the version-two atomic-swap state machines that
  consume R27); chapter 26 (the WebAssembly-target gating
  pattern used by R26); the public specification documents
  enumerated in 5.16; public crate documentation for the five
  dependency surfaces named in R3.
- *Permitted-input classes used:* baseline source; bound substrate
  identifiers introduced with in-chapter justification; public
  specification documents; public crate documentation.
- *Sibling-allowlist consultations:* none beyond the cross-chapter
  references listed in *Inputs*.
- *Forbidden corpus:* consulted only for the dictated-interop wire
  surface and externally observable status semantics of R28–R28C
  (the `trezor_connection_status` method string, its optional
  `device_pubkey` request field, its `status` response field with
  the `"Connected"` / `"Unreachable"` discriminant strings, the
  method's `error_type` token set with their HTTP-status mapping,
  and the status-query behaviour for optional identity assertions,
  concurrent session ownership, and non-mutating reachability
  probes governed by the underlying transport/device-call
  behaviour); no protected expression crossed.
