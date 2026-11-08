# Chapter 05 — Hierarchical-Deterministic Wallet Support

**Status:** driving-spec. §5.10B (R50–R58, T31–T36, D7) and the 2026-11-08
amendments of R29, R31 and T12 **passed the KDF Dirty Gate on 2026-11-08,
measure 0.02** (see §5.17 for the extent and method).

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
coin announce the key it signs with, so that those keys agree. R45–R49
(§5.12D) make the internal key the node's own key under a global-HD
login.

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
Iguana mode); a coin activated under the hardware (`Trezor`) key
policy is namespaced by the hardware device's own identity digest
(R25, §5.10B R51/R55), whichever software login holds the context,
and the mere presence of a hardware context does not change the
digest of a context-key coin (§5.10B R52).
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
extraction MUST be chosen by the **coin's own key policy, fixed at
its activation** (§5.10B R52), not by a fixed assumption of hardware
and not by whether a hardware-wallet context happens to exist:

  - when the coin was activated under the hardware (`Trezor`) key
    policy, the source is the hardware device (the device extractor
    of R25, which requires a hardware-wallet context and the coin's
    `trezor_coin` config, and performs the device protocol);
  - when the coin was activated under the context key policy of a
    `GlobalHDAccount` login, the source is the **in-context software
    derivation of R30** (the in-memory master), requiring no
    `trezor_coin` field — whether or not a hardware-wallet context
    exists at the time (§5.10B R52, T32). *(Amended 2026-11-08;
    passed the KDF Dirty Gate with §5.10B on 2026-11-08.)*

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
> Until 2026-11-05, Reloaded departed from R32--R33 under a global-HD login in
> two ways that were **not** parity, although an entry of `RELOADED_VS_GLEEC.md` described the
> first as retained for parity (plan item H0 corrected it on 2026-11-04): (a) the v1 HTLC key fell back to a hash of the mnemonic string
> for every coin that supplied no swap key -- every EVM swap, and every
> UTXO-family coin activated through the legacy route -- and the persistent
> key was that same hash; (b) order signing and `sender_pubkey` used that hash
> while keep-alive, `is_mine` and the stale sweep used the internal key, so
> no keep-alive was ever sent for the key the orders carried. The reference does
> neither. Plan items H3 (§5.12C, PR #250) and H4 (§5.12D, PR #254), both
> merged 2026-11-05, closed (a) and (b) (D6). When `from` is absent, Reloaded's UTXO withdraw uses the coin's
> activated address, the address at `path_to_address`, on the task route and,
> since plan item H5, on the direct method. The reference fails such a
> withdrawal (chapter 49 R49.4 note). The maintainer kept that divergence on
> 2026-11-04 (decision BY) and set the address on 2026-11-06 (decision CU).
> Reloaded's legacy `enable` under HD binds the internal key where the
> reference binds the coin key and then fails the response; plan item H7 is to
> bind the coin key and keep the successful answer behind a switch (decision
> CB, 2026-11-04; decided, not implemented yet).
> Plan `hd-key-unification` carries the fixes and records the maintainer's
> decisions BY to CC on which surfaces stay documented divergences; R32--R35
> record the reference target for the rest.

**R35a.** *Activation under a global-HD login lists the address at
`path_to_address` from the first activation on.* *(Spec Reader 2026-11-07,
rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both;
**passed the KDF Dirty Gate on 2026-11-07**, measure 0.04.)* At both rows, the HD activation of a UTXO-family, QTUM, BCH
or EVM coin -- task route and platform-with-tokens route alike -- handles the
address named by `path_to_address` (default account `0`, chain `External`,
address `0`) as follows:

1. *Account.* If the wallet's storage holds no account with the path's
   `account_id`, that account is created at activation, with the path's
   `account_id` as its index (the limit of R35c item 1 applies), and it is
   scanned when `scan_policy` is `scan_if_new_wallet` or `scan`; the
   activation result then reports **that account only**, even when the
   wallet's storage holds others. When the path's account already exists,
   every stored account is reported, and each is scanned only under `scan`.
2. *Known-address count.* For the path's account, the number of known
   addresses on the path's chain is raised to at least `address_id + 1`,
   exactly as `min_addresses_number` raises it (chapter 38 R38.8.5); the
   effective minimum is the larger of the two. Every other account gets
   `min_addresses_number` alone. The addresses derived to meet the minimum
   are persisted as known and their balances read, so the address at
   `path_to_address` is listed -- with its `derivation_path`, `chain` and
   balance -- in the activation result, and afterwards in `account_balance`
   and in the account target of `my_tx_history` (R35), from the first
   activation and after every restart, whether or not `get_new_address` ever
   handed it out. The wallet's *enabled address* (the swap and consolidation
   address, R33, R35b) is answered only from the derived set, so this step is
   also what makes it resolvable.
3. *No gap-limit bound on the index.* The gap limit governs scanning only.
   The one bound on `address_id` is the hardened-child limit (2^31
   addresses per chain): a `path_to_address` at or beyond it fails
   activation with the address-limit-reached arm of the balance error set.
4. *Nothing else makes an address known.* A swap uses the enabled address,
   which item 2 has already listed. A withdraw with `from` requires the
   selected account to exist and the selected address to be already known
   (chapter 49 R49.27b); it derives nothing new and lists nothing.
5. *Hardware wallet.* Items 1--3 hold the same; in addition, once the enabled
   address is derived, the coin is handed it, and the activation fails if it
   cannot be resolved.

> **Compatibility (this repository, 2026-11-07; not corpus-sourced; plan
> [`hd-key-unification`](../plans/hd-key-unification.md) item H5.0).**
> Until PR #279 (merged 2026-11-07), Reloaded raised the known-address count
> for `min_addresses_number` only (chapter 38 R38.8.5) and listed only the
> addresses already handed out, so a swap's funds at a never-requested
> `path_to_address` stayed unlisted until `get_new_address` reached it
> (SwapLab row HD5, 2026-11-06). Item 2 is the parity target; it is the option
> the plan's preparation note recommends. PR #279 built it; the as-built note
> below records how, and its known gap.

> **As built (2026-11-07, clean-side record; plan item H5.0).**
> `enable_hd_wallet` takes the activated address beside
> `min_addresses_number`. A software global-HD activation of a UTXO coin, QTUM
> or an EVM coin passes the address at `path_to_address`; the Trezor policy of
> a UTXO coin passes none and behaves as before. When the wallet holds no
> account at the path's `account_id`, activation creates it (the limit of R35c
> item 1 applies), scans it under `scan_if_new_wallet` or `scan`, and reports
> that account only; otherwise every stored account is reported and each is
> scanned only under `scan`. The path's account gets its chain raised to at
> least `address_id + 1` (External takes the larger of that and
> `min_addresses_number`); every other account gets `min_addresses_number`
> alone. The new addresses are persisted, their balances read, and they are
> listed in the result and afterwards in `account_balance` and
> `my_tx_history`. Known gap against item 3: an `address_id` at the hardened
> limit fails activation with the `Internal` arm, because Reloaded's balance
> error set has no address-limit arm yet (a follow-up); the account created
> for the path stays stored after such a failure. An EVM coin has no internal
> chain, so an Internal-chain path raises nothing there. Tests: the `t14a_*`
> tests in `coins/src/utxo/utxo_tests.rs` (module
> `hd_accounts_at_index_tests`) and in `coins_activation`'s
> `eth_with_tokens_activation` tests (module `hd_accounts_at_index`),
> `t14a_the_path_raises_its_own_chain`, and `new_account_index_tests`.

**R35b.** *UTXO maintenance commands under a global-HD login.* *(Same
provenance and gate status as R35a.)* At both rows the two commands below
serve a UTXO coin of the standard family only; for every other coin type
(QTUM, BCH, tokens, EVM and the rest included) they answer
`CoinNotSupported`, HTTP 400.

- `fetch_utxos` (request `{coin}`). Under HD it lists the unspent outputs of
  **every known address of every account** of the wallet, on both chains;
  under a single-address login, of the one address. Response: `total_count`
  and `addresses`, a list of `{address, count, utxos, derivation_path?}` whose
  `utxos` is a list of `{txid, vout, value}`; `derivation_path` is present for
  an HD address and absent for the single address. Errors (`error_type`, HTTP): `NoSuchCoin`
  404, `CoinNotSupported` 400, `InvalidAddress` 400, `Internal` 500.
- `consolidate_utxos` (request `{coin, merge_conditions?: {merge_at,
  max_merge_at_once}, broadcast?}`; `merge_at` and `max_merge_at_once`
  default to 50 each, `broadcast` to `false`). Under HD it merges the unspent
  outputs of the **enabled address only** into one output paid to **that same
  address**; it offers no account or address selector and never touches
  another known address. Fewer than `merge_at` unspent outputs, or fewer than
  two once the `max_merge_at_once` cut is applied, answer
  `BadMergeConditions` 400; an enabled address that cannot be resolved
  (R35a item 2 not yet satisfied -- a hardware wallet before derivation)
  answers `InvalidAddress` 400; the others are `NoSuchCoin` 404,
  `CoinNotSupported` 400 and `InternalError` 500. Response: `tx` and
  `consolidated_utxos` (a list of `{txid, vout, value}`), where `tx` is the
  transaction-details object with `from` and `to` both the enabled address
  and `fee_details` of the UTXO fee type; with `broadcast: false` the
  transaction is built and returned but not sent (chapter 38 R38.5.2).
