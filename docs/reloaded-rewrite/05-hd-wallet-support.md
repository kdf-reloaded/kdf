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
the central context and the platform-gated extensions. R36–R40
(§5.12B) are a Reloaded-original refusal rule for swaps whose keys
do not agree under a global-HD login. R41–R44 (§5.12C) make every
coin announce the key it signs with, so that those keys agree.

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

## 5.9B Key Usage of a Global-HD Login, per Surface (reference behavior)

*(Passed the KDF Dirty Gate on 2026-11-04, measure 0.03. Spec Reader 2026-11-04, rows `v2.6.0-beta` and
`v3.0.0-beta` of chapter 02 §2.15A, identical at both.)* A global-HD login
holds two secp256k1 keys of interest: the **internal key** of R16, at
`m/44'/141'/2147483647/0/0`, and, per activated coin, the **coin key** at the
coin's configured `derivation_path` extended by the activation's
`path_to_address` (default account 0, external chain, address 0). This
section records which of the two the reference uses on each surface, and
what it never uses: **no surface of the reference uses a hash of the
mnemonic string under a global-HD login.** That key exists only in the
legacy single-key (`Iguana`) mode, where it is the one key for everything.

**R32.** *The trading identity is the internal key.* Under a global-HD login
the reference signs maker orders and keep-alive messages, fills
`sender_pubkey` in `TakerRequest`, `MakerReserved`, `TakerConnect` and
`MakerConnected`, marks own orders (`is_mine`), exempts own orders from the
stale sweep (chapter 59 R59.6.5), and derives a relay's own libp2p identity
(chapter 28 R19) from the **internal key** -- the same key `get_public_key`
and `get_public_key_hash` report. Privacy-coin orders keep their per-order
key (chapter 16). The orderbook's `address` for such an order is derived
from the same public key, so for a UTXO or EVM coin it is the internal key's
address and not an address of the coin's HD wallet.

