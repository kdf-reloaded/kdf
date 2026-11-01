# Chapter 22 -- WalletConnect v2

**Status:** driving-spec

> **One-sentence claim:** the project provides an in-tree
> WalletConnect v2 dApp implementation that exposes a per-chain
> signing trait to coin support modules and persists sessions
> across native and browser builds, conforming byte-for-byte to
> the public WalletConnect v2 specification on the wire.

## 22.0 Executive Summary

A dedicated subsystem in this codebase implements the
WalletConnect v2 protocol as a **relay client / dApp**: it
generates pairing URIs, proposes and maintains sessions with
external wallets, and dispatches signing requests over an
encrypted JSON-RPC channel. It does *not* implement the
wallet-side role -- the project never settles inbound sessions
on behalf of a remote dApp and never signs on behalf of an
external requester.

The subsystem is divided cleanly between:

1. A **protocol layer** carrying the WC2 wire format, pairing
   and session lifecycle, encrypted envelope codec, and relay
   websocket loop.
2. A **persistence layer** that mirrors the active session set
   to durable storage on both native (SQLite) and browser
   (IndexedDB) builds, behind a single trait. Whether and how
   sessions are *written* is governed by the `wc_session_persistence`
   node-configuration setting (§22.5); *loading* is unconditional.
3. An **integration trait** that coin support modules implement
   to surface WalletConnect-backed signing for the chain family
   they own. The trait is intentionally minimal and chain-family
   agnostic.

**Port status.** *Implemented.* The protocol and persistence
layers, the §22.3 integration trait (`WcCoinSigner`), the
session-establishment handshake of §22.4.1 (propose-publish,
session-key derivation, and settled-session construction/
persistence), at least one coin support module per chain family
(EVM, Cosmos, UTXO), and the five public RPC handlers of §22.9A.2
are all present in reloaded. The trait surface (§22.3), the
required coin-integration set, the public RPC surface, and the
clean-channel wire payloads specified normatively in §22.8.1 and
§22.9A are the contract this port was implemented against; the
persistence shape (§22.5) was already binding. See the §22.9A and
§22.9 status notes for the per-item detail.

**What is not yet reachable.** The ported trait implementors have no
caller outside the integration layer, so the subsystem can **pair
but not sign**: no activation policy, withdrawal route, or swap path
reaches them, and EVM activation refuses the WalletConnect signing
policy. §22.9B is the driving spec for that remaining call-site
port and records, operation by operation, how far the reference
release carries it.

## 22.1 Subsystem Shape

The subsystem is a standalone library compilation unit. It
depends only on:

- Standard async-Rust crates (futures, tokio-style channels in
  the workspace's runtime selection).
- The workspace's central context substrate for the lazy
  per-context handle pattern.
- The workspace's database abstractions (see
  [Chapter 25](25-sql-query-builder.md) for the native SQL path
  and the IndexedDB wrapper for the browser path).
- Third-party crates carrying the WalletConnect v2 relay client,
  pairing API, and Type 0 envelope codec. These are external
  Cargo dependencies, not vendored sources; the project invokes
  them through their public Rust APIs and does not reproduce
  their internals.

The subsystem **does not depend on any coin support module**.
Coin support modules depend on the subsystem (through the
integration trait of §22.5), never the other way around. This
direction is a binding architectural rule: the WalletConnect
subsystem must remain coin-agnostic so it can be compiled and
tested without any chain backend present.

Behaviourally the subsystem must cover the following set of
responsibilities. This is a functional decomposition, not a
prescribed module layout: a conforming implementation MAY group
these differently.

- a consumer-facing surface comprising the per-context handle and
  the pairing-URI type;
- a chain taxonomy expressing the CAIP-2 chain families and the
  supported request-method set;
- error reporting that maps failures onto WalletConnect error
  codes;
- demultiplexing of inbound traffic into request and response
  paths keyed by message id;
- a relay-connection event loop driving the websocket transport;
- the pairing lifecycle (propose, accept, expire);
- app-identity metadata and the authentication-token constants;
- session state together with its key material and lifetime
  management;
- handling for each WC2 session method (propose, settle, update,
  delete, event, extend, ping);
- persistence with both a native and a browser backend behind a
  common abstraction.

The subsystem is bounded in size (on the order of a couple of
thousand lines of Rust); it contains its own serialization
tests, no relay-loop or crypto tests.

## 22.2 Public Handle and Connection API

The subsystem exposes a single public handle type, accessed
through the per-context lazy-init pattern of the
codebase's central-context substrate. Call sites obtain
the handle from the central context; they do not construct it
directly.

The handle exposes the following operations:

| Operation                          | Purpose                              |
|------------------------------------|--------------------------------------|
| Generate a new pairing             | Returns a pairing topic and a `wc:`  |
|                                    | URI to be shown to the user          |
| Send a signing request, await reply| Encrypts the JSON-RPC payload, sends |
|                                    | over the relay, awaits the response  |
| Drop a session                     | Sends the WC2 delete RPC, removes    |
|                                    | the persisted row, unsubscribes      |
| Encode session byte strings        | Applies the session `encoding_algo`  |
|                                    | selector (hex by default; base64     |
|                                    | only for required-wallet interop)    |
| Resolve account for a chain        | Looks up the active account address  |
|                                    | and metadata for a given chain id    |
| Wallet-type detection              | Identifies certain wallet families   |
|                                    | (Ledger Cosmos app; Keplr) where the |
|                                    | wire path must differ                |

The pairing URI follows the public WC2 format
(`wc:<topic>@2?...`) and is opaque to the project: it is
delivered verbatim to the consumer (a GUI, a CLI, a deep link).

## 22.3 The Integration Trait

Coin support modules that wish to expose WalletConnect-backed
signing implement an in-tree trait with the following shape:

- A method that returns the CAIP-2 chain id the coin is bound
  to, given the WalletConnect handle.
- A method that signs an unsigned transaction (associated
  parameter type and associated return type, both chosen by
  the implementor).
- A method that signs and broadcasts a transaction (same
  associated-type pattern).
- A method that returns the pairing topic the coin should use.

The trait is intentionally narrow: a chain-id resolver, a sign
flow, a send flow, and a session pointer. Associated types let
each chain family pick its own parameter and return shapes
(EVM transaction objects, Cosmos sign-direct payloads, UTXO
PSBTs) without the WalletConnect subsystem needing to know
anything about the specific tickers, transaction encodings,
or contract addresses involved.

The binding rule is that the trait must remain chain-family
agnostic in this codebase: the subsystem must not gain
knowledge of EVM, Cosmos, or UTXO transaction structures.
Chain-specific logic belongs in the coin support module that
implements the trait.

The "session pointer" the trait exposes resolves to the **session
topic** — the settled-session topic that carries the
`wc_sessionRequest` signing traffic (§22.8.1), not the
establishment-phase pairing topic. (The two are linked: a settled
session records its originating pairing in its `pairing_topic`
field, §22.8.1.5.) An implementor resolves the session to use
from the coin's CAIP-2 chain id by selecting the settled session
whose agreed namespaces grant that chain (its `chains` list or a
matching CAIP-10 `accounts` entry, §22.8.1.4).

## 22.4 Protocol Role

The codebase fills the **relay-client / dApp** role of WC2 in
full and the **wallet** role not at all.

| Step                              | Codebase | External wallet |
|-----------------------------------|----------|------------------|
| Initiate pairing                  | yes      | no               |
| Send `wc_sessionPropose`          | yes      | no               |
| Receive `wc_sessionSettle`        | yes      | yes (sends)      |
| Send `wc_sessionRequest` (sign)   | yes      | no               |
| Sign and respond                  | no       | yes              |
| Broadcast the signed transaction  | depends on the WC method chosen   | usually yes |

There is no inbound-session settlement and no signing on behalf
of external dApps. Adding the wallet role would be a substantial
new feature and is not in scope for this subsystem.

### 22.4.1 Establishment-handshake mechanics

The propose → settle handshake follows the WC2 standard; the
following points are fixed by that standard and are stated here to
remove implementer guesswork:

- The proposer publishes `wc_sessionPropose` **on the pairing
  topic**, encrypted with the pairing symmetric key, with the
  standard `relays: [{ "protocol": "irn" }]` array and a
  `proposer` advertising the dApp metadata and the ephemeral
  x25519 public key. The dApp retains the matching ephemeral
  secret until the response arrives.
- The wallet's reply is a JSON-RPC **response** (it carries
  `result`/`error`, **no** `method`) delivered **on the pairing
  topic**, whose `result` carries the responder public key. The
  dApp completes the x25519/HKDF exchange of §22.6 against that
  key, derives the session topic, and subscribes to it.
- `wc_sessionSettle` then arrives **on the session topic** and is
  the point at which the settled `Session` is built and persisted.
- The settled session records the `encoding_algo` byte-string
  selector described normatively in §22.7.1 and R11. The selector
  defaults to **hex** when absent, switches to **base64** only for
  explicit required-wallet compatibility, and is independent of
  both the WC2 Type 0 envelope codec (§22.7) and the Cosmos
  binary-**field** rule of §22.8.1.2.
- The originating `pairing` record MAY be retained after settle;
  no teardown is required, and `wc_get_session` with
  `with_pairing_topic` resolves the session via its
  `pairing_topic` field regardless.

## 22.5 Session Storage

Sessions persist across process restarts. A single trait
abstracts the storage backend; the build chooses between two
implementations:

| Build target | Implementation                                              |
|--------------|-------------------------------------------------------------|
| Native       | SQLite via the workspace's async SQL abstraction            |
| Browser/WASM | IndexedDB via the workspace's IndexedDB wrapper             |