- The automatic merge loop configured by `utxo_merge_params` (chapter 55
  R55.5.10) runs under HD on the enabled address only, with the same
  destination. If the enabled address cannot be resolved when the loop
  starts, the loop ends with a logged error and no merge runs for the coin's
  lifetime.

> **Compatibility (this repository, 2026-11-07; not corpus-sourced).**
> Until PR #281 (merged 2026-11-08), Reloaded refused both commands and left
> the automatic merge off under HD (plan `hd-key-unification` item H5.3). The
> behavior above is the parity target; PR #281 built it (as-built note
> below).

> **As built (2026-11-07, clean-side record; plan item H5.3).** Reloaded
> serves both commands and the automatic merge under a software global-HD
> login. `consolidate_utxos` goes through
> `utxo_common::merge_utxos_at_enabled_address`: for an HD coin the enabled
> address comes from `enabled_hd_address_info` (the address at
> `path_to_address`), and the merge is signed with the key derived at that
> address's path, through `generate_and_send_tx_from_sender`, or through
> `active_sender_key_pair` when `broadcast` is `false`. `fetch_utxos` reads
> every known address of every account on both chains, derives nothing beyond
> the known counts, and leaves out addresses with no outputs, as for a single
> address. `merge_utxo_loop` resolves the enabled address on each round and
> ends with a logged error if it cannot; the loop is shared, so QTUM under HD
> merges its enabled address too. Under the Trezor policy `consolidate_utxos`
> answers `InvalidAddress` and the loop ends, so no device is asked to sign;
> `fetch_utxos` reads such a wallet from its account public keys. Tests: the
> `t14b_*` and `r35b_*` tests in `rpc_command/utxo_hd_maintenance_tests.rs`.

**R35c.** *`task::create_new_account` under a software global-HD login.*
*(Same provenance and gate status as R35a.)* At both rows the task is served
for UTXO-family, QTUM and EVM coins activated in HD mode under a software
login: no hardware device is sought, and the new account's extended public
key is derived in software (§5.9A). Under a hardware login the device is
asked for it, with the progress values of item 3. Any other coin type, or a
coin not activated in HD mode, answers `CoinIsActivatedNotWithHDWallet`.

1. *Index.* The requested `account_id` when given; otherwise the lowest
   index not in use, counted from `0` up to the highest existing index, or
   that index plus one. The limit is 2^31 accounts: an index at or beyond it
   answers `AccountLimitReached` with `max_accounts_number`. A requested
   index that already exists is refused only after the key derivation, with
   the `Internal` arm.
2. *Scan and result.* The account is persisted first. Then, with `scan`
   (default `true`), it is scanned for used addresses up to `gap_limit` (the
   request's value, else the wallet's, default 20; chapter 38 R38.8.4) and
   the found addresses are listed with their balances; with `scan: false` it
   is returned with no addresses. The result is the account object of the
   activation result: `{account_index, derivation_path, total_balance,
   addresses: [{address, derivation_path, chain, balance}]}`.
3. *Progress.* In-progress values `Preparing`, `RequestingAccountBalance`,
   `Finishing`, and, for a hardware wallet, `WaitingForTrezorToConnect` and
   `FollowHwDeviceInstructions`; awaiting values `EnterTrezorPin` and
   `EnterTrezorPassphrase`.
4. *Cancel.* Cancelling after the account was created removes it again if it
   is the wallet's last account.
5. *Error set* (`error_type`, HTTP): `HwContextNotInitialized` 400,
   `NoSuchCoin` 400, `UnexpectedUserAction` 400, `Timeout` 408,
   `CoinIsActivatedNotWithHDWallet` 400, `InvalidBip44Chain` 400,
   `AccountLimitReached` 400, `RpcInvalidResponse` 500, `WalletStorageError`
   500, `HwError` 410, `Transport` 500, `Internal` 500.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort while reading item 1).** A caller that asks for an `account_id`
> already in use receives the `Internal` arm at the reference, although the
> condition is a request error: the collision is detected only when the
> freshly derived account is inserted. Reloaded should answer it with a
> request-class error (HTTP 400) before deriving anything; the discriminant is
> a Reloaded choice, since the reference has none for the case.

> **Compatibility (this repository, 2026-11-07; not corpus-sourced).**
> Until PR #279 (merged 2026-11-07), Reloaded's task asked for a hardware
> device before anything else, so it failed for every software-keyed coin
> (chapter 38 §38.8 status note; chapter 20 §20.10 D1 note; plan
> `hd-key-unification` item H5.4). The behavior above is the parity target;
> PR #279 built it, with the differences the as-built note below lists.

> **As built (2026-11-07, clean-side record; plan item H5.4).**
> `task::create_new_account` derives the account key in software under a
> software global-HD login, for UTXO, QTUM, EVM and Siacoin. The request gained an optional
> `account_id`. `choose_new_account_id` picks the index once, before any
> derivation: the requested index, or the lowest unused one; an index at or
> beyond 2^31 answers `AccountLimitReached`; an index in use answers
> `AccountAlreadyExists` with `account_id`, an error type that maps to HTTP 400
> (the code-quality finding above; a documented divergence). Like every error
> of the task, these are read from `task::create_new_account::status`; `init`
> answers with a task id. A coin not in HD mode answers
> `CoinIsActivatedNotWithHDWallet`. `task::create_new_account::cancel` is
> routed, and the task's cleanup removes the account it created from the
> wallet and its storage when it is the wallet's last account, the one with
> the highest number (an account created while a higher-numbered one exists
> stays); a task dropped
> between storing the account and recording it can leave the stored row.
> Differences from items 3 and 5: under a hardware wallet Reloaded reports
> `WaitingForUserToConfirmPubkey` where item 3 lists
> `FollowHwDeviceInstructions`; under a software login the task reports
> `Preparing` and then its result, without `RequestingAccountBalance` or
> `Finishing`; the error set has no `UnexpectedUserAction` or `HwError`
> discriminant, and `Timeout` answers HTTP 500. Tests: the `t14c_*` and
> `r35c_item4_*` tests and `new_account_index_tests`.
>
> *Update (2026-11-08, batch 4 item B4-2).* The source of an account's
> extended public key follows the coin's own key policy, not whether a
> hardware context exists. `task::init_trezor` is accepted after a software
> global-HD login and leaves a hardware context beside it; until this update
> a UTXO or QTUM coin of the software policy that names a `trezor_coin` then
> took the extended public key of a new account, in `create_new_account` and
> in activation, from the device, and Siacoin activation failed with
> `CoinDoesntSupportTrezor`. Now a coin of the software policy (UTXO, QTUM,
> EVM, Siacoin) derives the key in software and never asks a device; a coin of
> the Trezor policy asks the device, and without a hardware context it
> reports `HwContextNotInitialized`. The choice is made in
> `utxo_common::extract_extended_pubkey` and, for the UTXO activation, by
> `reads_keys_from_device`; Siacoin ignores an offered device; EVM already
> chose by its signer. Tests: the `r35c_*` and `r35a_*` tests with a device
> offered in `coins/src/utxo/utxo_tests.rs` (module
> `hd_accounts_at_index_tests`), and `r35a_*` in `coins_activation`'s
> `utxo_activation/tests.rs`.

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

## 5.10B Hardware Context next to a Software Login (reference behavior)

> **Clearance (chapter 01 R40) — passed on 2026-11-08.** This section, the 2026-11-08
> amendments of R29, R31 and T12 that re-key three sentences on the coin's
> own key policy, the matching sentence of chapter 38 R38.8.1 and the row
> of chapter 30 §30.3 were added on 2026-11-08 by the KDF Spec Reader under
> the two-team procedure of chapter 01 §1.4A, from forbidden-class input:
> the reference lineage at the rows `v2.6.0-beta` and `v3.0.0-beta` of
> chapter 02 §2.15A (identical for every fact taken), with the row
> *baseline anchor (R1)* for the anchor comparison. They **passed the KDF
> Dirty Gate on 2026-11-08**, measure 0.02; the attestation is in §5.17. The
> clearances of §5.9B, §5.10A and the rest of this chapter are not
> affected.

*Vocabulary.* This chapter and §5.9B speak of a "hardware login" or a
"hardware wallet" as a mode. The reference has no such login: a daemon
always starts from a software login — the legacy passphrase (`Iguana`) or
the mnemonic-derived global-HD account (`GlobalHDAccount`, R20), chosen by
`enable_hd` (chapter 45 R45.4.8) — or in no-login mode, and a hardware
(Trezor) context is an *optional slot* of the central cryptographic context
that a running daemon fills afterwards (`task::init_trezor`, chapter 55
§55.11). The device is then used **per coin**, by the coin's own key
policy, never by the daemon as a whole. Where §5.9B says "under a hardware
login" or "under a hardware wallet", read "for a coin activated under the
`Trezor` key policy while a hardware context exists"; where it says "under
a software login", read "for a coin activated under the context key
policy", whether or not a hardware context exists. This section binds that
reading. The hardware wire surfaces themselves — the init task, its
statuses, errors and the device-identity assertion — stay bound by
chapter 55 §55.11 and by R28–R28C above.