**R33.** *The swap's persistent key is the internal key; the HTLC keys are
the coin keys.* The legacy (v1) swap's persistent public key -- the
`persistent_pubkey` of chapter 51 R59, the own-key field of the start event
and of the swap-statistics record (chapter 51 R50A) -- is the **internal
key**. Each per-coin HTLC public key (chapter 51 R63) is the coin's own: the
**coin key** for a UTXO-family, QTUM, BCH, SLP (its platform's), QRC20, EVM
or Tendermint coin; the per-swap derivation of chapter 39 R39.5.6 for a
shielded coin, whose base key is the coin key; the padded ed25519 account key
of chapter 51 R64 for Sia. Because the HTLC keys then differ from the
persistent key, every HD-login negotiation emits shape 3 of chapter 51 R59
(R60). Payments are sent from, and spends and refunds paid to, the address of
the same coin key that signs them.

**R34.** *No up-front key-mismatch refusal exists, and none is needed.* The
reference has no check that a coin's key "matches" the negotiated one: the
HTLC key it puts on the wire is always the key the coin will sign with, so
the question does not arise. An implementation that puts any other key on
the wire is not at parity and cannot be made so by adding a check; the
remedy is R33.

**R35.** *Per-surface table.* The following holds at both reference rows.

| Surface (global-HD login) | Key or address | Notes |
| --- | --- | --- |
| Order signing, keep-alive, `sender_pubkey`, `is_mine`, stale-sweep exemption | internal key (R32) | per-order key for privacy coins |
| Orderbook `address` of an own order | address of the internal key | shielded outcome for ZHTLC, Sia and Lightning (chapter 32 R-F6b) |
| v1 `persistent_pubkey`, start-event own key, swap statistics | internal key (R33) | |
| v1 HTLC public key: UTXO, QTUM, BCH, SLP, QRC20, EVM, Tendermint | coin key at `path_to_address` | the same key funds the payment, signs, and receives spend and refund |
| v1 HTLC public key: shielded coin | per-swap key from the coin key (chapter 39 R39.5.6) | |
| v1 HTLC public key: Sia | ed25519 key at the single fixed Sia path, padded (chapter 51 R64) | |
| Legacy `enable` / `electrum`: UTXO, QTUM, BCH, QRC20, EVM | coin key at `path_to_address` (default 0 / external / 0); the coin's configured `derivation_path` is required | the coin is built and **registered**, then the response fails at its `address` member, because the single-address accessor is refused under HD; the coin stays enabled (chapter 55 R55.4.10) |
| Legacy `enable`: Tendermint | not routed | mmrpc route only |
| `enable_bch_with_tokens`, `enable_tendermint_with_assets` | coin key at `path_to_address` under the coin's `derivation_path` | Tendermint: a coin configuration without `derivation_path` fails activation |
| `task::enable_sia`, legacy `enable` for Sia | ed25519 key at one fixed single-address path | one address; the reference offers no account or address RPCs for Sia |
| `withdraw` (legacy) and `task::withdraw::init`: UTXO, QTUM, BCH | `from` **required**; the selected address's derived key signs; change returns to it | absent `from`: structured error, sender address not found (chapter 49 R49.4) |
| `withdraw`: SLP token | platform coin key; `from` refused as unsupported | |
| `withdraw`: Sia | the single address; `from` **ignored** | chapter 20 R-W1 |
| `withdraw`: shielded coin | task route only; `from` and `fee` refused as unsupported | chapter 39 R39.5A.2 |
| `withdraw`: EVM | activated address, or the `from` selector | chapter 49 R49.5 |
| `withdraw`: Tendermint | activated key, or the key derived at the `from` selector's path | |
| Legacy `my_balance`, legacy `get_enabled_coins`: UTXO and EVM | **fail** | both read the single-address accessor, refused under HD; `get_enabled_coins` fails for the whole list once one HD UTXO or EVM coin is enabled |
| `my_tx_history` (v2): UTXO, QTUM, BCH, SLP | `target` must name an account or an address | account: every **known** external and internal address of that account; address: that one activated address; the `Iguana` target is refused under HD, and the two HD targets are refused under a single-key login |
| `my_tx_history` (v2): Tendermint | the one account of the activated key; `target` ignored | |
| `my_tx_history` (v2): EVM | not served by the v2 method | legacy history only, from the activated address |
| Shielded history | own method, one account | chapter 39 §39.8 |

> **Compatibility (this repository, 2026-11-04; not corpus-sourced; the
> inventory is plan [`hd-key-unification`](../plans/hd-key-unification.md)).**
> Reloaded departs from R32--R33 under a global-HD login in two ways that are
> **not** parity, although an entry of `RELOADED_VS_GLEEC.md` described the
> first as retained for parity (plan item H0 corrected it on 2026-11-04): (a) the v1 HTLC key falls back to a hash of the mnemonic string
> for every coin that supplies no swap key -- every EVM swap, and every
> UTXO-family coin activated through the legacy route -- and the persistent
> key is that same hash; (b) order signing and `sender_pubkey` use that hash
> while keep-alive, `is_mine` and the stale sweep use the internal key, so
> no keep-alive is ever sent for the key the orders carry. The reference does
> neither. Reloaded's task-route UTXO withdraw without `from` falls back to
> the enabled address where the reference fails (chapter 49 R49.4 note); the
> maintainer kept that divergence on 2026-11-04 (decision BY). Reloaded's
> legacy `enable` under HD binds the internal key where the reference binds
> the coin key and then fails the response; plan item H7 binds the coin key
> and keeps the successful answer behind a switch (decision CB). Plan
> `hd-key-unification` carries the fixes and records the maintainer's
> decisions BY to CC on which surfaces stay documented divergences; R32--R35
> record the reference target for the rest.

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

## 5.12B Swap-Key Agreement under a Global-HD Login

> **Scope (Reloaded-original, 2026-11-04).** Item H1 of plan
> [`hd-key-unification`](../plans/hd-key-unification.md), maintainer
> Decision BL. Under a global-HD login (chapter 45 R45.4.8) the node
> holds three different secp256k1 keys where an iguana login holds
> one: the hash of the passphrase text, which the V1 swap machines
> use as the node's persistent key (chapter 51, the note under R63);
> the internal key at `m/44'/141'/2147483647/0/0`, which a coin
> enabled through legacy `enable` or `electrum` uses; and each coin's
> own HD key. A V1 swap leg whose announced key is not the key its
> coin pays from cannot complete: the other side rejects the dex fee
> or the payment. This section refuses such a leg before any
> negotiation. It is a stop-gap with no counterpart at the reference:
> there the key on the wire is always the key the coin signs with, so
> no such check exists or is needed (§5.9B R34). Plan items H3 and H4
> give every coin its own key and the node the internal key as its
> trading identity, as §5.9B R32--R33 record for the reference, and the
> refusals fall away with them (§5.12C for H3, D6 for H4). Under an iguana login the three
> keys are one key and nothing in this section changes.

**R36.** *The agreement rule.* Under a global-HD login, a coin MAY
take part in a V1 swap only when its **swap key** (the public key the
V1 machines announce for it: the coin's own HTLC key where the coin
supplies one, the node's persistent key otherwise) is its **paying
key**:

1. for a UTXO-family coin (the standard UTXO coin, QTUM, BCH, and the
   tokens on them), the public key of the address its dex fee and swap
   payments are funded from: the address of the key at the activation's
   `path_to_address` when the coin has an HD key policy (R42; until
   §5.12C it was the first address, account `0`, chain `External`,
   address `0`), and its single key otherwise *(amended 2026-11-05,
   §5.12C)*;
2. for an EVM coin or token, the public key of its signer.

The rule does not apply to a coin activated with the Trezor policy, to
a coin that derives a fresh key for every swap (chapter 39 R39.5.9), to
a coin whose native key is not a secp256k1 key (chapter 51 R64), or to
a coin that cannot trade (chapter 60 R60.6.5) or whose swaps are
switched off. Under an iguana login the rule always holds.

> *Consequences (informative).* On dev of 2026-11-04 the rule refuses,
> under a global-HD login: every EVM coin and token (swap key: the
> passphrase hash; paying key: the HD signer, or the internal key after
> legacy `enable`); every UTXO-family coin enabled through legacy
> `enable` or `electrum`, or activated with `priv_key_policy:
> IguanaPrivKey` (paying key: the internal key); and a UTXO coin
> activated through the HD task route at a `path_to_address` other than
> its first address (swap key: the key at `path_to_address`). It accepts
> a UTXO coin activated through the HD task route at its first address,
> and a coin whose single key is the passphrase hash (BCH activated
> together with its tokens). Once §5.12C is built, none of these is
> refused (R44).

**R37.** *Where it is checked.* R36 MUST be checked for both coins of
the pair at every place that refuses an untradable coin (chapter 60
R60.6.5): every order creation (`setprice`, `buy`, `sell`), every
`update_maker_order`, and `trade_preimage` in both its legacy and its
mmrpc 2.0 form. It MUST also be checked before the node answers a
match request with one of its own orders, and before it starts a swap.
A pair that fails is not negotiated: no reservation and no connect
message is sent for it. A swap that has already started is never
stopped by this rule.

**R38.** *Refusal.* A request that fails R36 MUST be refused before any
balance, fee or volume check of the request, with the text

`<coin> cannot be swapped under this login: <reason>`

where `<coin>` is the ticker and `<reason>` is one of:

1. `an EVM coin under an HD login ("enable_hd") pays from another key
   than the one its swaps announce; trade it under a login without
   "enable_hd"`;
2. `it was enabled with a single key (legacy "enable" or "electrum")
   under an HD login, so it pays from another key than the one its
   swaps announce; enable it through its HD activation, or log in
   without "enable_hd"`;
3. `it is activated at <path>, but its swaps pay from <first path>;
   activate it at account 0, external chain, address 0`, with both
   paths written in full.

The legacy RPCs answer this text as their error. `trade_preimage`
reports `error_type` `SwapKeyMismatch` with `error_data`
`{ "coin": <ticker>, "reason": <reason> }` and the same text.
`SwapKeyMismatch` is Reloaded's own discriminant: the reference has no
refusal for this case (§5.9B R34).

**R39.** *An order of the node's own that fails the rule.* When the
check before answering a match request (R37) fails for one of the
node's own maker orders, for example an order restored from storage
after an upgrade, the node MUST send no reservation, MUST cancel the
order as an ordinary cancellation that peers see, and MUST log one WARN
line: `Maker order <uuid> cancelled: <coin> cannot be swapped under
this login: <reason>`.

**R40.** *Switch.* The `MM2.json` boolean `swap_key_check`, default
`true`, enables R36–R39; `false` restores the behavior of earlier
builds, in which such a swap is negotiated and then fails in the coin
layer. Under `docs/COMPAT_SWITCHES.md`, "The default-side rule", this
is arm 2: it has no wire effect, affects only the node's own orders and
swaps, and improves fund safety. The setting is documented next to
itself and in `docs/GLEEC_COMPATIBILITY.md`.

> **Compatibility.** GLEEC KDF has no such check and needs none: its
> swaps announce the key the coin signs with (§5.9B R33--R34), so the
> pairs refused here trade normally there. Setting `swap_key_check` to
> `false` does not reach that behavior; it restores the behavior of
> earlier Reloaded builds, in which such a swap is negotiated and then
> fails in the coin layer. Item H3 (§5.12C) removes the pairs that this
> section refuses, and the divergence ends when item H4 lands (D6).

> **As built (2026-11-04, clean-side record; no requirement above changes).**
> *Coin side.* `SwapOps::swap_key_disagreement` (`coins/lp_coins_traits.rs`) answers
> `None` when a coin's swap key is its paying key and for a coin the rule does not
> apply to (the default, which covers ZHTLC, Sia, Tendermint, Solana and Lightning).
> UTXO-family coins (standard, QTUM, BCH, QRC20, SLP tokens) share
> `utxo_common::swap_key_disagreement`: a single key is compared with the node's
> persistent key; a coin with an HD key policy is compared by path, its
> `path_to_address` (now kept in `PrivKeyPolicy::HDWallet`) against account `0`,
> `External`, address `0`, which is the address `active_utxo_sender_address` funds
> from; the Trezor policy is out of scope. EVM coins compare the compressed form of
> the signer's public key with the node's persistent key (`eth_swap_ops.rs`); a
> hardware signer is out of scope. *Node side.* `swap_key_mismatch` and
> `swap_key_check_enabled` (`mm2_main/src/lp_ordermatch/ordermatch_types.rs`, next to
> `untradable_coin_ticker`) apply the rule under a global-HD login and skip a
> wallet-only coin. They are asked in `buy`, `sell`, `create_maker_order`,
> `update_maker_order` (`ordermatch_trading.rs`) and in `maker_swap_trade_preimage`
> and `taker_swap_trade_preimage`, after the untradable-coin refusal and before any
> balance check; in `process_taker_request` (R39: `RemovedMakerOrder::take_out`, the
> ordinary cancellation notice, then deletion with the `Cancelled` reason, and the
> WARN line built by `swap_key_cancel_line`); in `process_maker_reserved` (a
> reservation of a failing pair is skipped, no connect message); and at the start of
> `lp_connect_start_bob` and `lp_connected_alice`, before the swap object is built.
> The resume path of a saved swap does not ask. `TradePreimageRpcError::SwapKeyMismatch`
> carries `{ coin, reason }`. The order builders' own `build` methods have no node
> context and are not changed. A legacy error carries the source location that every
> other legacy refusal carries. *Tests.* T15: `coins`
> `eth_tests::swap_key_agreement_tests`,
> `ordermatch_tests::t05_12b_swap_key_check_tests::{the_refusal_text_is_the_one_of_r38,
> t15_order_creation_and_update_are_refused}`, `trade_preimage::tests::swap_key_mismatch`;
> T16: `coins` `utxo_tests::swap_key_agreement_tests`; T17:
> `t17_an_own_order_that_fails_is_cancelled_at_the_match_check`,
> `a_connect_message_is_not_sent_for_a_pair_that_fails`,
> `the_cancel_line_is_the_text_of_r39`; T18: `the_switch_false_disables_the_check`,
> `an_iguana_login_is_never_refused`, `a_wallet_only_coin_is_exempt`, and the Trezor
> and single-key cases in `coins`. Not covered by a unit test: the exemption of a
> ZHTLC and a Sia coin (no override of the default, and no coin of either family can
> be built without a node) and the refusal at swap start; the SwapLab row HD2 of the
> plan covers the end-to-end behavior.

## 5.12C One Swap Key per Coin

> **Scope (Reloaded-original, 2026-11-05).** This section is item H3 of plan
> [`hd-key-unification`](../plans/hd-key-unification.md), under maintainer
> Decision BL. It brings Reloaded to the HTLC half of §5.9B R33 and
> chapter 51 R74:
> - every coin announces on the V1 wire the key it signs its swap
>   transactions with;
> - a UTXO-family coin funds its swap, takes its change, and is paid its spend
>   and refund on that key's address.
>
> The node's persistent key, the `persistent_pubkey` of chapter 51 R59, does
> not change here. It moves to the internal key with item H4 (D6). Until then
> it stays the passphrase hash, and a negotiation whose HTLC keys differ from
> it emits shape 3 of chapter 51 R60.
> Under an iguana login every key named in this section is the node's one
> key, and nothing changes: not the wire, and not what a swap saves.

**R41.** *The swap key is the signing key.* For a V1 swap the node MUST
announce, as each coin's HTLC public key (chapter 51 R63), the public key of
the key that coin signs its swap transactions with (its **signing key**):

1. a UTXO-family coin (the standard UTXO coin, QTUM, BCH, QRC20, and SLP
   tokens through their platform coin) with an HD key policy: the key at the
   activation's `path_to_address` (account, chain, address index; default
   account `0`, chain `External`, address `0`);
2. a UTXO-family coin with a single key: that key. This includes a coin
   enabled through legacy `enable` or `electrum`, or with `priv_key_policy:
   IguanaPrivKey`, under a global-HD login, where the key is today the
   internal key; item H7 changes which key such a coin binds, and this rule
   follows it;
3. an EVM coin or token with a software signer: the signer's key, in
   compressed form;
4. a Tendermint coin or token: its activated key (unchanged; item H7 changes
   which key that is);
5. a shielded coin: the per-swap key of chapter 39 R39.5.6 (unchanged). Sia:
   chapter 51 R64 (unchanged).

A coin activated with the Trezor policy, and an EVM coin whose signer is a
hardware or external signer, are outside this section and keep their
present behavior.

**R42.** *One address per UTXO-family swap leg.* A UTXO-family coin with an
HD key policy MUST meet all of the following for every transaction of a V1
swap leg that it funds (the dex fee and the payment):

- it spends only outputs of the address of its signing key (R41 item 1);
- it returns the change to that address;
- it pays its spend and refund outputs to that address.

The balance a swap checks before it starts MUST use the same address, and so
MUST the fee estimate of `trade_preimage`. The balance checks are those of
order creation, `max_taker_vol`, `max_maker_vol`, and the checks at swap
start.

As built before this section, the balance check and the HTLC key already used
`path_to_address`, while funding and the fee estimate used the first address
(account `0`, chain `External`, address `0`). This rule moves funding and the
estimate. At the default `path_to_address` nothing changes. The V2 swap path
uses the same address: the "currently enabled HD address" of chapter 15 R36
is the address at `path_to_address`, for both the V2 HTLC key and the V2
funding.

**R43.** *What a swap saves, and resumption.*
- *At swap start.* A coin whose signing key is not the node's persistent key
  MUST have its key pair saved with the swap, as an HD UTXO coin's is saved
  today (the maker and taker swaps' saved HTLC key pairs). A coin whose
  signing key is the persistent key saves nothing, as today.
- *On resume.* A resumed swap MUST use the key pair saved with it. A swap
  saved without one, started by an earlier build, MUST keep using the
  node's persistent key, which is the key its Started event recorded.
- *Every later use.* The saved pair is the one every later use of the
  swap's HTLC key takes: the running swap, its resumption after a
  restart, and the manual recovery of its funds (`recover_funds_of_swap`,
  with the key candidates of chapter 39 R39.5.4). A later change of the
  coin's activation or derivation settings does not change it. For an
  EVM coin this storage and these recovery semantics are the same as for
  a UTXO coin; only Sia announces its key without saving a pair (chapter
  51 R64).

No step of the plan changes the key of a swap that has already started.

**R44.** *The agreement rule after this section.* With R41 and R42 built, R36
holds for every coin it applies to, under every login, and none of R38's
reasons 1 to 3 occurs any more. §5.12B stays in force unchanged as a guard:
a refusal under R38 after this section is a defect to report, not an
expected answer. The switch `swap_key_check` (R40) stays, with its default.

> **Compatibility.** With this section Reloaded's V1 HTLC keys and its
> UTXO-family funding address match the reference (§5.9B R33, R35; chapter 51
> R74). Two differences to the reference remain, and both go with item H4:
> the persistent key, which is still the passphrase hash under a global-HD
> login, and the trading identity of §5.9B R32. A swap between a Reloaded
> node built with this section and an earlier Reloaded build negotiates as
> any shape-3 swap does; the earlier build's own HD swaps still fail as they
> did.

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

**T13.** *HD trading identity (R32, R33).* Under a global-HD login, a
posted maker order's public key, the keep-alive's public key, the
`sender_pubkey` of a taker request and the v1 swap's persistent public key
shall all equal the internal key of R16, and none shall equal a key derived
by hashing the mnemonic string.

**T14.** *HD HTLC key is the coin key (R33).* Under a global-HD login with a
non-default `path_to_address`, a UTXO and an EVM v1 swap shall each put the
coin key at that path on the wire as the HTLC public key, fund the payment
from that key's address, and pay the spend or refund to it.

**T15.** *EVM under a global-HD login.* With an EVM coin activated
through `enable_eth_with_tokens`, and again through legacy `enable`,
`setprice`, `buy`, `sell`, `update_maker_order` and `trade_preimage`
(both forms) for a pair with that coin are refused with reason 1 of
R38 before any balance check, and `trade_preimage` carries
`SwapKeyMismatch`. The same requests under an iguana login pass this
check.

**T16.** *UTXO under a global-HD login.* RIN enabled through legacy
`electrum` is refused with reason 2. RIN activated through
`task::enable_utxo` at its first address passes this check. RIN
activated with `path_to_address` at account `0`, chain `External`,
address `1` is refused with reason 3, which names both full paths.

**T17.** *Own order that fails the rule.* A maker order for a pair that
fails R36, present in storage at start-up: a matching request gets no
reservation, the order is cancelled once with the WARN line of R39, and
no swap starts. A swap of such a pair that started before the restart is
resumed and not stopped.

**T18.** *Switch and exemptions.* With `swap_key_check` set to `false`
the requests of T15 and T16 pass this check. With the default, a ZHTLC
coin, a Sia coin, a coin activated with the Trezor policy, and BCH
activated together with its tokens are not refused by it.


**T19.** *EVM signing key on the wire (R41 item 3).* Under a global-HD login,
an EVM coin activated with a software HD signer at a non-default
`path_to_address` announces the compressed public key of its signer as its V1
HTLC public key. The swap saves that key pair, and R36 holds for the coin.

**T20.** *One address for an HD UTXO leg (R41 item 1, R42).* RIN is activated
through `task::enable_utxo` at account `0`, chain `External`, address `1`.
- Its V1 HTLC public key is the key of that address.
- The dex fee and the payment spend only outputs of that address and return
  their change to it.
- Its spend and refund outputs pay to that address.
- `trade_preimage` estimates the fee from that address's outputs.
- The V2 HTLC public key is the key of the same address.

At the default `path_to_address`, every one of these is the first address,
as before.

**T21.** *Single-key UTXO coin under a global-HD login (R41 item 2).* RIN
enabled through legacy `electrum` under a global-HD login:
- it announces its own key as the V1 HTLC public key;
- it funds from that key's address;
- it pays its spend and refund to that address;
- the swap saves the key pair.

**T22.** *Iguana login unchanged (R41, R43).* Under an iguana login, RIN and
an EVM coin announce the node's persistent key, and a swap saves no HTLC key
pair for either. The negotiation message and the saved swap are the same as
those of the previous build.

**T23.** *Resumption (R43).*
- A swap saved by the previous build without an HTLC key pair resumes with
  the node's persistent key, which is the key in its Started event.
- A swap saved with a pair resumes with that pair.
- In both cases the refund of the node's own payment uses the key that the
  Started event recorded.

**T24.** *The guard after §5.12C (R44).* Under a global-HD login with the
default `swap_key_check`, none of the requests of T15 and T16 is refused.
`trade_preimage` answers for them without `SwapKeyMismatch`.

## 5.14 Deferred Work

**D1.** Per-account hierarchical-deterministic activation
flows (the substrate binds the per-curve derivation surface and
the discriminator; the per-coin and per-account activation
plumbing that consumes them is owned by a sibling activation
chapter). *Partially discharged:* the software extended-public-key
source and identity for global-HD accounts are bound in §5.9A
(R29–R31); the UTXO per-account bootstrap, new-address, and scan
behaviour are bound in Chapter 38 §38.8. Chapter 20 binds a second,
structurally distinct activation-plumbing consumer: a non-UTXO,
SLIP-0010 ed25519 coin family (Sia) whose HD account and wallet
types consume this chapter's R15 `derive_ed25519_signing_key`
helper and the `GlobalHDAccount` policy (R20), but do not and
cannot consume the secp256k1 extended-public-key path of §5.9A
(R29–R31) — SLIP-0010, unlike BIP-32, defines no public-key-only
(non-hardened) child derivation, so there is no ed25519 analogue of
an `xpub`. This is a structural property of the curve, not a gap
in this chapter's derivation surface: an ed25519 HD account
necessarily carries the account-level *private* signing key rather
than an extended public key. §5.9A's scope note ("Bitcoin-family
(UTXO) coins") already excludes this case; Chapter 20 §20.10 D1
binds the ed25519-specific account type and its activation wiring.

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

**D5.** *(Superseded.)* Mnemonic-export RPC. An earlier draft of
this chapter deferred a mnemonic-export RPC because Chapter 7
forbade one at substrate landing time. Chapter 7 §7.3A now binds a
wallet-password-gated own-seed export RPC (`get_mnemonic`,
Chapter 7 R-K7) and a password re-encryption RPC
(`change_mnemonic_password`, Chapter 7 R-K8) as first-class,
non-deferred capabilities under a default-off / opt-in posture that
applies only to the wider bulk/offline/HD/ZHTLC export superset
(Chapter 7 R-K1–R-K4). Both RPCs decrypt and re-encrypt through this
chapter's mnemonic module and encryption envelope (§5.4–§5.5) but
are themselves bound, gated and tested entirely within Chapter 7; no
further coordinated change to this chapter is required.

**D6.** *One trading identity under a global-HD login* (plan
[`hd-key-unification`](../plans/hd-key-unification.md), item H4).
§5.12B refuses the swaps whose keys do not agree, and §5.12C (item H3)
makes them agree. Item H4 moves the trading identity and the swap's
persistent key from the passphrase hash to the internal key (§5.9B R32,
R33). It MUST keep the passphrase-hash key for a swap saved without an
HTLC key pair (R43), because that is the key such a swap recorded. Its
rules will be added to this chapter with that item.

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
  encrypted-mnemonic envelope this chapter's format payloads, and
  the now-bound `get_mnemonic` / `change_mnemonic_password` export
  RPCs referenced in the corrected D5); chapter 14 (the version-two
  atomic-swap state machines that consume R27); chapter 20 (the Sia
  ed25519 hierarchical-deterministic account, a second D1 activation
  consumer of R15's `derive_ed25519_signing_key`, cross-referenced in
  D1); chapter 26 (the WebAssembly-target gating pattern used by
  R26); the public specification documents enumerated in 5.16;
  public crate documentation for the five dependency surfaces named
  in R3.
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
- *Scoped addition (2026-11-04) -- §5.9B (R32--R35), T13, T14 and the
  compatibility note under R35.* **KDF Dirty Gate: PASS on 2026-11-04, measure 0.03.** Prompted by the
  HD-gaps inventory of the 0.4.0 cycle. *Inputs:* the forbidden corpus at the
  rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at
  both, consulted **only** to establish which key a global-HD login uses on
  each surface of the table: the trading identity and the orderbook address;
  the v1 persistent key and each family's HTLC key; what the legacy
  activation methods bind and how their response ends; the keys of the two
  platform-with-tokens activations and of Sia; the sender rules of the
  withdraw routes per family; the failure of the two legacy address-reading
  methods; and the history method's `target` handling per family. What
  crosses is behavior, public RPC names and dictated request members
  (`path_to_address`, `from`, `target`, `derivation_path`); no private
  identifier, function body, helper decomposition, error, log or comment text
  was reproduced. The compatibility note rests on this repository's own code
  and inventory; the T-items are Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04, measure
  0.03, no findings. *Extent:* the HD key policy under a global-HD login and
  its trading, swap, legacy-activation, withdraw and history surfaces, at
  `v3.0.0-beta` with `v2.6.0-beta` as cross-check; for the whole batch, 30
  source files and one dependency manifest were read at `v3.0.0-beta`, 29 of
  them checked by hash against the tagged states of both rows. *Method:*
  side-by-side reading of the compared regions against every added or changed line,
  plus that hash check; the gate script was not run because the candidate has
  no code blocks. *Measure:* 0.03, read against the standard that any
  discretionary finding fails a file; dictated-interop and public-surface
  tokens were excluded from the measured set.
- *Reloaded-original addition (2026-11-04) -- §5.12B (R36–R40), T15–T18,
  D6 and the last sentence of §5.1.* Written by a context that never
  read the forbidden corpus, from maintainer Decision BL and this
  repository's own swap, ordermatch, coin-key and activation code. The
  sentences that describe the reference restate §5.9B R32--R34, which
  passed the KDF Dirty Gate on 2026-11-04; the addition carries no
  reference content of its own and needs no Dirty Gate pass of its
  own. *(Renumbered on 2026-11-04 from R32–R36 and
  T13–T16, which §5.9B took.)*
- *Clean-side amendment (2026-11-05) -- the compatibility note under R35.*
  Its last sentences now record the maintainer's decisions BY and CB of
  2026-11-04 (plan `hd-key-unification`), and that plan item H0 corrected the
  parity claim. Written by a session that never read the corpus, from those
  decisions and this repository's own code. The reference facts of §5.9B are
  unchanged. It takes no forbidden-class input and needs no Dirty Gate pass.
- *Reloaded-original addition (2026-11-05) -- §5.12C (R41–R44), T19–T24,
  D6 as rewritten, the amendment of R36 item 1, and the pointers in §5.1
  and §5.12B.* Written by a context that never read the forbidden corpus.
  It rests on maintainer Decision BL and on this repository's own swap
  key, UTXO funding, balance, trade-preimage and activation code. The
  sentences that describe the reference restate §5.9B R33 and R35 and
  chapter 51 R74, which passed the KDF Dirty Gate on 2026-11-04. The
  addition carries no reference content of its own and needs no Dirty
  Gate pass of its own.