### 22.5.1 The `wc_session_persistence` setting

A single string setting in the node configuration
(`MM2.json`), `wc_session_persistence`, governs whether and how
sessions are **written** to durable storage. It controls
*saving only* and never affects loading. Its values are:

| Value       | Meaning                                                          |
|-------------|-----------------------------------------------------------------|
| `open`      | *(default)* Write the session record in the GLEEC-compatible    |
|             | plaintext on-disk format (§22.5.3), including the session        |
|             | symmetric key. The key is therefore stored **unencrypted at     |
|             | rest**.                                                          |
| `none`      | Never write sessions to storage. Existing rows are still read    |
|             | and used at startup, but are never rewritten or updated.         |
| `encrypted` | Reserved for a future encrypted-at-rest format. Not yet          |
|             | implemented; selecting it now must stop startup with an          |
|             | explanatory error.                                               |

The `open` default is a **documented Security-versus-compatibility
departure** (see `CODING_STANDARDS.md` §5.1 and the
"Security-versus-compatibility departures" subsection of
`COMPAT_SWITCHES.md`, and the `wc_session_persistence` row of
`GLEEC_COMPATIBILITY.md`). The bounded exposure it creates is
explicit: theft of a persisted session symmetric key permits
WalletConnect session hijack — soliciting a signing prompt from
the paired wallet — but **not** direct theft of funds. The
fund-controlling wallet seed/mnemonic is encrypted independently
(Argon2id / SLIP-0021) and is unaffected by this setting. This
exception is the minimum necessary to keep the on-disk format
byte-interchangeable with GLEEC KDF in both directions (GLEEC can
read records written here and this project can read GLEEC's
records).

> *Future-work note (to be carried as a code comment when the
> encrypted format ships): once `encrypted` is available and a
> stronger-security value can be offered, selecting the
> less-secure `open` value would at that point warrant a
> prominent runtime warning and possibly acknowledgement-gating.
> That warning is **not** added now — nothing better is yet
> offered — it is recorded only as deferred work (§22.9 D9).*

### 22.5.2 Loading is unconditional and format-autodetecting

On relay connect the loader reads **every** persisted session
record regardless of the setting value, auto-detecting the
on-disk format. Today only the `open` (plaintext) format exists;
the `encrypted` format is future. The future format is
distinguished from `open` on read by a distinct on-disk
discriminator (a renamed field/column or an explicit
format/version marker) so the loader can select the right
decoder; the exact discriminator shape is deferred (§22.9 D8).

Because loading is independent of the setting, the setting
enables an in-place migration path: run with `open` (plaintext on
disk), stop, set `encrypted`, restart — the existing plaintext
rows are still read correctly and are subsequently re-written in
the encrypted form. The setting decides only how (and whether)
rows are written, never whether they are read.

Lifecycle on relay connect:

1. Load every persisted session record, auto-detecting its
   on-disk format (load is unconditional; the setting is not
   consulted here).
2. Reconstruct each session's in-memory state fully from the
   record, including the session symmetric key, so the restored
   session can **decrypt** subsequent messages (§22.6).
3. Expire any record whose `expiry` (Unix epoch seconds) is in
   the past.
4. Re-subscribe to the session and pairing topics so that
   messages delivered while the relay was disconnected are
   processed.
5. When session state changes, write the record back — subject
   to `wc_session_persistence` (under `none`, the write is
   skipped).
6. On user-initiated disconnect, delete the record and
   unsubscribe.

### 22.5.3 On-disk record format (Interop / wire-format reuse, R29)

The `open` on-disk session record exists to be byte-interchangeable
with GLEEC KDF. It is therefore an **Interop / wire-format reuse**
fragment under clean-room rule R29: expression and function are
merged, because differing bytes break interoperability. Its only
authoritative source is the relicensed historical record (the
corpus forbidden under R8), so R31 applies: the externally-required
names below are embedded here, sanitized per R31 (authored prose,
only the names strictly necessary for interop, externally-identifying
JSON keys preserved exactly).

The native schema is a single table keyed by topic:

```sql
CREATE TABLE wc_session (
    topic   CHAR(32) PRIMARY KEY,
    data    TEXT     NOT NULL,
    expiry  BIGINT   NOT NULL
);
```

The browser schema is the same shape: a single object store
indexed by `topic`, holding the same serialized record.

- `topic` — session topic; primary key / index key.
- `expiry` — session expiry as **Unix epoch seconds**.
- `data` — the JSON-serialized session record.

The `data` payload carries the following JSON keys, which are
part of the externally-required on-disk identity and MUST be
emitted and consumed exactly as named for byte-interop:

| JSON key             | Carries                                            |
|----------------------|----------------------------------------------------|
| `topic`              | Session topic                                      |
| `subscription_id`    | Relay subscription id                              |
| `session_key`        | Session key material (see below)                   |
| `controller`         | Controlling party (wallet) descriptor              |
| `proposer`           | Proposing party (this dApp) descriptor             |
| `relay`              | Relay descriptor                                   |
| `namespaces`         | Agreed namespaces                                  |
| `propose_namespaces` | Proposed namespaces                                |
| `expiry`             | Expiry, Unix epoch seconds (mirrors the column)    |
| `pairing_topic`      | Pairing topic                                      |
| `session_type`       | Controller / Proposer role                         |
| `session_properties` | Optional wallet-reported session properties        |
| `active_chain_id`    | Optional active CAIP-2 chain id                    |
| `encoding_algo`      | Session byte-string selector (`Hex` / `Base64`)    |

For `encoding_algo`, the open-format semantic values are
`Hex` and `Base64`. A legacy or partial record with the key
omitted MUST be interpreted as `Hex` when the session is restored
or when a call site asks the WalletConnect handle to encode bytes
for that session. Any other value is invalid record data; a
conforming implementation MUST NOT silently reinterpret unknown
values as either `Hex` or `Base64`.

The session symmetric key is carried inside `session_key`, an
object with two keys:

| JSON key      | Carries                                                       |
|---------------|---------------------------------------------------------------|
| `sym_key`     | The 32-byte ChaCha20-Poly1305 session symmetric key. In the   |
|               | `open` format this is stored in plaintext, and it MUST        |
|               | round-trip so a restored session can decrypt (§22.6).         |
| `public_key`  | The local x25519 public key used in key derivation.           |

The values of `controller`, `proposer`, `relay`, and the
namespace entries are shaped by the external WalletConnect relay
SDK types (§22.1, Third-party-API-bound shape, R33) and are
serialized as those types dictate.

The binding rule is that the storage layer must remain a single
trait with one record per session; both backends store the same
serialized payload, and the `open`-format names above are fixed by
the GLEEC interop requirement. Schemas may be extended additively
but must remain compatible between backends and must not break
byte-interop with GLEEC in the `open` format.

## 22.6 Cryptography

The transport-layer cryptography is fully specified by WC2 and
is reproduced byte-for-byte. The codebase performs:

1. **Key exchange.** x25519 ECDH between an ephemeral
   `StaticSecret` and the peer's `PublicKey`, producing a 32-byte
   shared secret.
2. **Symmetric-key derivation.** HKDF-SHA256 over the shared
   secret with **empty salt** (`None`) and **empty info**
   (`&[]`), expanded to 32 bytes. The empty salt and info are
   mandated by the WC2 specification; they are not authorial
   choices and must be matched byte-for-byte by any
   interoperable implementation.
3. **Symmetric encryption.** ChaCha20-Poly1305 with the
   derived key, packaged in the canonical WC2 Type 0 envelope
   (version byte, salt, ciphertext, Poly1305 tag). The envelope
   codec is delegated to the external WalletConnect SDK crate
   referenced by Cargo; the project does not reimplement the
   envelope byte layout.

The symmetric key is redacted in any `Debug` output: the
formatter substitutes a fixed placeholder for the key bytes
rather than rendering them. The session-key type **shall
additionally implement zeroize-on-drop**; in the chapter-bound substrate the
implementation masks but does not zeroize, and this is named
explicitly in §22.9 as required follow-on work.

Persistence does not relax these in-memory protections. The
session symmetric key is part of the serialized/deserialized
on-disk record (§22.5.3): it is written into the `open`-format
record in plaintext at rest — the documented
Security-versus-compatibility departure — and on load it is read
back so the in-memory session key is **fully reconstructed**. A
restored session must be fully usable: it must be able to
**decrypt** inbound messages after a process restart, not merely
re-subscribe. The in-memory key value continues to be masked in
`Debug` and remains subject to the zeroize-on-drop obligation of
§22.9 regardless of how it is stored at rest.

## 22.7 Encrypted JSON-RPC Envelopes

Each WalletConnect message on the wire is a Type 0 envelope
wrapping a JSON-RPC payload. The envelope structure is:

| Field      | Size                | Notes                                  |
|------------|---------------------|----------------------------------------|
| Version    | 1 byte              | Type 0 = 0x00                          |
| Salt       | 32 bytes            | Per-message nonce material             |
| Ciphertext | variable            | ChaCha20-Poly1305 over JSON-RPC bytes  |
| Tag        | 16 bytes            | Poly1305 authentication tag            |

The codebase calls the external SDK's encode and decode entry
points to produce and consume envelopes; this is the only
normative WC2 Type 0 envelope codec. Wallet-specific compatibility
selectors in §22.7.1 MUST NOT alter the x25519/HKDF key
derivation, the Type 0 encrypted byte layout, or the SDK-defined
envelope text codec.

### 22.7.1 Session `encoding_algo` byte-string selector