**R50.** *A hardware context is accepted next to either software login,
and only there.* `task::init_trezor::init` MUST be accepted while the
daemon runs under a mnemonic (global-HD) login **and** while it runs under
a legacy passphrase (`Iguana`) login. The initialisation MUST NOT consult
the key-pair policy, MUST NOT refuse or warn on account of the login type,
and MUST NOT switch the daemon's key-pair policy, its trading identity
(R32) or its wallet identity (R29): after the task completes, every
software surface behaves exactly as before, and coins already activated
keep the key source they were activated with (R52). In no-login mode (no
wallet name and no passphrase; chapter 45 §45.4) there is no
central cryptographic context to hold the slot, so the task MUST fail at
status level with `Internal` (string, HTTP 500); the reference offers no
hardware-only login (D7.d). While a hardware context already exists, a new
init task MUST replace it (the previous context is dropped once the new
device has been read); the only concurrency refusal is
`HwContextInitializingAlready` (HTTP 400) while another init task is still
running (chapter 55 R55.11.6). A failed or cancelled init MUST leave the
slot empty, including when it was filled before the attempt.
*Rationale:* the device is a key source for individual coins, not an
alternative identity for the node; refusing it under one login type would
have no behavioural basis in the reference and would break the published
procedure (connect the device, then activate the coins that need it).

**R51.** *Device identity digest.* The hardware context's identity is the
20-byte `RIPEMD160(SHA256(pubkey))` digest of the compressed secp256k1
public key the device returns for the daemon's **internal derivation
path** (R16, `m/44'/141'/2147483647/0/0`), requested with the device's
Bitcoin-family public-key message and the secp256k1 curve. It is the value
`task::init_trezor` reports as `device_pubkey` (chapter 55 R55.11.5), the
value the optional `device_pubkey` assertion of R28A and R55.11.2 compares
against, and the storage identity of every coin activated under the
`Trezor` policy (R55). It is distinct from, and in general unequal to, the
software identity of R29 (the digest of the login's internal key at the
same path): the two coincide only when the device holds the same seed as
the login, in which case every key the two sources derive coincides too.

**R52.** *The key source of a coin is its own activation policy; hardware
presence never decides.* For every coin family that accepts a key policy
in its activation request (`priv_key_policy`), the source of the coin's
keys and of its account extended public keys MUST be fixed **per coin, at
activation, by that request**: `ContextPrivKey` (the default when the
member is absent) binds the login's software key — the single key under
`Iguana`, the seed-derived HD keys under `GlobalHDAccount` (§5.9A, §5.9B);
`Trezor` binds the device; `WalletConnect` binds the session named in the
request (chapter 22). The existence of a hardware context MUST NOT change
the source of a `ContextPrivKey` activation, and a `Trezor` activation
MUST NOT fall back to the seed when the context is missing. The coins-file
`trezor_coin` member is a **precondition, not a selector**: it is read
only when the `Trezor` policy was requested, and its absence refuses that
activation (`CoinDoesntSupportTrezor` where the family exposes it; the
EVM task maps it to `Internal`, chapter 35 R35.3.2); its presence on a
`ContextPrivKey` activation has no effect. Two preconditions apply to a
`Trezor` activation and to nothing else: a hardware context exists
(otherwise `HwContextNotInitialized`, reported through the family's
coin-creation failure class for UTXO coins and as `Internal` for EVM,
chapter 55 / chapter 35) and the coin configuration carries a usable
`derivation_path`. A `Trezor`-policy coin and a `ContextPrivKey` coin of
another ticker MUST be able to coexist in one daemon; the mapping of a
ticker to a source lasts for the lifetime of its activation, and changing
it requires deactivation and a fresh activation (an activation of an
already-active ticker is refused as such, whatever policy it names;
chapter 55). *Rationale:* a wallet that mixes device-held and seed-held
coins is the reference's normal shape; keying any decision on "a device is
plugged in" would silently re-route seed-held coins to the device or the
other way round.

**R53.** *Per-family key-source rule under a software login with a
hardware context present.* The following table binds where each family's
key comes from when both a software login and a hardware context exist.
"Software" means the login's key as §5.9B describes it.

| Family and method | Source when the request says `Trezor` | Source when the request says nothing or `ContextPrivKey` | Notes |
| --- | --- | --- | --- |
| UTXO standard, BCH, QTUM — `task::enable_utxo`, `task::enable_bch`, `task::enable_qtum` | device; HD shape; requires the context, `trezor_coin` and `derivation_path` | software | the policy is honoured only by the task methods |
| UTXO standard, BCH, QTUM — legacy `enable` / `electrum` | *(not reachable)* | software | the legacy methods have no policy member and never use the device, even with a context present |
| SLP and Lightning on a `Trezor` BCH / platform | inherit the platform's device source | inherit the platform's software source | tokens take their platform's policy |
| QRC20 on a `Trezor` QTUM platform | software | software | the token's own activation always binds the login key; a QRC20 token on a device-held QTUM platform therefore signs with the seed while its platform is on the device (a mixed state the reference permits; D7.e) |
| EVM platform — `task::enable_eth_with_tokens` | device; HD shape; requires the context, `trezor_coin` and `derivation_path` (chapter 35 R35.3.2) | software | |
| EVM platform — one-shot `enable_eth_with_tokens` | refused: `UnexpectedDeviceActivationPolicy` (chapter 35 R35.1.5) | software | |
| ERC20 token (task and one-shot) | inherits the platform's source; the task form reads account keys from the device, the one-shot form refuses a device-held platform (chapter 35, the `enable_erc20` divergence note) | inherits the platform's software source | |
| EVM `get_my_address` (no activation) | n/a — see R57 | device **if a hardware context exists**, else software | the one presence-keyed surface |
| Tendermint and its tokens — `task::enable_tendermint_with_assets` | the member is accepted and ignored (chapter 36, request table); software | software | a device source is unreachable; a pubkey-only or WalletConnect activation is the family's alternative (chapter 36) |
| Sia, Solana, ZHTLC (ARRR) | *(not reachable from the request)*; the families refuse a hardware policy with a typed hardware-not-supported error if it is ever presented (chapter 46 R46.1.3, chapter 40, chapter 39) | software | the hardware context is never consulted |

**R54.** *Account creation, new address, scan and listing follow the
coin's policy.* `task::create_new_account`, `get_new_address`,
`scan_for_new_addresses` and `account_balance` MUST choose the device or
the software derivation by the **coin's** key policy (`Trezor` or not),
never by whether a hardware context exists at call time. For a
`Trezor`-policy coin the account's extended public key is read from the
device, a new address is confirmed on the device, and the current hardware
context is looked up at call time: if the slot is empty the call fails with
`HwContextNotInitialized` (HTTP 400; `task::create_new_account` and
`get_new_address` carry the token; the activation tasks report the same
condition through their failure classes, R52). For a `ContextPrivKey` coin
the same calls derive in software (§5.9A R30, §5.9B R35c) and MUST NOT
touch the device even while a context exists. `account_balance` and the
address lists in activation results read the coin's own account set and
contact no device under either policy. *Rationale:* the account set a coin
exposes is the set its key source produced; mixing sources inside one
coin's account set would store extended public keys that the coin's signer
cannot spend from.

**R55.** *Storage identity of a hardware-policy coin.* The per-coin HD
account storage (the HD-account table of the wallet database) MUST namespace a `Trezor`-policy coin by the
**device digest of R51** and a `ContextPrivKey` global-HD coin by the
**software digest of R29**, the choice following the coin's policy (R52),
not the login type and not the presence of a context. Consequences that
MUST hold: (1) a `Trezor` activation and a later `ContextPrivKey`
activation of the same ticker under the same login use **different**
namespaces, so the later software activation MUST NOT load device-derived
accounts, and a later `Trezor` activation MUST NOT load seed-derived ones;
(2) two devices with different seeds have different namespaces for the
same ticker and login; (3) a device holding the login's own seed shares
the software namespace, which is harmless because its accounts are
identical. *Dictated on-disk shape (interop):* the reference release rows
store one account per row with the columns `coin`, `hd_wallet_rmd160`,
`account_id`, `account_xpub`, `external_addresses_number` and
`internal_addresses_number`; `hd_wallet_rmd160` is the policy-chosen
digest above, and the key of a row is (`coin`, `hd_wallet_rmd160`,
`account_id`). The *baseline anchor (R1)* keyed the same table additionally
by the login's software digest (`mm2_rmd160`), and at the anchor only the
device source existed, so the two columns always held different values;
the reference dropped the software column before `v2.6.0-beta`. Reloaded
keeps the anchor's three-part key (R29, chapter 38 R38.8.1); under this
rule its `hd_wallet_rmd160` column MUST hold the policy-chosen digest, so
that the three-part key separates the cases exactly as the reference's
two-part key does (D7.b).