Separate from the WC2 Type 0 codec, session state carries a
byte-to-text selector named `encoding_algo`. This selector is a
KDF compatibility contract for WalletConnect-owned call sites
that must turn raw bytes into JSON strings before or around a
`wc_sessionRequest` signing flow. It is not a generic WC2
negotiation parameter and it is not a replacement for the
external Type 0 envelope codec.

The required contract is:

1. **Interface.** The WalletConnect handle MUST expose an
   encoding operation equivalent to:
   `encode(session_topic, bytes) -> string`. A lower-level
   operation MAY instead accept an already-resolved
   `encoding_algo` plus bytes. Call sites that construct
   WalletConnect signing request payloads or apply a
   KDF-controlled byte-to-text conversion to already-enveloped
   bytes MUST use this operation rather than hard-coding a
   codec.
2. **Valid values.** The only valid semantic values are `Hex`
   and `Base64` (§22.5.3). `Hex` renders bytes as lowercase hex
   with no `0x` prefix. `Base64` renders bytes with the standard
   base64 alphabet and padding. Unknown values are invalid input,
   not a request to guess.
3. **Default.** If an otherwise usable session has no
   `encoding_algo` recorded, for example because a legacy stored
   record omits the key, the selector MUST default to `Hex`.
   The same default applies when the handle-level encoder is
   asked to encode bytes for a session whose selector key is
   absent. This default is observable and must be tested.
4. **Base64 trigger.** Base64 is allowed only for an explicit
   interoperability branch. The current required branch is the
   settled wallet whose WC2 app metadata `name` is `Keplr`; such
   a session MUST record/select `Base64`. All other wallet names
   select `Hex` unless a future compatibility entry is added
   because a named wallet or chain demonstrably requires base64.
   Base64 MUST NOT be selected merely because the CAIP namespace
   is `cosmos` or because a request method belongs to a broad
   chain family.
5. **Cosmos field-level independence.** This selector does not
   change the Cosmos binary-field rule in §22.8.1.2. The
   `authInfoBytes`, `bodyBytes`, account `pubKey`, account
   `address`, and Cosmos signature fields remain governed by
   their field-level contract. An implementation MAY reuse the
   selected semantic value when that field-level contract says to
   do so, but the two requirements must remain independently
   testable.

> **Upstream divergence (informative).** WalletConnect v2 does
> not negotiate a KDF-specific `encoding_algo` value. This project
> persists and applies the selector as a compatibility behavior
> for wallets whose request byte strings require a non-default
> encoding. The selector is a KDF interop rule, not a change to
> the WalletConnect Type 0 envelope specification.

A JSON-RPC payload on the WC channel has the standard shape:

```jsonc
{
  "jsonrpc": "2.0",
  "id":      <numeric message id>,
  "method":  "wc_sessionRequest",
  "params":  {
    "chainId": "eip155:1",
    "request": {
      "method": "eth_signTransaction",
      "params": [ /* method-specific */ ]
    }
  }
}
```

**Request/response correlation** uses a oneshot channel keyed
by the JSON-RPC message id. The send path registers the
oneshot under the id, the inbound router matches incoming
responses to pending ids and wakes the waiter, and a fixed
time-to-live (the chapter-bound five-minute default) caps the
wait if no response arrives. The TTL is currently a constant;
a per-call override is named as follow-on work in §22.9.

## 22.8 Chain Taxonomy and Request Methods

The subsystem models the multi-chain surface of WC2 through
CAIP-2 chain identifiers (`<family>:<reference>`). Three chain
families are recognised by the chapter-bound substrate:

| CAIP-2 family    | Meaning                                          |
|------------------|--------------------------------------------------|
| `eip155:<id>`    | Ethereum and EVM-compatible chains               |
| `cosmos:<id>`    | Cosmos SDK chains                                |
| `bip122:<hash>`  | UTXO chains (Bitcoin family, prefix of genesis)  |

Adding a new family is an additive change (a new enum variant
plus a new RPC submodule for the methods that family exposes).
Removing a family would be a breaking change to coin
implementors and is not envisaged.

The wire method names the subsystem is prepared to issue on
behalf of an integration are enumerated explicitly; the
mapping from internal variant to wire name is one-to-one and
total. The chapter-bound set is:

| Variant family   | Wire method name             | Chain family |
|------------------|------------------------------|--------------|
| Sign EVM tx      | `eth_signTransaction`        | eip155       |
| Send EVM tx      | `eth_sendTransaction`        | eip155       |
| EVM personal sign| `personal_sign`              | eip155       |
| Cosmos direct    | `cosmos_signDirect`          | cosmos       |
| Cosmos amino     | `cosmos_signAmino`           | cosmos       |
| Cosmos accounts  | `cosmos_getAccounts`         | cosmos       |
| UTXO accounts    | `getAccountAddresses`        | bip122       |
| UTXO send        | `sendTransfer`               | bip122       |
| UTXO sign PSBT   | `signPsbt`                   | bip122       |
| UTXO personal    | `personal_sign` (UTXO route) | bip122       |

`cosmos_signAmino` is the Ledger-compatible path (the Cosmos
Ledger app supports Amino-JSON sign payloads only);
`cosmos_signDirect` is the default for software wallets.

`eth_signTypedData_v4` is intentionally not in the enum at the
time of writing; it is a common EVM method and is expected to
be added when the first integrator needs it. Adding it is an
additive enum + match-arm change.

## 22.8.1 Request-method wire payloads and session-info field spellings (Interop / wire-format, R29/R33)

This subsection pins the **clean-channel wire payload shapes** for
the ten §22.8 request methods, the CAIP-2 reference formats per
chain family, and the per-account field spellings of the
`session-info` record (RP6, §22.9A.2). Every field name, JSON
type, and optionality below is **externally dictated** by a
public standard — WalletConnect v2, the CAIP family, the Reown
multichain RPC reference, the Ethereum/EIP standards, the Cosmos
SDK signing schemes, and the BIP-174 PSBT format — and is binding
on a conforming integration **as a wire contract (R29/R33)**.

The internal Rust realisation of these payloads (the concrete
type names, field identifiers, helper decomposition, and module
placement chosen by the implementor) is **informative (R36)**: a
re-derivation that emits the *same wire shapes* under *different*
internal naming is fully conformant. The associated parameter and
return types of the §22.3 trait (RP2) are the natural home for
the chain-family payloads, but their internal spelling is not
constrained here.

All ten methods are carried inside the WC2 `wc_sessionRequest`
envelope: the method string (§22.8) goes in the request
`method` field and the param shape below goes in the request
`params` field; the response shape below is the JSON-RPC
`result`.

### 22.8.1.1 EVM family (`eip155`)

**`eth_signTransaction` / `eth_sendTransaction`** — `params` is a
JSON array containing exactly one transaction object. Field
spellings (Ethereum JSON-RPC / EIP-1559 dictated):

| Field                  | JSON type     | Req/Opt | Meaning                                                 |
|------------------------|---------------|---------|---------------------------------------------------------|
| `from`                 | string (hex)  | required| sender 20-byte address, `0x`-prefixed                   |
| `to`                   | string (hex)  | optional| recipient address; omitted for contract creation        |
| `data`                 | string (hex)  | optional| call data / contract input (`input` accepted as alias)  |
| `value`                | string (hex)  | optional| wei amount as `0x` quantity                             |
| `gas`                  | string (hex)  | optional| gas limit as `0x` quantity (`gasLimit` accepted alias)  |
| `gasPrice`             | string (hex)  | optional| legacy gas price; mutually exclusive with the 1559 pair |
| `maxFeePerGas`         | string (hex)  | optional| EIP-1559 max fee per gas                                |
| `maxPriorityFeePerGas` | string (hex)  | optional| EIP-1559 priority fee per gas                           |
| `nonce`                | string (hex)  | optional| account nonce as `0x` quantity                          |
| `chainId`              | string (hex)  | optional| target chain id as `0x` quantity                        |

Response: `eth_signTransaction` returns the signed raw
transaction as a `0x`-prefixed hex string (RLP-encoded);
`eth_sendTransaction` returns the broadcast transaction hash as a
`0x`-prefixed 32-byte hex string.

**`personal_sign`** (EIP-191) — `params` is a JSON array of two
strings in the order `[challenge, address]`, where `challenge`
is the message hex-encoded with a `0x` prefix and `address` is
the signer's `0x`-prefixed address. Response: a `0x`-prefixed
65-byte signature hex string (`r ‖ s ‖ v`).

The optionality column above is the WC2/Ethereum wire contract
seen by the wallet, not a constraint on the dApp: a conforming
integration MAY populate unconditionally any optional field whose
value it already knows (for example emitting `value` as `0x0`
when zero, and always emitting `gas`), and MAY omit an optional
field to defer it to the wallet (for example omitting `gasPrice`
or the EIP-1559 pair so the wallet prices the transaction). The
legacy `gasPrice` and the `maxFeePerGas` / `maxPriorityFeePerGas`
pair remain mutually exclusive in any single request.

### 22.8.1.2 Cosmos family (`cosmos`)

**`cosmos_signDirect`** (SignMode `SIGN_MODE_DIRECT`) — `params`
is a JSON object. Field spellings (Cosmos SDK / protobuf
SignDoc dictated):

| Field                   | JSON type | Req/Opt | Meaning                                                 |
|-------------------------|-----------|---------|---------------------------------------------------------|
| `signerAddress`         | string    | required| bech32 signer address                                   |
| `signDoc`               | object    | required| the protobuf SignDoc, fields below                      |
| `signDoc.chainId`       | string    | required| chain id (e.g. `cosmoshub-4`)                           |
| `signDoc.accountNumber` | string    | required| on-chain account number, decimal as string             |
| `signDoc.authInfoBytes` | string    | required| serialized `AuthInfo` protobuf, binary-encoded (below)  |
| `signDoc.bodyBytes`     | string    | required| serialized `TxBody` protobuf, binary-encoded (below)    |

Response: a JSON object `{ "signature": { "pub_key": { "type",
"value" }, "signature": <base64> }, "signed": { "chainId",
"accountNumber", "authInfoBytes", "bodyBytes" } }` — the `signed`
object echoes the (possibly wallet-normalised) SignDoc that was
actually signed and MUST be used for broadcast.

**`cosmos_signAmino`** (SignMode `SIGN_MODE_LEGACY_AMINO_JSON`,
Ledger-compatible) — `params` is a JSON object. Field spellings
(Cosmos Amino `StdSignDoc` / ADR-036 dictated):

| Field                     | JSON type | Req/Opt | Meaning                                              |
|---------------------------|-----------|---------|------------------------------------------------------|
| `signerAddress`           | string    | required| bech32 signer address                                |
| `signDoc`                 | object    | required| the Amino `StdSignDoc`, fields below                 |
| `signDoc.chain_id`        | string    | required| chain id (note snake_case, Amino dictated)           |
| `signDoc.account_number`  | string    | required| account number, decimal as string                    |
| `signDoc.sequence`        | string    | required| account sequence, decimal as string                  |
| `signDoc.fee`             | object    | required| `{ "amount": [{ "denom", "amount" }], "gas": <str> }`|
| `signDoc.msgs`            | array     | required| Amino-encoded message array                          |
| `signDoc.memo`            | string    | required| memo (may be empty string)                           |

Response: a JSON object `{ "signature": { "pub_key", "signature"
(base64) }, "signed": <StdSignDoc> }`; the `signed` field echoes
the canonicalised Amino doc actually signed.

**`cosmos_getAccounts`** — `params` is an empty object (or
omitted). Response: a JSON array of account entries, each
`{ "address": <bech32 string>, "algo": <string, e.g.
"secp256k1">, "pubkey": <base64 string> }`.

**Binary-field encoding rule (dictated by wallet metadata).** For
Cosmos byte-valued fields (`authInfoBytes`, `bodyBytes`,
public-key bytes, signature bytes) the encoding is selected by
the paired wallet's WC2 app-metadata `name`: when the metadata
`name` is the value `Keplr` the bytes are **base64**-encoded; for
all other wallets the bytes are **hex**-encoded. This selection
is keyed solely on the dictated metadata `name` value and applies
uniformly to the binary fields above.

**Broadcast assembly across both sign modes.** Whichever sign
mode is used, the broadcast transaction is the protobuf `TxRaw`
(`bodyBytes`, `authInfoBytes`, `signatures`) — the WC2 sign
methods return a signature, never a broadcastable transaction,
because no Cosmos wallet-broadcast method exists (the integration
broadcasts through the coin's own node RPC, §22.3 sign-and-send).
For `cosmos_signDirect` the broadcast `bodyBytes` / `authInfoBytes`
are taken from the result's `signed` echo (which the wallet may
have normalised). For `cosmos_signAmino` the signature is computed
over the Amino `StdSignDoc`, but the broadcast envelope is still
protobuf: the integration reuses the protobuf `bodyBytes` and
re-derives `authInfoBytes` with SignMode
`SIGN_MODE_LEGACY_AMINO_JSON`. Consequently the `cosmos_signAmino`
`msgs` array is the Amino-JSON representation of the same messages
carried in the protobuf `bodyBytes`; supplying that Amino-JSON
representation is the dApp's responsibility (the protobuf `Any`
form does not self-describe its Amino-JSON encoding), and it is
required only on the Ledger amino path selected by §22.8.1.7.

### 22.8.1.3 UTXO family (`bip122`)

The UTXO methods follow the Reown Bitcoin multichain RPC
reference.

**`getAccountAddresses`** — `params` is an empty/selector object.
Response: a JSON array of address entries:

| Field        | JSON type | Req/Opt | Meaning                                          |
|--------------|-----------|---------|--------------------------------------------------|
| `address`    | string    | required| the address                                      |
| `publicKey`  | string    | optional| compressed public key, hex                       |
| `path`       | string    | optional| BIP-32 derivation path (e.g. `m/84'/0'/0'/0/0`)  |
| `intention`  | string    | optional| address purpose hint (e.g. `payment`)            |

A wallet typically returns addresses across BIP-44/49/84/86
derivation purposes; the integration filters to the purposes the
coin enabled. The companion `bip122_addressesChanged` session
event carries the same entry shape.

**`sendTransfer`** — `params` is a JSON object:

| Field              | JSON type | Req/Opt | Meaning                                |
|--------------------|-----------|---------|----------------------------------------|
| `account`          | string    | optional| source account selector                |
| `recipientAddress` | string    | required| destination address                    |
| `amount`           | string    | required| amount in the chain's base unit string |
| `changeAddress`    | string    | optional| explicit change address                |
| `memo`             | string    | optional| memo / op-return payload               |

Response: a JSON object `{ "txid": <string> }` carrying the
broadcast transaction id.

**`signPsbt`** (BIP-174) — `params` is a JSON object:

| Field         | JSON type | Req/Opt | Meaning                                                    |
|---------------|-----------|---------|------------------------------------------------------------|
| `account`     | string    | optional| account selector                                           |
| `psbt`        | string    | required| the PSBT, base64-encoded (BIP-174)                         |
| `signInputs`  | array     | optional| inputs to sign: `[{ "address", "index", "sighashTypes"? }]`|
| `broadcast`   | bool      | optional| if true the wallet also broadcasts                         |

Response: a JSON object `{ "psbt": <signed PSBT base64>, "txid":
<string, present only when broadcast> }`.

**UTXO message signing** — the bip122 message-signing wire method
name is **`signMessage`** (Reown Bitcoin RPC reference);
`params` is a JSON object `{ "account"|"address": <string>,
"message": <string> }`; response `{ "signature": <base64
string>, "address": <string> }`.

> **Upstream divergence (informative).** The §22.8 taxonomy table
> labels this method `personal_sign (UTXO route)`. The actual
> on-wire bip122 method string is `signMessage`, not
> `personal_sign` (the latter is the EVM/`eip155` method). A
> conforming UTXO integration MUST emit `signMessage`. The §22.8
> label is a descriptive grouping, not the wire string.

**Integration notes (informative).** For `signMessage` the
integration supplies the coin's own enabled signing address as the
`address` selector. For `signPsbt` the `txid` field is treated as
optional on the sign-only path (`broadcast = false`) and required
on the sign-and-broadcast path (`broadcast = true`); a stray
`txid` returned on the sign-only path is accepted and ignored. For
`getAccountAddresses` the "filter to the purposes the coin
enabled" step is a post-parse selection over the returned entries
(keyed on the optional `path` / `intention` hints when present);
an integration MAY return all entries unfiltered when the coin
imposes no purpose restriction.

### 22.8.1.4 CAIP-2 chain-reference formats

CAIP-2 chain ids take the form `<namespace>:<reference>`. The
reference segment per family:

| Namespace | Reference rule (CAIP-2 dictated)                                                                              | Example                                   |
|-----------|--------------------------------------------------------------------------------------------------------------|-------------------------------------------|
| `eip155`  | the EVM chain id in **decimal**                                                                               | `eip155:1`                                |
| `cosmos`  | the Cosmos chain id / chain-name string                                                                       | `cosmos:cosmoshub-4`                      |
| `bip122`  | the **first 32 hex characters (16 bytes)** of the genesis block hash, in conventional big-endian hex display | `bip122:000000000019d6689c085ae165831e93`|

For `bip122` the reference is derived by hex-encoding the genesis
block hash in its conventional (big-endian) display order and
truncating to the leading 32 hex characters (16 bytes); the
truncation length is fixed by CAIP-2 and is not implementation
discretion.

A UTXO coin need not store its genesis hash: the integration MAY
derive it at call time from the active RPC backend — the native
backend returns the genesis block hash already in display order,
while the Electrum backend yields the genesis header whose
double-SHA256 (internal order) is reversed for display — and then
apply the fixed 16-byte truncation above. A cached/stored genesis
hash is an optional optimisation, not a requirement.

CAIP-10 account ids extend this as
`<namespace>:<reference>:<address>` and populate the WC2
namespace `accounts` arrays described next.

### 22.8.1.5 `session-info` record field spellings (RP6)

The top-level `session-info` record (RP6) serialises with exactly
these field spellings (they match the RP6 tokens verbatim):

| Field           | JSON type | Meaning                                              |
|-----------------|-----------|------------------------------------------------------|
| `topic`         | string    | session topic                                        |
| `metadata`      | object    | wallet-reported WC2 app metadata (below)             |
| `pairing_topic` | string    | originating pairing topic                            |
| `namespaces`    | object    | map: agreed CAIP namespace → WC2 namespace record    |
| `expiry`        | number    | session expiry, Unix epoch **seconds**               |

`metadata` is the WC2 `Metadata` object with fields `name`
(string), `description` (string), `url` (string), and `icons`
(array of strings).

Each value in `namespaces` is the WC2 standard namespace record:

| Field      | JSON type | Req/Opt | Meaning                                           |
|------------|-----------|---------|---------------------------------------------------|
| `accounts` | array     | required| CAIP-10 account ids agreed for this namespace     |
| `methods`  | array     | required| request method strings agreed for this namespace  |
| `events`   | array     | required| event strings agreed for this namespace           |
| `chains`   | array     | optional| CAIP-2 chain ids covered by this namespace        |