**R56.** *Refusals are keyed on the coin's policy, not on hardware
presence and not on the login type.* In the reference no wallet-level
method consults the hardware slot: `get_mnemonic`,
`change_mnemonic_password`, `get_wallet_names` and `delete_wallet` depend
only on the named wallet and its password (chapter 07 §7.3A); they succeed
under a mnemonic login while a device is connected, because the mnemonic
they handle is the login's, which the device neither holds nor protects.
`get_public_key`, `get_public_key_hash` and `get_shared_db_id` report the
**software login's** identity (R32) with or without a device. Every
hardware-related refusal is **per coin**, raised when *that coin* was
activated under the `Trezor` policy: the legacy private-key display
(`show_priv_key`) and the bound key-export paths of chapter 07 R-K6 refuse
a `Trezor`-policy coin and MUST still serve the software coins of the same
daemon; the single-address public-key accessor answers the `Trezor`
variant of the unexpected-derivation-method class; swap signing and
peer-to-script-hash signing on a `Trezor`-policy UTXO coin, swap signing
and raw-transaction signing on a `Trezor`-policy EVM coin, fail with the
family's typed error (hardware swaps are unsupported in the reference,
chapter 35 / chapter 38); and the HD-account calls of R54 need the context
at call time. *Rationale:* a refusal keyed on "a device is present" would
deny the operator access to secrets the device has nothing to do with,
and a refusal keyed on "login type" has no referent, since every login is
a software login. Reloaded diverges on the two mnemonic methods; see the
compatibility note and D7.c.

**R57.** *The one presence-keyed surface: the own-address query.*
`get_my_address` for an EVM-protocol coin that has not been activated
(chapter 07 T7) MUST, in the reference, derive the reported address from
the **device** whenever a hardware context exists — requiring the coin's
`trezor_coin` and `derivation_path` and the device's HD account state —
and from the login's software key at `path_to_address` otherwise. It is
the only reference surface where the presence of a hardware context, and
not a per-coin policy, selects the key. Reloaded's `get_my_address` derives
from the login's key only (compatibility note below); whether to follow
the reference here is D7.e.

**R58.** *Reference states.* Everything in R50–R57 is identical at the
rows `v2.6.0-beta` and `v3.0.0-beta`: the hardware context, the per-coin
policy selection, the storage namespace and the refusal set did not change
between them (the only differences in the files read are copyright lines
and, in the EVM platform task, two refusals of NFT activation on TRON that
do not touch key selection). Against the *baseline anchor (R1)* the
reference differs in four points that this chapter already binds
elsewhere or that R55 records: the task methods are `task::init_trezor::*`
with `cancel` (the anchor had only the three flat methods, chapter 55
R55.11.1); re-initialising an already-filled slot is accepted and replaces
it (the anchor refused it as already initialised); the only software login
at the anchor was `Iguana`, and the device was the only HD key source
(§5.9A upstream-divergence note); and the storage key had three parts
(R55).