### 22.8.1.6 Per-account detail record (`sessionProperties.keys`)

Richer per-account detail (signing algorithm, public key, and the
advisory hardware-wallet flags referenced by RP6) is carried by
the WC2 `sessionProperties` object delivered at session-settle,
under a `keys` field. The `keys` value is an **array** of
account-detail records (some wallets, notably Keplr, encode this
array as a JSON-**string**; a conforming reader MUST accept both a
JSON array and a JSON-string-encoded array). Each entry uses the
following dictated field spellings (Keplr `Key` wire shape):

| Field                | JSON type | Req/Opt | Meaning                                                  |
|----------------------|-----------|---------|----------------------------------------------------------|
| `chainId`            | string    | required| CAIP chain id the account is bound to                    |
| `name`               | string    | optional| wallet-assigned account label                            |
| `algo`               | string    | required| signing algorithm (e.g. `secp256k1`)                     |
| `pubKey`             | string    | required| account public key (encoding per §22.8.1.2 rule)         |
| `address`            | string    | required| raw account address bytes (encoding per §22.8.1.2 rule)  |
| `bech32Address`      | string    | optional| bech32-formatted address                                 |
| `ethereumHexAddress` | string    | optional| EVM-style `0x` hex address (EVM-compatible Cosmos chains)|
| `isNanoLedger`       | bool      | required| advisory: account is on a Ledger Nano hardware device    |
| `isKeystone`         | bool      | optional| advisory: account is on a Keystone hardware device       |

The `isNanoLedger` and `isKeystone` flags are the advisory
hardware-wallet indicators named in RP6. (There is no separate
"Keplr" boolean: the Keplr wallet is identified by the metadata
`name` value `Keplr`, per the §22.8.1.2 encoding rule.)

### 22.8.1.7 Cosmos amino-vs-direct selection rule (RP3)

The Cosmos integration selects between `cosmos_signAmino` and
`cosmos_signDirect` from a **dictated wire signal**, not an
internal heuristic:

- **Signal source.** The per-account `isNanoLedger` flag of the
  matching `sessionProperties.keys` entry (§22.8.1.6), surfaced
  from the session-settle metadata.
- **Rule.** When `isNanoLedger` is `true` for the signing account,
  the integration MUST use **`cosmos_signAmino`** — the Cosmos
  Ledger application supports only the Amino-JSON sign mode and
  rejects protobuf `SIGN_MODE_DIRECT`. Otherwise the integration
  uses **`cosmos_signDirect`**.

This rule is stated behaviourally; it is satisfied by emitting the
correct method string for the dictated flag value, independent of
any internal Rust naming.

## 22.9 Binding Requirements and Deferred Work

The following are **binding rules** for this subsystem and any
coin support modules that integrate with it:

R1. **Coin agnosticism.** The WalletConnect subsystem must not
    depend on any coin support module. Chain-family-specific
    transaction encoding lives in the integrating coin module.

R2. **Single trait integration boundary.** Coin integration
    must be expressed through the integration trait of §22.3.
    No coin module may reach into the subsystem's internals.

R3. **Spec-byte-faithful crypto.** The HKDF salt and info, the
    Type 0 envelope layout, and the JSON-RPC message shape are
    set by the WC2 specification and are not authorial. Any
    change that diverges from spec bytes is a bug.

R4. **Storage uniformity.** Both storage backends must expose
    the same trait, the same row shape, and the same lifecycle
    (load -> expire -> re-subscribe -> update -> delete).

R5. **Zeroize-on-drop for session-key material.** The
    session-key type shall implement zeroize-on-drop. At the
    time of writing the type masks the key in `Debug` only;
    closing this gap is required follow-on work.

R6. **Public dispatcher integration.** The codebase shall expose
    the subsystem's user-facing operations (start pairing,
    list sessions, drop session) through the public RPC
    dispatcher. The subsystem's public handle methods are the
    intended targets of those RPCs; the dispatcher wiring is
    required follow-on work.

R7. **Save governed by `wc_session_persistence`; save-only
    semantics.** Whether and how a session record is *written*
    to storage is governed solely by the `wc_session_persistence`
    setting: `open` writes the GLEEC-compatible plaintext record
    (§22.5.3), `none` never writes, `encrypted` is reserved and
    must stop startup with an explanatory error until implemented.
    The setting MUST NOT affect loading.

R8. **Unconditional, format-autodetecting load.** On relay
    connect the loader MUST read every persisted session record
    irrespective of the setting, auto-detecting the on-disk
    format (today only the `open` plaintext format). This is what
    enables the `open` → `encrypted` migration path of §22.5.2.

R9. **Restored sessions must be decryptable.** The session
    symmetric key MUST round-trip through the persisted record so
    that, on load, the in-memory session key is fully
    reconstructed and the restored session can decrypt subsequent
    messages — not merely re-subscribe. The in-memory key remains
    masked in `Debug` (and subject to R5) regardless of its
    at-rest representation.

R10. **`open`-format byte-interop.** The `open` on-disk record is
    an Interop / wire-format reuse fragment (R29): its
    externally-identifying names (§22.5.3) MUST stay
    byte-interchangeable with GLEEC KDF in both directions. Schema
    changes must be additive and must not break that interop.

R11. **Session `encoding_algo` selection.** The WalletConnect
    handle's session byte-string encoder SHALL implement §22.7.1:
    only `Hex` and `Base64` are valid; omitted or absent
    selectors default to `Hex`; the required-wallet branch selects
    `Base64` only for explicit interop needs; and this selector
    does not amend the WC2 Type 0 codec or the Cosmos field-level
    binary encoding rules.

The following items were **required ports** (binding driving-spec,
specified normatively in §22.9A) plus genuinely-optional
follow-on work; the required-port items are flagged as such:

D1. **[REQUIRED PORT — §22.9A.1, implemented]** At least one coin
    support module per the EVM, Cosmos, and UTXO families
    implements the `WcCoinSigner` integration trait
    (`mm2src/coins/eth/wc_integration.rs`,
    `mm2src/coins/utxo/wc_integration.rs`,
    `mm2src/coins/tendermint/wc_integration.rs`).

D2. **[REQUIRED PORT — §22.9A.2, implemented]** The five public RPC
    handlers of §22.9A.2 (see R6) are registered in the public
    dispatcher (`wc_new_connection`, `wc_get_sessions`,
    `wc_get_session`, `wc_delete_session`, `wc_ping_session`).

D3. The session-key type shall acquire `Zeroize` /
    `ZeroizeOnDrop` (see R5).

D4. Wallet-type detection is currently heuristic: the subsystem
    infers whether the peer is a Ledger hardware wallet or the
    Keplr wallet from advisory fields the peer reports in its
    session/pairing metadata. These heuristics are documented but
    not normative; a more robust capability-based detection is
    desirable.

D5. CAIP-10 account-address parsing currently uses a simple
    delimiter split. Stricter validation would reject malformed
    wallet responses earlier.

D6. No defence-in-depth message deduplication is implemented;
    the project relies on the relay to deduplicate. A local
    dedup layer is desirable.

D7. The response time-to-live is a constant. A per-call override
    is desirable.

D8. The `encrypted` value of `wc_session_persistence` is reserved
    but not yet implemented: an encrypted-at-rest on-disk record
    format, plus the distinct on-disk discriminator (a renamed
    field/column or an explicit format/version marker) that lets
    the unconditional loader auto-detect format and select the
    right decoder. The discriminator's exact shape is deferred
    (TODO); it must not be over-specified before the format is
    designed. Until then, selecting `encrypted` must stop startup
    with an explanatory error.

D9. When the encrypted format of D8 ships and a stronger-security
    value can be offered, selecting the less-secure `open` value
    should at that point emit a prominent runtime warning and may
    be acknowledgement-gated. This warning is intentionally **not**
    added now (nothing better is yet offered); it is recorded here
    as future work only.

D10. **[REQUIRED PORT — §22.9B, not implemented]** Signing is not
    reachable by a user. The §22.3 trait implementors have exactly
    one consumer, the integration layer itself; no activation
    policy, withdrawal route, or swap path calls them, and EVM
    activation refuses the WalletConnect signing policy. The port
    needed is the call-site plumbing of §22.9B — RP8 activation
    acceptance, the delegated-signing wiring behind withdrawal and
    the legacy swap HTLC paths for the families RP9 marks
    reachable, RP10's watcher gate, and RP11.1's early rejection
    for the paths that cannot be served. The subsystem and the
    trait implementors themselves need no redesign. RP9, cited
    here for the families to wire first, is an **informative**
    record of the reference release (§22.9B.4) and not a binding
    scope: what this item requires is the §22.9B rules named
    above, and a port that makes more operations reachable than
    RP9 records is conformant — D11 asks for exactly that.

D11. The UTXO withdrawal refusal recorded in RP9 is a reference gap,
    not a contract: reloaded should make withdrawal reachable for a
    key-less UTXO coin through the same delegated signing operation
    the swap path uses (§22.9B.4, finding 1), coordinated with
    [Chapter 49](49-withdrawal-task-path.md). Both references this
    item follows land in **informative** material: the refusal is
    a negative cell in RP9's reference-release table, and
    §22.9B.4's findings are informative code-quality observations.
    Neither binds an implementation — which is the point, since
    this item exists to say that the refusal must not be ported.
    The requirement is the one stated in this item.

## 22.9A Required Port — Coin Integration and Public RPC Surface (driving-spec)

**STATUS.** *Implemented.* The capabilities in this section were
**required** at chapter authoring time and have since been
ported. The WalletConnect library crate provides the transport
cryptography, JSON-RPC payload types, and persistence layers; the
session-establishment handshake — propose-publish on the pairing
topic, deriving the session key from the responder public key per
§22.6, and building/persisting the settled session (AC1) — is
implemented rather than stubbed; the §22.3 integration trait
(`WcCoinSigner`) is present; the per-family coin integration
(§22.9A.1) has at least one implementor per chain family; and the
public RPC dispatcher surface (§22.9A.2) is registered — all
emitting the clean-channel wire payloads pinned in §22.8.1.

### 22.9A.1 Coin-integration requirement

**RP1.** At least the following chain families MUST implement the
§22.3 integration trait so that coins of each family can sign and
broadcast through a WalletConnect-paired external wallet:

| Chain family            | CAIP-2 namespace | Request methods (§22.8) it drives                                   |
|-------------------------|------------------|---------------------------------------------------------------------|
| EVM (Ethereum & EVM)    | `eip155`         | `eth_signTransaction`, `eth_sendTransaction`, `personal_sign`       |
| Cosmos / Tendermint     | `cosmos`         | `cosmos_signDirect`, `cosmos_signAmino`, `cosmos_getAccounts`       |
| UTXO (Bitcoin family)   | `bip122`         | `getAccountAddresses`, `sendTransfer`, `signPsbt`, `personal_sign`  |

**RP2.** Each integration MUST remain in its own coin support
module and MUST NOT add chain-family knowledge to the
WalletConnect subsystem (R1, R2). The associated-type choices of
§22.3 carry the chain-specific transaction/sign-payload shapes
(EVM transaction objects, Cosmos sign-direct / amino payloads,
UTXO PSBTs).

**RP3.** The Cosmos integration MUST select `cosmos_signAmino`
when the paired wallet is the Cosmos hardware-wallet (Ledger) app
and `cosmos_signDirect` otherwise, consistent with the §22.8
taxonomy; the wallet-type signal comes from the session/pairing
metadata surfaced by §22.2.

### 22.9A.2 Public RPC dispatcher surface

**RP4.** The following **five top-level JSON-RPC v2 methods** MUST
be registered in the public dispatcher. These method strings are
the wire contract:

| Method               | Request fields                                                       | Response shape                              |
|----------------------|---------------------------------------------------------------------|---------------------------------------------|
| `wc_new_connection`  | `required_namespaces` (JSON object), `optional_namespaces` (JSON object, optional) | `{ "url": <wc: URI string>, "pairing_topic": <topic string> }` |
| `wc_get_sessions`    | none (optional empty object accepted)                               | `{ "sessions": [ <session-info>, … ] }`     |
| `wc_get_session`     | `topic` (string), `with_pairing_topic` (bool, optional, default false) | `{ "session": <session-info> \| null }` |
| `wc_delete_session`  | `topic` (string)                                                    | empty object `{}`                           |
| `wc_ping_session`    | `topic` (string)                                                    | `{ "result": <status string> }`             |

**RP5.** `wc_new_connection` MUST initiate a new pairing using the
caller-supplied namespace requirements, returning the `wc:` URI
(§22.2) for the caller to present to a wallet and the pairing
topic. `wc_delete_session` MUST perform the WC2 session delete
(§22.2), remove the persisted record subject to
`wc_session_persistence` (§22.5), and unsubscribe.
`wc_ping_session` MUST issue a WC2 session ping and report
success/failure; the `result` string is a human-readable status
whose exact wording is NOT part of the contract.

**RP6 — `session-info` wire shape.** The session record returned
by `wc_get_session` / `wc_get_sessions` MUST serialise with at
least these fields:

| Field           | Type                                              |
|-----------------|---------------------------------------------------|
| `topic`         | session topic string                              |
| `metadata`      | wallet-reported app-metadata object               |
| `pairing_topic` | pairing topic string                              |
| `namespaces`    | object: agreed CAIP namespace → namespace record  |
| `expiry`        | session expiry, Unix epoch seconds                |

The precise serialisation is pinned by §22.8.1.5: the
`session-info` record carries **exactly** the five top-level
fields above, and each `namespaces` value is the standard WC2
namespace record (`accounts` / `methods` / `events` / `chains`).
The richer per-account detail (chain id, address, signing
algorithm, public key, and the advisory hardware-wallet flags) is
**not** a field of `session-info`: it is delivered separately at
session-settle as the WC2 `sessionProperties.keys` structure
(§22.8.1.6) and is consumed internally by the signing
integrations (for example the amino-vs-direct selection of
§22.8.1.7). Where this paragraph's looser "at least" wording and
the earlier prose about namespace records diverge from §22.8.1.5,
§22.8.1.5/.6 govern.

**RP7 — Error envelope.** The WalletConnect RPC handlers MUST
report failures through the project's typed-error envelope
(`error_type` / `error_data`) with three wire `error_type`
tokens distinguishing: an initialisation/precondition failure (a
client-error / 400 condition), a session-request failure, and a
generic internal failure (both server-error / 500 conditions).
The human-readable messages each token carries are diagnostic and
NOT part of the contract. A request that names an unknown session
topic (for example `wc_get_session`, `wc_ping_session`, or
`wc_delete_session` for a topic with no live session) is an
initialisation/precondition condition and maps to the
client-error / 400 token.

### 22.9A.3 Acceptance criteria

- AC1. With the WalletConnect subsystem enabled, a caller can
  drive a full pair → sign → disconnect cycle for at least one
  coin in each of the three chain families of RP1 using only the
  RP4 method surface.
- AC2. `wc_new_connection` returns a spec-conformant `wc:` URI
  (§22.2) and a pairing topic; the URI is delivered verbatim.
- AC3. `wc_get_sessions` enumerates every live session and
  `wc_get_session` resolves a single session by topic (and, when
  `with_pairing_topic` is set, by pairing topic), each with the
  RP6 wire shape.
- AC4. `wc_delete_session` tears down the session, deletes its
  persisted record subject to `wc_session_persistence`, and
  subsequent `wc_get_session` for that topic returns
  `{ "session": null }`.
- AC5. The coin integrations add no chain-family knowledge to the
  WalletConnect subsystem (RP2 / R1 / R2 hold after the port).
- AC6. A restored or constructed session whose stored
  `encoding_algo` key is omitted or absent still uses the `Hex`
  default for the handle-level byte encoder. For bytes
  `[0x01, 0x02, 0x03, 0xff]`, the encoder returns `010203ff`.
- AC7. A settled session on the required-wallet branch
  (currently wallet metadata `name = "Keplr"`) records/selects
  `Base64`. For bytes `[0x01, 0x02, 0x03, 0xff]`, the encoder
  returns `AQID/w==`; a non-required wallet with the same chain
  namespace returns the AC6 hex value. The test must also assert
  that this branch does not change the §22.8.1.2 Cosmos
  field-level contract.

## 22.9B Reachability of Signing from User-Facing Entry Points (driving-spec)

**STATUS.** *Required port — not implemented.* The subsystem, the
§22.3 integration trait, and one trait implementor per chain family
are present (§22.9A, D1), but in this repository nothing beyond the
integration layer calls them: no activation policy, no withdrawal
route, and no swap path reaches a `WcCoinSigner` implementation, and
EVM activation refuses the WalletConnect signing policy outright.
The subsystem can therefore **pair but not sign**. This section
states the user-facing contract a port must satisfy, and records
informatively how far the reference release carries it.

> **Clearance (chapter 01 T1, R40).** This section was authored by
> the KDF Spec Reader under the chapter-01 two-team workflow and
> **passed the KDF Dirty Gate** on 2026-10-17 before being relied
> on (PASS, finding list empty). Its clearance is therefore
> *complete* and not pending: nothing in this section is a
> candidate awaiting a gate, and the STATUS line above reports port
> status, which is a different claim. The extent, method and
> measure of that pass are recorded once, in the *Dirty Gate
> clearance* item of §22.12, and are not restated here.

> **Binding versus informative in this section.** The section
> carries both kinds of material and this note changes neither.
> Marked **informative**, and binding nothing: RP9's per-operation
> reachability table, which records what the reference release
> reaches today; the whole of §22.9B.4, both its release
> designations and its code-quality findings; and each note headed
> *(informative)*. Everything else is stated as requirement. Where
> a requirement or a deferral points at informative material, it
> now says so at the point of reference, so that a reader following
> the pointer is not left to infer the status of what they land on.

The chain-namespace and request-method sets of §22.8 need no change
for this port: the reference release exposes the same three CAIP-2
namespaces and the same ten method strings already specified there.
The gap is entirely in the call sites, not in the taxonomy, the
wire payloads, or the trait shape.

### 22.9B.1 Activation-policy acceptance

**RP8.** A coin shall be activatable against an already-settled
WalletConnect session by naming that session in the activation
request's key-source policy field.
[Chapter 55](55-coin-activation-wire-contract.md) R55.5.11 defers
this form to the present chapter. Two JSON shapes are in use for
the same concept, and both are part of the accepted wire surface:

| Family | Request field | Accepted JSON for a WalletConnect key source | Already specified in |
|--------|---------------|-----------------------------------------------|----------------------|
| EVM / EVM-family | `priv_key_policy` | adjacently tagged: `{"type": "WalletConnect", "params": {"session_topic": "<session topic>"}}` | [Chapter 35](35-evm-v2-activation-rpcs.md) R35.1.4 |
| Tendermint / Cosmos | `activation_params` | adjacently tagged: `{"type": "WalletConnect", "params": {"session_topic": "<session topic>"}}` | [Chapter 36](36-tendermint-v2-activation-rpcs.md) §36 activation-params form 2 |
| UTXO | `priv_key_policy` | **externally** tagged: `{"WalletConnect": {"session_topic": "<session topic>"}}` | this chapter (deferred to it by [Chapter 55](55-coin-activation-wire-contract.md) R55.5.11) |