> **Compatibility (this repository, 2026-11-08; not corpus-sourced).**
> Checked against the worktree at the `dev` tip of 2026-11-08:
>
> 1. *Selection keyed on presence.* The UTXO standard, BCH and QTUM
>    activation tasks, the shared platform-with-tokens activation, the Sia
>    activation and the EVM hardware activation path choose the device
>    extended-public-key reader whenever the hardware slot is filled, and
>    the software derivation only when it is empty. Under a mnemonic login
>    with a connected device this sends a `ContextPrivKey` UTXO or EVM
>    activation to the device for its account keys while its signer stays
>    the seed — the mismatch R52 and R54 forbid. R31 and T12, as worded
>    before this pass, prescribed that behaviour; they are amended with
>    this section. See the code-quality finding below.
> 2. *Storage namespace.* Reloaded's HD-account storage initialiser prefers
>    the software digest of R29 whenever the login is global-HD and falls
>    back to the device digest only under an `Iguana` login. Under a
>    mnemonic login with a device, a `Trezor`-policy coin is therefore
>    stored under the software namespace — the same rows a `ContextPrivKey`
>    activation of that ticker uses — so the later activation of R55 (1)
>    *does* load the other source's accounts, and a device swap leaves the
>    namespace unchanged. See the code-quality finding below.
> 3. *Mnemonic methods.* Reloaded's `get_mnemonic` and
>    `change_mnemonic_password` refuse whenever a hardware context exists
>    (chapter 07 R-K6, "a hardware-wallet session MUST be rejected"). The
>    reference does not (R56). Under a mnemonic login this blocks the
>    export or re-encryption of the login's own mnemonic because a device
>    is connected. Chapter 07 R-K6's per-coin arm (refusing a `Trezor`-policy
>    coin in key export) matches the reference; its wallet-level arm is a
>    Reloaded hardening. D7.c decides whether to keep it.
> 4. *Own-address query.* Reloaded's `get_my_address` derives from the
>    login's key under both policies and never consults the hardware slot;
>    the reference's presence keying of R57 is not implemented. D7.e.
> 5. *Re-initialisation.* Reloaded already replaces a filled slot on a new
>    init and refuses only a concurrent one, as R50 requires.
> 6. *No-login mode.* Reloaded's init task reads the central context first
>    and therefore fails as R50 requires when there is none.
>
> *Status (2026-11-08, batch 4 item B4-2, PR #293):* items 1 to 3 are
> resolved as D7.a–c record (the source of a coin's keys and its storage
> digest follow its policy; the wallet-level refusals are gone); item 4
> stays, by D7.e. Every site of item 1 now leaves the choice to the coin's
> policy: the UTXO and QTUM activation by the activation policy, the
> account reader by the coin's key policy, Siacoin by ignoring an offered
> device, EVM by its signer as before. `get_new_address` on a `Trezor`-policy
> coin now looks the hardware slot up at call time and answers
> `HwContextNotInitialized` when it is empty (R54, T35). Two points of R54
> and R53 stay open: a new address of a `Trezor`-policy coin is derived from
> the stored account key and not confirmed on the device (a follow-up), and
> the BCH task refuses the `Trezor` policy (chapter 55 R55.9.2). When the
> accounts of a coin of the software policy are loaded from storage, each
> stored row's account extended public key is compared with the key the
> seed derives for that account index below the coin's derivation path; a
> row that differs (one an earlier build stored for a `Trezor`-policy coin
> under the software digest) is deleted and not loaded, with one WARN that
> names the ticker and the removed account ids and carries no key material.
> A failure of the derivation or of the removal fails the load. R55 (1)
> therefore holds also for rows written before this change. Tests:
> `test_r55_*` in `coins/hd_wallet_storage/mod.rs`.

> **Code-quality finding (informative; this repository, 2026-11-08).**
> *Trigger:* a contradiction between two things read side by side — the
> reference's per-coin policy keying (R52) and the presence keying at six
> Reloaded sites (compatibility items 1 and 2). *Problem:* under a
> mnemonic login with a connected Trezor, (a) every `ContextPrivKey` HD
> activation of a UTXO, QTUM, BCH, EVM or Sia coin asks the device for its
> account extended public keys and stores addresses the seed cannot spend
> from, and (b) a `Trezor`-policy coin's accounts land in the software
> namespace shared with the seed-held activation of the same ticker.
> *Proposed fix:* (a) choose the extended-public-key reader from the coin's
> own key policy (`Trezor` → device, otherwise software), as the reference
> does, and drop the "is a device connected" test at those sites; a
> `Trezor` request with an empty slot keeps failing as today; (b) have the
> coin builders pass the policy-chosen digest (R55) into the storage
> initialiser instead of letting the initialiser infer it from the login,
> keeping the three-part key. Both changes are behaviour-neutral for a
> daemon without a device and for an `Iguana` login with a device.

> **Code-quality finding (informative; corpus-derived, stated as
> behaviour).** *Trigger:* apparent with little extra effort while reading
> R50 and R54 side by side. *Problem:* in the reference a coin activated
> under the `Trezor` policy keeps no record of the device identity (R51) it
> was activated with; it looks up whatever hardware context exists at each
> device operation. Because R50 lets a new init replace the context with a
> different device, such a coin then confirms addresses, reads new account
> keys and signs with a device whose keys do not match its stored accounts,
> and stores the new device's account keys under the old device's
> namespace (R55). *Proposed fix (Reloaded):* record the device digest in
> the coin at activation and, before any device operation, compare it with
> the current context's digest; on mismatch answer the existing
> `FoundUnexpectedDevice` hardware error class without touching the device
> or the storage. Nothing in this is a wire change.

**R58a.** *Device identity of a `Trezor`-policy coin (Reloaded-original;
maintainer decision EC (a), 2026-11-08; batch 4 item B4-14).* When a coin
is activated under the `Trezor` policy, the node records the device digest
of R51 of the hardware context the activation used. Before every operation
of that coin that would use the device or the device's storage namespace —
reading an account's extended public key (`task::create_new_account` and
the account an activation creates), deriving a new address, and signing
(except a swap's signing, R58a.1) — the node compares the recorded digest
with the digest of the current hardware context. On a mismatch the
operation fails with `FoundUnexpectedDevice` before the device or the
storage is touched; an empty slot keeps the answer it had:
`HwContextNotInitialized` for the account and address calls (R54),
`NoTrezorDeviceAvailable` for a withdrawal. The coin keeps its recorded
digest for the lifetime of its activation; using another device needs a
deactivation and a fresh activation (R52). *Rationale:* the corpus-derived
finding above. *Compatibility (informative):* no wire shape changes, and
only this node's own coins are affected: where the reference would use the
new device's keys against the old device's accounts, Reloaded refuses.
Under `docs/COMPAT_SWITCHES.md`, "The default-side rule", arm 2: Reloaded's
behavior by default, no switch.

**R58a.1.** *A swap keeps the coin's device (Reloaded-original;
2026-11-08, batch 4 item B4-14, review of PR #302).* The swap-v2 signing of
a UTXO coin is excepted from R58a's comparison with the current hardware
context. A swap, whether it was already running or starts later, signs
through the coin's recorded hardware context, whose connection check reads
the identity of the connected device and refuses any other device with
`FoundUnexpectedDevice`. The swap therefore signs with the coin's own
device whenever that device is connected, also after a re-init put another
device in the slot, and never with another device. *Rationale:* refusing a
swap's signing would cost the trader a wait for the refund. *Safety:* as
under R58a, no other device signs and nothing is written to another
device's storage namespace; unlike R58a, the device is asked before the
refusal.

> **As built (2026-11-08, clean-side record; batch 4 item B4-14, PR #302).**
> A coin activated under the `Trezor` policy keeps the hardware context its
> activation read (`UtxoCoinFields.hw_ctx` for the UTXO family,
> `EthTrezorSigner.hw_ctx` for EVM and its tokens). The activation reads the
> context once and uses that one context for the device read, the coin's
> record and its storage namespace (`HDWalletCoinStorage::init_for_device`).
> Before reading an account's extended public key (`task::create_new_account`
> and the account an activation creates), before `get_new_address`, and before
> signing a withdrawal, the node compares the digest of that context with the
> current one (`HardwareWalletCtx::ensure_same_device`); a different device
> fails with `FoundUnexpectedDevice` through the path's existing hardware error
> class, before the device or the storage is touched. An empty slot keeps the
> answer it had: `HwContextNotInitialized` for the account and address calls,
> `NoTrezorDeviceAvailable` for a withdrawal. A scan and `account_balance` ask
> no device; Reloaded's scans derive from stored keys. The swap-v2 signing of
> a UTXO coin keeps its own check instead (R58a.1): it signs through the
> coin's recorded context, whose connection check reads the identity of the
> connected device and refuses another device with `FoundUnexpectedDevice`.
> That check asks the device first and follows the connected device, not the
> current hardware context. Tests: the `r58a_*` tests in `coins`
> (utxo_tests, eth_tests, hd_wallet_storage); in `crypto`,
> `ensure_same_device_compares_the_digest`, and over a scripted device
> `check_trezor_refuses_another_device`,
> `check_trezor_accepts_the_device_of_the_context` and
> `trezor_returns_the_connected_client_of_the_context_device`. Covered
> through their shared check only, since the lab has no Trezor: the wiring of
> the two withdraw signing paths, EVM's new-address check, and the swap-v2
> signing's use of the connection check. The reconnect that
> `HardwareWalletCtx::trezor` makes after a failed check needs a real
> transport and is not tested. The activation's single read is covered by
> construction and the storage test.

*Verification notes.*

**T31.** *Init under both logins.* With a Trezor emulator, start one
daemon under a mnemonic (`enable_hd: true`) login and one under a legacy
passphrase login; `task::init_trezor::init` → `status` reaches `Ok` on
both, `get_public_key` and `get_public_key_hash` report the same values
before and after, and a software coin activated before the init still
signs a withdrawal from the seed afterwards (R50). In no-login mode the
task's status is `Error` with `error_type` `Internal` (R50).

**T32.** *Presence does not select.* Under a mnemonic login with the
emulator initialised, `task::enable_utxo` of a UTXO coin **without**
`priv_key_policy` completes without any device interaction (no
`WaitingForTrezorToConnect` or `FollowHwDeviceInstructions` status, no
device request recorded by the emulator), its `account_xpub` equals the
software derivation of R30 for the login's mnemonic, and `get_new_address`
on it contacts no device; the same request with `"priv_key_policy":
"Trezor"` for another ticker reads its account key from the device and
confirms a new address there (R52–R54).

**T33.** *Namespaces.* Under one mnemonic login, activate ticker X with
`Trezor` (emulator seed A), deactivate, activate X with `ContextPrivKey`;
the second activation reports account `0` with the software `account_xpub`
and not the device's; re-activating with `Trezor` reports the device's
again; the stored rows for X exist twice with different `hd_wallet_rmd160`
values, the device one equal to the `device_pubkey` of the init task (R51,
R55).

**T34.** *Refusals by coin policy.* With a device connected under a
mnemonic login, `show_priv_key` of a software coin succeeds and of a
`Trezor`-policy coin is refused; `get_mnemonic` behaves per the outcome of
D7.c, and the test pins whichever is chosen; `get_public_key_hash` equals
the software identity, not `device_pubkey` (R56).

**T35.** *Context lost.* After `task::init_trezor::cancel` or a failed
re-init emptied the slot, `get_new_address` and `task::create_new_account`
on a `Trezor`-policy coin answer `HwContextNotInitialized` (HTTP 400 for
the direct method; status-level for the task), while the same calls on a
software coin succeed (R54).

**T36.** *Device replaced.* Re-run `task::init_trezor::init` with a second
emulator seed while a `Trezor`-policy coin from the first is active; the
test pins the behavior chosen under the corpus-derived finding above
(refusal with `FoundUnexpectedDevice` before any device or storage
access). *Adopted as R58a (decision EC (a)).* The lab has no Trezor, so
the refusal is pinned by unit tests with two device contexts.

**D7.** *Open decisions for the maintainer (2026-11-08).*

- **D7.a** *Refuse `task::init_trezor` under a software login?* The
  reference accepts it under both logins (R50) and has no other way to
  obtain a context. Options: (i) parity — accept under both, with the
  per-coin keying of R52–R55 restored (recommended; the only option that
  keeps the published connect-then-activate procedure); (ii) accept only
  under a global-HD login (no reference basis; blocks a legacy-passphrase
  user who wants a device-held coin); (iii) refuse unless a hardware-only
  login exists — impossible until D7.d is decided in favour.
- **D7.b** *Must the storage identifier carry the key source?* In the
  reference the single digest *is* the source's identity (R55). Reloaded's
  three-part key separates the cases once `hd_wallet_rmd160` holds the
  policy-chosen digest; an explicit source column would be a schema change
  (chapter 44) with no reference counterpart. Options: (i) policy-chosen
  digest in the existing column (recommended; no migration); (ii) add a
  source column (migration; clearer audits).
- **D7.c** *What do refusals key on?* Reference: the coin's policy, nothing
  at wallet level (R56). Reloaded: additionally hardware presence for
  `get_mnemonic` / `change_mnemonic_password` (chapter 07 R-K6). Options:
  (i) parity — drop the wallet-level presence check, keep the per-coin
  arm; (ii) keep the Reloaded hardening and document it as a divergence in
  chapter 07; (iii) key the two methods on whether the active wallet holds
  a software mnemonic (always true today), which is (i) in effect.
  Login-type keying is not an option: every login is a software login.
- **D7.d** *A hardware-only login.* Not in the reference. If wanted, it is
  a new mode with its own identity rules (trading identity, R32; storage,
  R55) and is out of this section's scope.
- **D7.e** *Follow the reference's two presence/mixed behaviours?*
  `get_my_address` keyed on presence (R57), and QRC20 tokens on a
  device-held QTUM platform signing with the seed (R53). Options: (i)
  parity; (ii) key `get_my_address` on an explicit request member and
  refuse the QRC20 mixed case; (iii) keep Reloaded's current software-only
  `get_my_address` (an existing, documented divergence) and refuse the
  mixed QRC20 case. Neither affects wire shapes.

> **Resolutions (clean-side record, 2026-11-08; maintainer decision EB (b),
> batch 4 item B4-2, PR #293).**
> - *D7.a:* (i). `task::init_trezor` stays accepted under both software
>   logins, and every coin takes its keys by its own policy (R52–R55).
> - *D7.b:* (i). The policy-chosen digest goes into the existing
>   `hd_wallet_rmd160` column; no schema change.
> - *D7.c:* (i). The wallet-level presence checks of `get_mnemonic`,
>   `change_mnemonic_password` and `get_private_keys` are dropped; the per-coin
>   arm stays. In addition, Reloaded's opt-in export superset (chapter 07
>   R-K4) refuses a requested ticker that is activated under the `Trezor`
>   policy, so that no form of the export returns a key for a device-held
>   coin. There is no hardware-only login in this codebase either: every
>   context has an `Iguana` or `GlobalHDAccount` policy, and the hardware
>   context sits beside it.
> - *D7.d:* not pursued (out of scope).
> - *D7.e:* `get_my_address` keeps Reloaded's software-only derivation, a
>   divergence from R57; the mixed QRC20-on-`Trezor`-QTUM case of R53 keeps
>   the reference behaviour for now and is an open follow-up.
> - The corpus-derived code-quality finding above (no record of the
>   activating device) is resolved by maintainer decision EC (a): R58a
>   (batch 4 item B4-14, PR #302).

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
> refusals fall away with them (§5.12C for H3, §5.12D for H4). Under an iguana login the three
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
> fails in the coin layer. Item H3 (§5.12C; PR #250, merged 2026-11-05)
> removed the pairs that this section refuses, and the divergence ended when
> item H4 landed (§5.12D; PR #254, merged 2026-11-05).

> **As built (2026-11-04, clean-side record; no requirement above changes).**
> *Coin side.* `SwapOps::swap_key_disagreement` (`coins/lp_coins_traits.rs`) answers
> `None` when a coin's swap key is its paying key and for a coin the rule does not
> apply to (the default, which covers ZHTLC, Sia, Tendermint, Solana and Lightning).
> UTXO-family coins (standard, QTUM, BCH, QRC20, SLP tokens) share
> `utxo_common::swap_key_disagreement`. *(As built 2026-11-04: a single key was compared
> with the node's persistent key; a coin with an HD key policy was compared by path, its
> `path_to_address` against account `0`, `External`, address `0`, the address
> `active_utxo_sender_address` funded from. Since §5.12C both comparisons are gone, see the
> note under §5.12C: the coin announces its own key and pays from its address.)* The Trezor
> policy is out of scope. EVM coins compared the compressed form of
> the signer's public key with the node's persistent key (`eth_swap_ops.rs`; since §5.12C a
> software signer announces its own key, and only a key-less signer is still compared with the
> node's persistent key); a
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
> plan covers the end-to-end behavior. *(Since §5.12C the T15 and T16 refusal cases of the coin
> tests are agreements; the tests were rewritten, see the note under §5.12C.)*

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
> not change here. It moved to the internal key with item H4 (D6; §5.12D,
> PR #254, merged 2026-11-05). Until then it stayed the passphrase hash, and a
> negotiation whose HTLC keys differed from it emitted shape 3 of chapter 51
> R60.
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
  node's persistent key, which is the key its Started event recorded (once
  §5.12D is built, R47 chooses that key).
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
> R74). Two differences to the reference remained after this section, and
> both went with item H4 (§5.12D; PR #254, merged 2026-11-05; D6): the
> persistent key, which until then was still the passphrase hash under a
> global-HD login, and the trading identity of §5.9B R32. A swap between a Reloaded
> node built with this section and an earlier Reloaded build negotiates as
> any shape-3 swap does; the earlier build's own HD swaps still fail as they
> did.

> **As built (2026-11-05, clean-side record; no requirement above changes).**
> *R41, key source.* A new `SwapOps::own_signing_key_pair` (`coins/lp_coins_traits.rs`, default
> `None`) names the key pair a coin with one key of its own signs with: a single-key UTXO coin
> (`utxo_common::own_signing_key_pair`, used by the standard coin, QTUM, BCH, QRC20 and the SLP
> token through its platform coin) and an EVM coin with a software signer (`eth_swap_ops.rs`, the
> signer's secret as a compressed pair; a hardware or key-less signer answers `None`).
> `SwapOps::get_htlc_key_pair` is unchanged and still answers for an HD key policy, Tendermint and
> the Trezor policy. *R41 and R43, mechanism.* `start_htlc_key_pair` (`mm2_main/src/lp_swap.rs`)
> now takes the node's persistent public key. For a coin that does not derive its key it takes
> `get_htlc_key_pair`, else `own_signing_key_pair` when that pair is not the persistent key, and
> announces and saves the pair it took in the saved fields `maker_coin_htlc_privkey` and
> `taker_coin_htlc_privkey`; a coin whose own key is the persistent key (every coin under an
> iguana login) takes nothing, announces the persistent key and saves nothing, which keeps the
> wire and the saved swap byte-identical. The maker and taker start paths pass
> `ctx.secp256k1_key_pair().public()`. Resumption and recovery needed no change: a restored swap
> holds the saved pair (`restored_htlc_key_pair`), a swap saved without one keeps the
> persistent key its Started event recorded, and `recover_funds_of_swap` reads the saved pair
> among its candidates. *R42.* `enabled_hd_address_info` and `try_enabled_hd_address_info`
> (`utxo_standard_swap_v2.rs`) answer, for a coin with `PrivKeyPolicy::HDWallet`, the address of
> the activated key with the full derivation path of `path_to_address` (`activated_hd_address_info`);
> a coin with another policy keeps the first external address of account `0`. Every reader of
> the helper follows: the V1 and V2 funding and change (`active_utxo_sender_address`,
> `utxo.rs`), the fee estimate (`trade_preimage_sender_address`, `utxo_common_tx.rs`), the V2
> spend and refund output (`build_and_broadcast_maker_v2_spend`), and the V2 HTLC key
> (`get_htlc_pubkey_v2`, `get_htlc_key_pair_v2`, `try_derive_htlc_pubkey_v2`). The V1 spend and
> refund outputs pay to the address of the swap's own HTLC key and already did. The balance
> checks read the activated key's address (`my_balance` through `my_public_key`) and the
> fee estimates the helper, so they agree. *R44.* `utxo_common::swap_key_disagreement` takes the
> coin's own key as the swap key and, for a coin with an HD wallet, compares its public key
> with the funding address's: agreement by construction for an HD key policy, a disagreement
> (`HdAddress` for the HD key policy, else `SingleKey`) only for a coin whose funding address
> is not its own, or whose funding address cannot be established: the guard fails closed, and
> the free helper takes no node key any more (the trait method keeps it for the EVM check of a
> key-less signer). The EVM check takes the signer's own key as the swap key. `swap_key_check`
> and the node side are unchanged. *Tests.* T19: `eth_tests::swap_key_agreement_tests::
> t19_an_evm_coin_announces_the_compressed_signer_key`; T20, T21, T22 and T24 (coin side):
> `utxo_tests::hd_coin_swap_key_tests::{t20_the_funding_address_is_the_one_at_path_to_address,
> t20_trade_preimage_estimates_from_the_address_at_path_to_address,
> t20_change_and_spend_outputs_use_the_address_at_path_to_address,
> t20_the_v2_htlc_key_is_the_key_at_path_to_address, t20_at_the_default_path_the_first_address_is_used,
> a_coin_without_an_hd_key_policy_keeps_the_first_address_of_its_account,
> t21_a_single_key_coin_announces_and_funds_from_its_own_key,
> t22_the_key_sources_are_unchanged_for_the_other_policies, t24_no_coin_of_t16_disagrees}`;
> T24 guard: `utxo_tests::swap_key_agreement_tests::{a_single_key_other_than_the_node_key_agrees,
> an_hd_coin_at_another_address_agrees, the_guard_still_refuses_a_constructed_disagreement}` and
> `eth_tests::swap_key_agreement_tests::an_evm_coin_whose_signer_is_not_the_node_key_agrees`; swap
> start, resumption and recovery (T19, T21, T22, T23): the module `maker_hd_coin_swap_key_tests`
> of `mm2_main/src/lp_swap/` (`t22_an_iguana_login_saves_no_pair_and_announces_the_persistent_key`,
> `t21_a_coin_key_other_than_the_persistent_key_is_announced_and_saved`,
> `an_hd_key_policy_pair_is_still_saved`,
> `t23_a_swap_saved_without_a_pair_resumes_with_the_persistent_key`,
> `t23_a_swap_saved_with_a_pair_resumes_and_recovers_with_that_pair`). The node-side refusals
> of §5.12B stay covered by their mocked-coin tests (`ordermatch_tests::t05_12b_swap_key_check_tests`,
> `trade_preimage::tests::swap_key_mismatch`). End-to-end swaps are the SwapLab rows HD3 and HD4. *Update (2026-11-xx, §5.12D).* The persistent key mentioned above is
> now the node key, which is the internal key under a global-HD login; `start_htlc_key_pair` takes the swap's own
> node public key, and a swap saved without a pair keeps the node key pair its Started event recorded, chosen by
> `swap_node_key_pair` (the node key or the legacy passphrase key). Recovery lists both keys among its candidates.

## 5.12D One Trading Identity under a Global-HD Login

> **Scope (Reloaded-original, 2026-11-05).** This section is item H4 of plan
> [`hd-key-unification`](../plans/hd-key-unification.md), under maintainer
> Decision BL and decisions CR, CS and CT of 2026-11-05. It brings Reloaded to
> §5.9B R32 and to the persistent-key half of R33: under a global-HD login the
> node's own key is the internal key of R16, as at the reference. The
> passphrase hash stays available only where a record or a family activation
> still depends on it. Under an iguana login the internal key and the
> passphrase hash are the node's one key, and nothing changes.

**R45.** *The node key is the internal key.* Under a global-HD login, the
key the node uses as its own (its **node key**) MUST be the internal key of
R16, the key `get_public_key` reports. The node key:

1. signs maker orders, keep-alive messages and every ordermatch message sent
   without a per-order key, and fills `sender_pubkey` (§5.9B R32);
2. is the `persistent_pubkey` of every V1 swap started from now on: the
   negotiation, the own-key field of the start event, and the swap-statistics
   record (chapter 51 R59, R50A);
3. signs the swap messages and the transaction-helper messages of such a swap
   when it has no per-swap p2p key (chapter 51 R50B);
4. is the HTLC key of a coin that supplies none of its own (R43, chapter 51
   R63);
5. signs the peer healthcheck probe and its acknowledgment. The receiver checks
   only that the signature matches the key inside the message, so this has no
   effect on peers;
6. on a relay, is the key whose SHA-256 is the relay's libp2p identity secret
   (chapter 28 R19). An HD relay's peer identifier therefore changes once at
   upgrade (Decision CS; R49).

**R46.** *The legacy passphrase key.* The hash of the passphrase text, the
node key of earlier builds under a global-HD login, MUST stay available as
an explicitly named **legacy key** (Decision CR). It MUST be used only:

1. for a swap that recorded it (R47);
2. as the seed of the Lightning node, so that a Lightning node keeps its
   identity (Lightning is not enabled in this release);
3. by the shared platform-with-tokens activation, which passes it to the
   platform coin: BCH together with its tokens and Tendermint, until plan
   item H7 moves them to the coin key (§5.9B R35; decision CB, decided, not
   implemented yet), and Solana, which is not
   enabled in this release. The EVM platform ignores it *(amended
   2026-11-05: Solana named)*.

No other part of the node may use it.

**R47.** *Swaps that recorded the legacy key.* A swap whose Started event
recorded the legacy key as its own persistent key was started by an earlier
build. When resumed, it MUST use as its node key the key pair, out of the
internal key and the legacy key, whose public key equals the recorded one
(Decision CT). That pair signs the swap's messages and transaction-helper
messages, serves as the HTLC key of R43 when no pair was saved, and signs its
spend and refund. Manual recovery (`recover_funds_of_swap`) MUST include the
legacy key among its key candidates (chapter 39 R39.5.4). A recorded key that
matches neither key is reported as an error naming the swap, and that swap is
not resumed.

**R48.** *Ordermatch under a global-HD login.* With R45, the maker's orders
carry the key that keep-alive, `is_mine` and the stale-order sweep already use.
- A maker sends one keep-alive per pair every 30 s, and other nodes keep its
  orders without gaps.
- Its own `orderbook` marks them `is_mine`.
- The orderbook `address` of an own order is the internal key's address
  (§5.9B R32, R35).
- Orders restored from storage at start are announced under the internal key.
  The copies other nodes still hold under the legacy key expire after the
  maker-order timeout.

**R49.** *Operator note at upgrade.* The release that ships this section
MUST carry an upgrade note for HD logins covering:
- orders re-announced under the internal key, with the old copies expiring on
  other nodes after the maker-order timeout;
- swaps in flight finishing on the key they recorded (R47);
- swap statistics showing two public keys for the node across the upgrade;
- an HD relay's new peer identifier (R45 item 6), which peers learn again
  through the relay's address (chapter 28 R21).

> **Compatibility.** With this section Reloaded's trading identity, persistent
> key and relay identity under a global-HD login match the reference (§5.9B R32,
> R33; chapter 28 R19). The legacy key of R46 has no counterpart there: the
> reference never uses the passphrase hash under a global-HD login. Reloaded
> keeps it only for its own earlier swap records, the Lightning seed, and the
> two activations that item H7 is to move (decision CB; decided, not
> implemented yet).

> **As built (2026-11-xx, clean-side record; no requirement above changes).**
> *R45, R46, mechanism.* `MmCtx` (`mm2_core/src/mm_ctx.rs`) keeps two key fields. `secp256k1_key_pair` is the
> node key and `legacy_passphrase_key_pair` the legacy key, read through the accessor
> `MmCtx::legacy_passphrase_key_pair()` (it answers the node key when no separate legacy key is held, which is
> an iguana login and a test context). `CryptoCtx::init_crypto_ctx_with_policy_builder`
> (`crypto/src/crypto_ctx.rs`) now pins `secp256k1_key_pair` to the key `CryptoCtx` holds (the internal key under
> a global-HD login, the passphrase hash under an iguana login) and `legacy_passphrase_key_pair` to the hash of
> the passphrase text; the `DEFERRED(hd-dispatch)` comment is gone. `ctx.rmd160` and the database paths already
> derived from the internal key and are untouched. The legacy accessor is read at exactly three places: the
> Lightning seed (`coins/lightning/ln_utils.rs`, `init_keys_manager`) and the two family activations
> (`coins_activation/src/platform_coin_with_tokens.rs`, the BCH-with-tokens and Tendermint paths, and the Solana path, which is not enabled; the EVM path ignores the key). Every other
> reader of the context key (ordermatch signing and `sender_pubkey`, the swap start, swap messages, the
> healthcheck, `public_id`) is unchanged code and follows the pin. *R45 item 6.* `relay_p2p_identity_secret`
> (`mm2_main/src/lp_native_dex.rs`) is the relay's identity secret, the SHA-256 of the node key's secret.
> *R47, mechanism.* A V1 swap holds its own `node_key_pair` (`MakerSwap`, `TakerSwap`; the constructors take the
> pair instead of the persistent public key, which is derived from it). A swap started now gets the node key
> from the order-matching code. A restored swap gets it from `lp_swap::swap_node_key_pair`, which compares the
> `my_persistent_pub` of its Started event with the node key and then the legacy key and returns the matching
> pair, or an error naming the swap and the recorded key ("the swap is not resumed"); `load_from_saved` returns
> that error, so both the resume and `recover_funds` refuse such a swap. The swap's pair is the default HTLC
> key pair of a coin that saved none (R43), the key for `start_htlc_key_pair` and `derive_htlc_pubkeys` (which
> now takes the node public key instead of the context), the spend and refund key through the HTLC pair, and
> the signing key of the swap messages: `broadcast_swap_message(_every)`, `broadcast_p2p_tx_msg` and
> `broadcast_signed_msg_every` take the pair and sign with it when the swap has no per-swap p2p key
> (`swap_msg::signing_identity`). Recovery (`resolve_recovery_htlc_key`) adds the legacy key as a last candidate
> (`RecoveryHtlcKeyKind::LegacyPassphrase`) when it differs from the node key. *R48.* No ordermatch code
> changed: orders restored by `orders_kick_start` are announced through the same signing path, which now reads
> the internal key, and the keep-alive, `is_mine` and the stale sweep already used it. *Tests.* T13 and T25:
> `ordermatch_tests::hd_trading_identity_tests::{t13_t25_the_node_key_is_the_internal_key,
> t13_rmd160_and_database_path_derive_from_the_internal_key}`; T26: `...::t26_the_relay_identity_comes_from_the_node_key`;
> T27: `maker_hd_coin_swap_key_tests::{t27_a_swap_that_recorded_the_legacy_key_resumes_and_refunds_with_it,
> t27_a_swap_that_recorded_the_internal_key_and_a_new_swap_use_it,
> t27_a_recorded_key_that_matches_neither_key_is_reported, t27_swap_messages_are_signed_with_the_swaps_node_key}` and
> `taker_derived_htlc_key_tests::t27_a_taker_swap_resumes_with_the_key_it_recorded`; T28:
> `maker_hd_coin_swap_key_tests::t28_recovery_recovers_a_payment_built_with_the_legacy_key`; T29:
> `ordermatch_tests::hd_trading_identity_tests::t29_the_legacy_key_is_the_passphrase_hash` and
> `eth_with_tokens_activation::tests::t29_an_hd_daemon_keeps_the_passphrase_hash_for_the_family_activations`;
> T30: `ordermatch_tests::hd_trading_identity_tests::t30_an_iguana_login_is_unchanged` and
> `maker_hd_coin_swap_key_tests::t30_under_an_iguana_login_the_legacy_key_is_the_node_key`. Existing tests that
> restored a saved swap now record the persistent key of their context in the fixture, because a restored swap
> whose recorded key matches neither key is refused.

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
the account xpub via the software derivation of R30; for a coin
activated under the hardware key policy, the same extraction is
asserted to route to the device source (R25); for a context-key coin
it is asserted to stay in software while a hardware-wallet context
exists (§5.10B R52, T32). *(Amended 2026-11-08; passed the KDF Dirty
Gate with §5.10B on 2026-11-08.)*

**T13.** *HD trading identity (R32, R33).* Under a global-HD login, a
posted maker order's public key, the keep-alive's public key, the
`sender_pubkey` of a taker request and the v1 swap's persistent public key
shall all equal the internal key of R16, and none shall equal a key derived
by hashing the mnemonic string.

**T14.** *HD HTLC key is the coin key (R33).* Under a global-HD login with a
non-default `path_to_address`, a UTXO and an EVM v1 swap shall each put the
coin key at that path on the wire as the HTLC public key, fund the payment
from that key's address, and pay the spend or refund to it.

**T14a.** *The enabled address is listed from activation (R35a).*
*(Reloaded-original; passed the KDF Dirty Gate together with R35a on 2026-11-07.)* A
fresh HD wallet activates a UTXO coin with the default `path_to_address`,
and again with `account_id: 1, chain: External, address_id: 3` and no
`min_addresses_number`: in the first case address `0` of account `0`, in the
second account `1` with its external addresses `0` to `3`, appear in the
activation result and in `account_balance` without any `get_new_address`;
a restart keeps them listed; an `address_id` of 2^31 fails activation with
the address-limit arm; an iguana login is unchanged.

**T14b.** *UTXO maintenance commands under HD (R35b).* *(Reloaded-original;
same gate status.)* Under an HD login with two known addresses holding
outputs, `fetch_utxos` lists both with their derivation paths and the right
`total_count`; `consolidate_utxos` with `merge_at: 2` and `broadcast: false`
spends only the enabled address's outputs to itself and returns the
transaction unsent; a QTUM coin answers `CoinNotSupported` 400; fewer
outputs than `merge_at` answer `BadMergeConditions` 400.

**T14c.** *Account creation under a software HD login (R35c).*
*(Reloaded-original; same gate status.)* Under a software HD login,
`task::create_new_account` for a UTXO, a QTUM and an EVM coin each completes
without a hardware device: the first call creates account `1`, a call with
`account_id: 3` creates account `3`, the next call without an index creates
account `2`; `scan: false` returns no addresses; an index at 2^31 answers
`AccountLimitReached` 400; a repeated `account_id` is refused with a
request-class error before any derivation (the finding under R35c).

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


**T25.** *Node key under a global-HD login (R45, R48).* T13 holds. In
addition:
- the maker's own `orderbook` marks its order `is_mine`;
- one keep-alive per pair leaves every 30 s, signed with the internal key;
- the orderbook `address` of the order is the internal key's address.

**T26.** *Relay identity (R45 item 6).* A relay under a global-HD login has
the peer identifier derived from the internal key. Under an iguana login the
peer identifier is the same as before.

**T27.** *Legacy key for a recorded swap (R47).* A swap saved by the
previous build under a global-HD login, with the legacy key recorded and no
saved pair, resumes with the legacy key: its swap messages are signed with
it and its refund uses it. A swap started after the upgrade uses the
internal key. A recorded key that matches neither is reported, and that swap
is not resumed.

**T28.** *Recovery candidates (R47).* `recover_funds_of_swap` recovers a
payment of a swap that recorded the legacy key and saved no pair.

**T29.** *What keeps the legacy key (R46).* Under a global-HD login, the
Lightning seed, and the keys of BCH activated with its tokens and of
Tendermint, are the same as before. No other use of the legacy key remains.

**T30.** *Iguana login unchanged (R45–R48).* Under an iguana login the node
key, the relay peer identifier, the wire keys and every saved swap are the
same as those of the previous build.

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

**D6.** *(Resolved 2026-11-05 by §5.12D.)* One trading identity under a
global-HD login (plan [`hd-key-unification`](../plans/hd-key-unification.md),
item H4). R45–R49 move the trading identity and the swap's persistent key to
the internal key, and R46–R47 keep the passphrase-hash key for the swaps that
recorded it.

**D7.** *(Open, 2026-11-08; stated in §5.10B.)* Hardware context next to a
software login: whether to refuse `task::init_trezor` under a software login
(D7.a), whether the storage identifier must carry the key source (D7.b), what
refusals key on (D7.c), a hardware-only login (D7.d), and the two
presence-keyed or mixed reference behaviours (D7.e). *Resolved on 2026-11-08
(maintainer decision EB (b)); see the resolutions after D7 in §5.10B. The
device-identity finding there is resolved by decision EC (a), as R58a.*

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
- *Reloaded-original addition (2026-11-05) -- §5.12D (R45–R49), T25–T30, D6
  as resolved, and the pointers in §5.1 and R43.* Written by a context that
  never read the forbidden corpus. It rests on maintainer Decision BL, on
  decisions CR, CS and CT of 2026-11-05, and on this repository's own key,
  ordermatch, swap-message, recovery, relay-identity and healthcheck code. The
  sentences that describe the reference restate §5.9B R32, R33 and R35 and
  chapter 28 R19, which passed the KDF Dirty Gate on 2026-11-04. The addition
  carries no reference content of its own and needs no Dirty Gate pass of its
  own.
- *Scoped addition (2026-11-07) -- §5.9B R35a--R35c, T14a--T14c, the
  compatibility notes and the code-quality finding under them.* **KDF Dirty
  Gate: PASS on 2026-11-07, measure 0.04.**
  *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and `v3.0.0-beta`
  of chapter 02 §2.15A, identical at both for every fact taken, consulted
  **only** to establish: how HD activation treats the account and address of
  `path_to_address` (creation, scanning, the raised known-address count, the
  bound on the index, what the result reports); that no swap or withdraw
  makes an address known; which coin types the two UTXO maintenance commands
  serve under HD, over which addresses, to which destination, with which
  request and response members and error discriminants, and how the
  automatic merge behaves; and how the account-creation task chooses an
  index, scans, reports progress, cancels and fails under a software HD
  login. What crosses is behavior, public method names and dictated request,
  response and error members; no private identifier, function body, helper
  decomposition, error, log or comment text was reproduced. The compatibility
  notes rest on this repository's own code and on plan `hd-key-unification`
  and its preparation note; T14a--T14c are Reloaded-original. The finding is
  stated as a behavioral fact.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-07, measure
  0.04, no findings. *Extent:* HD activation's handling of `path_to_address`
  (account and address, scan policy, known-address minimum, index bound), the
  two UTXO maintenance commands and the automatic merge, and the
  account-creation task, at `v3.0.0-beta` with `v2.6.0-beta` as cross-check;
  for the whole batch, about 24 source files were read in full or in the
  relevant regions at the `dev (as of 2026-03-27)` row, whose source tree is
  identical to `v3.0.0-beta`, and the same modules were diffed against
  `v2.6.0-beta`. *Method:* reading, with mechanical cross-checks (diffs
  between the tabled states; pattern searches of the added lines for paths,
  identifiers and markers); the gate script was not run because the candidate
  has no code blocks. *Measure:* 0.04, read against the standard that any
  discretionary finding fails a chapter; dictated-interop and public-surface
  tokens were excluded from the measured set.
- *Clean-side amendment (2026-11-05) -- R46 item 3 and the as-built note
  under §5.12D.* They now name the Solana path of the shared
  platform-with-tokens activation, which also reads the legacy key, and say
  that the EVM path ignores it. Found in a second-session review of the H4
  code. Written by a context that never read the corpus, from this
  repository's activation code. It takes no forbidden-class input and needs
  no Dirty Gate pass.
- *Clean-side amendment (2026-11-06) -- one sentence of the compatibility
  note under R35.* It records decision CU of 2026-11-06 (the default sender
  of a UTXO withdraw without `from` is the coin's activated address) and
  that plan item H5 serves the direct method. Written by a session that
  never read the corpus, from that decision and this repository's own code.
  The reference facts of §5.9B are unchanged. It takes no forbidden-class
  input and needs no Dirty Gate pass.
- *Scoped addition (2026-11-08) -- §5.10B (R50--R58, T31--T36, D7), the
  re-keying amendments of R29, R31 and T12, and the D7 pointer in §5.14.*
  **KDF Dirty Gate: PASS on 2026-11-08, measure 0.02, no leaks**; the
  attestation follows this item. *Inputs:* the forbidden
  corpus at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A,
  identical at both for every fact taken, with the row *baseline anchor (R1)*
  for the anchor comparison, consulted **only** to establish: whether the
  hardware-wallet init task consults the login type or the key-pair policy,
  what it does in no-login mode, on re-initialisation and on failure; which
  request member, configuration member or context state selects a coin's key
  source per family, and which families cannot reach a device source; which
  identity digest namespaces a hardware-policy coin's stored accounts, the
  columns of that table, and how the anchor keyed it; which wallet-level and
  coin-level methods refuse on a hardware condition and what the condition
  is; and the behaviour of the own-address query when a hardware context
  exists. What crosses is behaviour, public method names, dictated request,
  response, error and schema members; no private identifier, function body,
  helper decomposition, error, log or comment text was reproduced. The
  compatibility note and the first code-quality finding rest on this
  repository's own code; the second finding is corpus-derived and stated as
  behaviour. T31--T36 are Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-08, measure
  0.02, no leaks. The pass covered this addition together with the
  re-keyed sentence of chapter 38 R38.8.1 and the chapter 05 row of
  chapter 30 §30.3. *Extent:* the hardware-wallet context and its init
  task, the per-coin key-policy selection for every coin family, the HD
  storage identity and its schema, the HD account and address methods, the
  wallet-level mnemonic and identity methods, the legacy key display and
  the own-address query; compared at `v2.6.0-beta` (49 files read or
  searched), at `v3.0.0-beta` (compared against `v2.6.0-beta`, the relevant
  differences read) and at the *baseline anchor (R1)* (4 files, plus token
  checks). *Method:* reading, with a mechanical comparison of every
  backticked token of the candidate (64 tokens) against the corpus and
  against this repository's tree, each match classified under the
  forced-versus-free test; the gate script was not run because there are no
  code blocks. *Measure:* 0.02 on the discretionary-expression basis.