The EVM and Tendermint forms are already bound by chapters 35 and
36 and are reproduced here only so the three families can be read
side by side; those chapters govern their own request shapes. The
UTXO form is bound here.

**RP8.1.** The topic named is the **settled session** topic of
§22.3, not a pairing topic. Activation shall fail with a structured
error, not a panic, when no live session carries that topic or when
the session's agreed namespaces do not grant the coin's CAIP-2
chain.

**RP8.2.** The EVM-family route shall accept the policy **only**
when an explicit chain id is resolvable for the coin, and shall
refuse it on the **pre-v2 legacy** EVM enable route (the old
`enable`-style activation that carries no explicit chain id),
with a structured unsupported-policy error. Both v2 EVM activation
forms — the one-shot call and the task-based call — accept it, as
chapter 35 R35.1.4 already records.

**RP8.2a.** The UTXO form is accepted on the **task-based** UTXO
activation route, and the coin configuration must carry both the
CAIP-2 chain id in its protocol data and a derivation path, since
the derivation path is what selects which of the wallet's reported
addresses becomes the activated one (RP8.3). A message-signing
prefix must also be configured, because it is required for the
public-key-recovery fallback of RP8.3.

**RP8.3.** Activation under this policy learns the address and
public key **from the wallet**, never from a local secret:

- UTXO: request the wallet's account addresses, select the entry
  whose reported derivation path equals the coin's configured full
  derivation path; if the wallet reports no derivation paths at
  all, fall back to its first entry and log that the selection was
  positional; if it reports paths but none match, fail. When the
  selected entry carries no public key, recover one by asking the
  wallet to sign a fixed short message and recovering the key from
  the compact signature against the coin's configured
  message-signing prefix. A hex **or** base64 signature encoding
  shall both be accepted.
- EVM: recover the account's uncompressed public key and address
  from a wallet message signature, then derive the compressed form.
- Cosmos: obtain the account public key from the wallet's account
  enumeration method.

> **Upstream divergence (informative).** The two tagged-enum JSON
> shapes in RP8 are not externally dictated; they are an accident
> of the reference carrying a family-generic key-source type
> alongside separate EVM- and Tendermint-specific ones, only the
> latter two of which acquired a `type` discriminator. Accepting
> both shapes on every route is a permitted superset and is
> recommended (chapter 55 R55.5.11 already takes that approach for
> the UTXO unit variants), provided the reference form for each
> family continues to be accepted.

### 22.9B.2 Operations a paired wallet can actually sign

**RP9 (informative record of the reference release, §22.9B.4).**
Signing reachability in the reference is **per operation, not per
subsystem**:

| Operation | EVM | Cosmos | UTXO |
|-----------|-----|--------|------|
| Activate; learn address and public key from the wallet | yes | yes | yes |
| Withdrawal (both the legacy and task withdrawal routes) | yes | yes | **no — refused as an unsupported key policy** |
| Ordinary transaction build then wallet signature then broadcast | yes | yes | yes |
| Legacy (v1) atomic-swap HTLC payment / spend / refund | transaction-send path is wired; not announced as supported | generic send path is wired; not announced as supported | **yes — the one family the reference announces as swap-capable** |
| Swap-protocol-v2 HTLC paths | no | no | no |
| Watcher-assisted swap (detached transactions a watcher broadcasts later) | no | no | no |
| Stand-alone message-signing RPC | no | no | no |
| Private-key export / display | refused | refused | refused |

**RP9.1.** Wallet message signing is reachable only as an
activation-time public-key recovery step (RP8.3). It is not wired
to the user-facing message-signing RPC for any family.

**RP10 — Watcher participation off for a key-less policy
(binding).** A coin whose signing policy holds no local secret
shall report itself **unsupported by the watcher subsystem** of
[Chapter 09](09-watcher-reward-infrastructure.md). The watcher
hand-off requires a detached signature produced with the same key
the swap itself uses, and an external wallet yields neither that
key nor a detached raw signature it did not broadcast. This gate is
what keeps a key-less coin out of the watcher preimage paths
instead of letting it reach a key accessor that cannot serve it.

**RP11 — How the swap protocol's key requirements are met without
a local key (UTXO).** The two obstacles
[Chapter 47](47-metamask-integration.md) R47.5.13 names do not both
bind the UTXO family:

1. *No per-swap HTLC key is needed.* For the UTXO family the HTLC
   key was never derived per swap: the HTLC public key is the
   activated address's compressed public key and the swap-unique
   data is ignored. Under a key-less policy the HTLC public key is
   therefore simply the public key the wallet reported at
   activation (RP8.3) — no derivation is attempted and none is
   required.
2. *The framework keeps broadcast control.* Every signature over an
   HTLC output is obtained by handing the wallet a PSBT carrying
   the single input to be signed, annotated with the previous
   (non-witness) transaction, the redeem script, and a sighash
   selector of ALL, with broadcasting explicitly **not** requested.
   The framework extracts the partial signature from the returned
   PSBT, assembles the final unlocking script itself — signature,
   then the branch-selecting script data, then the redeem script —
   and retains control of when the transaction is broadcast.
   Ordinary pay-to-public-key-hash and
   pay-to-witness-public-key-hash inputs use the same no-broadcast
   PSBT request, with the framework finalizing any input the wallet
   leaves unfinalized.
3. *The detached-signature requirement is declined, not satisfied.*
   Nothing produces a re-broadcastable detached signature for a
   third party; RP10 removes the need by excluding the coin from
   watcher participation.

**RP11.1.** The swap-protocol-v2 HTLC paths and the watcher-message
paths still read the per-swap key-pair accessor directly. For a
key-less policy that accessor has no implementation. A key-less coin
shall therefore be prevented from entering those paths at the
**earliest** lifecycle point with a structured unsupported-operation
error, consistent with Chapter 47 R47.5.13a — never by reaching an
unimplemented accessor.

**RP12 — No WalletConnect signing RPC.** Signing shall not be
exposed as its own RPC method. The five methods of §22.9A.2 are the
complete WalletConnect-specific RPC surface; every signature is a
side effect of an ordinary coin operation performed on a coin
activated under the policy of RP8. A caller therefore reaches
signing by: pair, settle, activate with the WalletConnect key
source, then invoke the ordinary withdrawal or trading operation.

### 22.9B.3 Build-gating caveat

**RP13.** External-wallet signing for the UTXO family must be
reachable in the **shipped artifact**, not only in some build
configuration. In the reference release the UTXO external-wallet
signing and activation code sits behind a Cargo feature that is
**off** in the daemon crate's default feature set and switched on
only by the binary/library crate that wraps it, because the
additional dependency it pulls in conflicts with an existing
dependency version when test binaries link. A conforming
implementation shall either keep the capability unconditional or
ensure every shipped target enables it; a build that compiles the
daemon crate directly with default features silently loses UTXO
external-wallet signing and activation and reports it as a
build-configuration failure at use time rather than at build time.

### 22.9B.4 Reference release designations (informative)

| Public release designation | WalletConnect change |
|----------------------------|----------------------|
| `v2.5.0-beta` (2025-07-04) | First appearance. WalletConnect v2 pairing, sessions, persistence, and signing integration for the EVM and Cosmos families; the EVM activation key-source field acquires its tagged-object form as an announced breaking change. |
| `v2.6.0-beta` (2025-11-28) | UTXO (`bip122`) **activation** only, explicitly announced as activation-only with signing to follow. Pairing responses begin returning the pairing topic. |
| `v3.0.0-beta` (2026-03-06) | UTXO transaction signing and **legacy (v1) atomic-swap** support. This is the last material change; the newest reference lineage available carries nothing later. |

No release designation announces WalletConnect swap support for the
EVM or Cosmos families, nor watcher, swap-protocol-v2, or UTXO
withdrawal support under this policy. The reference's own
end-to-end WalletConnect swap test is a manually driven,
non-automated test against a public test network that requires a
human to approve the signing prompts in a real wallet; there is no
automated coverage of the swap path.

> **Code-quality finding (informative).** Three gaps are visible in
> the reference's own treatment of this policy. Each is stated as a
> behavioral fact with a proposed remedy; none should be ported as
> a contract.
>
> 1. *UTXO withdrawal is refused while UTXO swapping is permitted.*
>    Both withdrawal routes reject the key-less policy as
>    unsupported, yet the same coin may enter a legacy swap, where
>    the framework signs ordinary inputs through exactly the
>    delegated PSBT path a withdrawal would need. The capability
>    exists and is simply not wired to withdrawal. Remedy: route
>    both withdrawal paths through the same delegated signing
>    operation the swap path uses, so a user who can swap a coin can
>    also move it — this is the one RP9 **no** that reloaded should
>    turn into a **yes**.
> 2. *Unimplemented key accessors are reachable without a guard.*
>    The swap-protocol-v2 HTLC paths and the watcher-message paths
>    call the per-swap key-pair accessor directly, and that accessor
>    has no implementation for a key-less policy. Nothing in order
>    placement or swap start prevents a key-less coin from reaching
>    them; the watcher case is closed only incidentally, by the
>    watcher-support gate of RP10, and the swap-protocol-v2 case only
>    by that protocol being an opt-in node setting that is off by
>    default. Remedy: RP11.1's early structured rejection.
> 3. *A correctness hazard is acknowledged but unaddressed.* The
>    EVM delegated-signing path records that it does not take the
>    account nonce serialization it would need once swaps run over
>    this policy. Any future EVM swap support under this policy must
>    close that first, or two concurrent delegated sends can race on
>    the same account nonce. Remedy: acquire the same nonce
>    serialization the local-key path uses before handing a
>    transaction to the wallet, independently of whether swaps are
>    enabled.

## 22.10 External References

- The WalletConnect v2 protocol specification (transport,
  pairing, session, JSON-RPC envelope, namespaces). The
  binding spec for the wire-level behaviour described above.
- CAIP-2 (chain identifiers) and CAIP-10 (account identifiers)
  for the namespace identifiers used in §22.8.
- The Ethereum JSON-RPC method names (`eth_signTransaction`,
  `eth_sendTransaction`, `personal_sign`, `eth_signTypedData_v4`)
  as defined by the Ethereum and EIP standards.
- The Cosmos signing schemes (Amino-JSON and SignDirect /
  Protobuf) as defined by the Cosmos SDK.
- The PSBT format (BIP-174) for the UTXO `signPsbt` flow.
- RFC 5869 (HKDF), RFC 7539 (ChaCha20-Poly1305 / Poly1305),
  RFC 7748 (x25519) for the cryptographic primitives.

## 22.11 Baseline Verifications

The following are verifiable from the baseline state defined in
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** WalletConnect v2
    subsystem. A directory listing of the baseline tree
    (`git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f`)
    returns no path containing any WalletConnect-related crate
    or module. The subsystem described in this chapter is
    therefore material introduced after the baseline in its
    entirety.

V2. The baseline tree contains no integration trait of the
    shape described in §22.3. A tree-wide `git grep` for the
    integration-trait name against the baseline returns no
    matches.

V3. The third-party Cargo dependencies that provide the relay
    client and Type 0 envelope codec are referenced from the
    workspace `Cargo.toml` by tag-pinned git URL. The codebase
    consumes their public Rust APIs only; no vendored copies of
    their sources are present.

## 22.12 Provenance Footer

- *Inputs:* the baseline workspace at the pinned baseline-revision
  commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of the
  subsystem at baseline verified via
  `git ls-tree -r c1d46c0c1592faa0860f704008b2b2381bc3840f`
  and tree-wide `git grep` for the integration-trait name
  against the baseline; chapter 31 (the central application-
  context substrate the `wallet_connect` sub-context slot is
  registered on per chapter 31 R7 / R8); the public WalletConnect v2
  specification; the CAIP-2 and CAIP-10 namespaces; the
  Ethereum JSON-RPC method definitions; the Cosmos SDK signing
  schemes (Amino and SignDirect); BIP-174 (PSBT); RFC 5869
  (HKDF), RFC 7539 (ChaCha20-Poly1305), RFC 7748 (x25519); the
  Reown multichain RPC reference (eip155 / cosmos / bip122 method
  payloads) and the Keplr public account (`Key`) wire shape; the
  relicensed historical record, consulted under R31 solely as the
  Interop / wire-format source for the §22.5.3 `open` on-disk
  session-record names, the §22.7.1/R11 `encoding_algo`
  selection/default behaviour, and, for §22.8.1, the
  externally-dictated request/response wire field spellings, the
  CAIP-2 reference formats, the `session-info` / namespace-record
  field spellings, and the dictated wallet-metadata signals; and,
  for §22.9B, the signing-reachability acceptance sets, the
  activation key-source JSON shapes, and the public release
  designations (see *Forbidden corpus* below).
- *Permitted-input classes used:* baseline source; external public
  specifications (WalletConnect v2; CAIP-2; CAIP-10; Ethereum
  JSON-RPC; EIP-191/EIP-1559; Cosmos SDK signing schemes / ADR-036;
  BIP-174; the Reown multichain RPC reference; the Keplr public
  account wire shape; RFC 5869, RFC 7539, RFC 7748); cross-chapter
  contracts (Chapter 31); Interop / wire-format reuse (R29/R33) for
  the `open` on-disk session-record names embedded in §22.5.3,
  functional behaviour confirmation for the §22.7.1/R11
  `encoding_algo` selector, and for the §22.8.1 request/response
  wire payloads, CAIP-2 reference formats, `session-info` /
  namespace-record field spellings, and dictated wallet-metadata
  signals, and for the §22.9B signing-reachability acceptance sets,
  activation key-source JSON shapes, and public release
  designations, with the relicensed historical record cited as the
  R31 source (see *Forbidden corpus* below).
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted for clean-room derivation of
  protected expression. There are four narrow exceptions, each
  consulted under R31 **solely** for clean functional or
  externally-required interop facts:
  1. the §22.5.3 `open` on-disk session-record format, whose only
     authoritative source is the relicensed historical record
     (`wc_session`/`topic`/`data`/`expiry` and the `data`-payload
     keys, including `session_key`/`sym_key`);
  2. the §22.7.1/R11 `encoding_algo` selector behaviour: the
     valid semantic values, the hex default when the selector is
     missing, and the explicit required-wallet base64 branch;
  3. the §22.8.1 request/response wire payloads, CAIP-2 reference
     formats, `session-info` / namespace-record JSON field
     spellings, and the dictated wallet-metadata signal values —
     all of which are fixed by public WC2 / CAIP / Reown / EIP /
     Cosmos / BIP-174 / Keplr standards, with the corpus consulted
     only to confirm which standard-dictated method set and field
     spellings reloaded must emit.
  4. §22.9B, consulted under R31 for the reachability facts a
     clean-side implementer cannot derive from public
     specifications: which activation JSON shapes the reference
     accepts for a WalletConnect key source and on which activation
     route (RP8, RP8.2), what the wallet is asked for at activation
     (RP8.3), which user-facing operations are reachable per chain
     family (RP9), the watcher exclusion for key-less policies
     (RP10), the delegated-PSBT signing model and the absence of
     per-swap HTLC key derivation for the UTXO family (RP11), the
     absence of any signing RPC (RP12), the shipped-artifact build
     gating (RP13), and the public release designations of §22.9B.4.
     Behaviour and acceptance sets only; the §22.9B.4 findings are
     restated as behavioral facts.
  No discretionary expression — no bodies, private identifiers,
  comments, or diagnostics — was taken from the corpus; the
  surrounding prose is authored fresh per the R31 sanitization
  discipline.
- *Dirty Gate clearance (chapter 01 T1):* the fourth exception
  above — §22.9B in full (RP8 through RP13 and §22.9B.4), the
  paragraph it added to §22.0, deferrals D10 and D11, and the
  fourth scoped item of this footer — was authored by the KDF Spec
  Reader and **passed the KDF Dirty Gate** on 2026-10-17: PASS,
  discretionary-expression measure 0.02, finding list empty, and
  accordingly no remedy class for any line or for the candidate as
  a whole. The clearance is therefore **complete and not pending**,
  so R40 no longer withholds this material; §22.9B carries a short
  pointer to this item so that a reader at the material itself does
  not have to come here to learn as much. The pass compared the
  whole chapter, new material and previously cleared text alike;
  the material listed above is what it gated for the first time.
  The three elements of the R39 scope attestation, carried forward
  here in corpus-free terms as chapter 01 T1 requires:
  - *Extent.* The subject matter this chapter's own scope names:
    the WalletConnect v2 dApp / relay-client subsystem and its
    per-chain signing integration for the three chain families; the
    coin-activation key-source acceptance surface for those same
    three families; the withdrawal, ordinary-transaction,
    legacy-swap and swap-protocol-v2 signing paths, together with
    the watcher-support gate; the per-swap swap-key accessor
    surface; the build-configuration / feature surface that governs
    whether the UTXO external-wallet capability is present in a
    shipped artifact; and the published release-notes record.
    Reference versions: the publicly published release designations
    `v2.5.0-beta`, `v2.6.0-beta` and `v3.0.0-beta`, the last being
    the newest publicly designated release of that lineage
    available to the gate. That is as precise as this record goes —
    a more exact reference cannot be given within the limits
    binding a scope attestation (§1.7) — and the gap is left
    visible rather than papered over. Quantity examined:
    approximately **twenty files** read in full or in the relevant
    part for the close reading, and **864 files plus the published
    release-notes document** enrolled in the mechanical sweep.
  - *Method.* A stated combination of three. (1) **Reading** —
    line-by-line classification of every added line of the
    candidate against the corresponding compared material, with the
    per-token forced-versus-free test applied to each token that
    matched. (2) **Mechanical comparison** — a word-normalised
    n-gram overlap sweep at **N = 5, 6, 7 and 8** between the
    candidate text and the compared set described above, run both
    over the new sections alone and over the whole chapter line by
    line. (3) **Targeted absence check** — a check of the candidate
    for the private, non-wire identifiers of the compared
    subsystem: **25 candidate names checked, all absent** from the
    chapter.
  - *Measure.* **0.02** on the discretionary-expression basis — the
    same figure the leak report carries, not a second one. The
    judgement standard it is read against: the measured set admits
    discretionary expression only and excludes dictated-interop and
    pinned-interface content, so genuine discretionary expression
    fails in any amount while dictated and public matches score
    nothing however dense, and token-match density is therefore not
    read as evidence. The residual figure is attributable solely to
    publicly published release designations and to generic
    subject-matter wording that cannot be varied: at **N = 8 the
    whole chapter has no overlap at all** with the whole compared
    set, and the entire mechanical residue at **N = 6 and N = 7**
    consists of publicly published release designations and their
    release dates, which §1.7 expressly admits as public-release
    facts.

  This item records the fourth exception only. The first three
  exceptions were committed in June 2026, before §1.4A was adopted
  (2026-10-11), so T1 does not reach them and R43 makes them no
  less sound; recording the route for such pre-adoption material is
  chapter 01 D3.
