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

**Operator availability switches.** Separately from what is
*reachable*, §22.9C specifies the operator-facing switches that
decide whether a reachable WalletConnect operation is *offered*: a
global enable/disable switch for the whole subsystem, enforced at
activation and at session establishment and failing closed, and
per-family switches for the withdrawal and swap operations that can
actually occur. A delegated operation that has no enumerated
operation class is **refused** rather than passed on the global
switch, so a fund-moving route cannot become available to a paired
wallet by falling through a catch-all (§22.9C.6). They are a security control rather than a
GLEEC-compatibility control (§22.9C.2), they default to today's
behaviour, and none of them is implemented yet (D12).

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

D12. **[REQUIRED — §22.9C, not implemented]** The operator
    availability switches of §22.9C: the global WalletConnect
    enable/disable switch (RS1–RS4.3), the four per-family
    per-operation switches RS6 specifies, the single signer-selection
    chokepoint of RS7 with the residual-class refusal and call-site
    enumeration of RS7.3, the nested configuration block of
    RS8–RS10, the single resolution point of RS11, and the
    per-switch tests ACS4 requires. Nothing of this exists
    in the tree today. The implementation plan is
    [`docs/plans/walletconnect-switches-and-presigned-refund.md`](../plans/walletconnect-switches-and-presigned-refund.md),
    phase 1. Each per-family switch is sequenced behind the change
    that makes its operation reachable (RS5.1), so this item
    partially follows D10 and D11.

D13. Two controls §22.9C deliberately leaves unspecified rather than
    specify dormant: a **per-family activation** switch (the global
    switch's activation refusal, RS2(1), is the only activation-level
    control specified), and a **runtime or RPC-level** disable (RS4.2
    makes the switch restart-scoped; an RPC that can disable a
    security control needs its own authorization story first). Both
    fall under RS5.2 if they are added.

D14. **[REQUIRED — §22.9C, not implemented]** RS6.1's gate: a
    Tendermint coin activated under a WalletConnect key source must
    be refused entry to the **legacy (v1) HTLC paths** at the
    earliest lifecycle point. Neither RP11.1 nor
    [Chapter 47](47-metamask-integration.md) R47.5.13a names that
    path, and the per-swap key accessor returns no error there — it
    returns "no key of my own", on which the swap machine substitutes
    the node's persistent public key. This is a gate rather than a
    switch and is why §22.9C.5 specifies no `cosmos` swap switch.
    It covers a Tendermint **token** as well: a token forwards the
    per-swap key accessor to its platform coin, so a token whose
    platform coin is activated under a WalletConnect key source
    inherits the same hazard and must be refused on the same terms.

D15. **[REQUIRED — §22.9C, not implemented]** RS6.2's gate: a UTXO
    coin activated under a WalletConnect key source must be refused
    at the swap-protocol-v2 **public-key** accessor, not only at the
    per-swap key accessor. The key accessor would refuse a policy
    with no local secret; the public-key accessor's HD arm answers
    for a non-key-pair policy, so an implementation that adds a
    WalletConnect arm by mirroring it announces a public key the
    paired wallet cannot sign with. This is the UTXO counterpart of
    D14 and, like it, a gate rather than a switch. It must land no
    later than the change that introduces the policy arm.

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

## 22.9C Operator Availability Switches (driving-spec)

**STATUS.** *Required — not implemented.* None of this section
exists in the tree: there is no `walletconnect` configuration block,
no signer-selection chokepoint, and no enforcement point. The
section is the driving spec for that work and is tracked as D12. It
is **clean-room-original design** — it restates no external
behaviour and derives from no forbidden-class input (§22.12).

### 22.9C.1 Why the subsystem needs an off switch

WalletConnect is the only subsystem in this tree holding a
**persistent, relay-mediated, externally-pairable session with key
material at rest**. Three properties combine here that combine
nowhere else:

- the session outlives the process (§22.5), so the exposure is not
  bounded by how long a node happens to run;
- the counterparty is reached through a third-party relay and the
  pairing is initiated from outside the node's trust boundary
  (§22.4); and
- the session symmetric key is written to disk **unencrypted** under
  the default `open` value of `wc_session_persistence` (§22.5.1) —
  the deliberate, documented Security-versus-compatibility departure
  §22.5.1 records, whose fix is the reserved encrypted at-rest format
  **D8** tracks and which is **unimplemented** at the time of writing.

R5 / D3 are a *different* obligation answering a *different* threat
and are not the fix for this one: zeroize-on-drop binds the
**in-memory** session-key type, which addresses core dumps, swap
files and freed-heap reads. Zeroizing an in-memory key does nothing
about a key sitting at rest on disk. Both obligations are open; this
section conflates neither with the other.

The bounded exposure §22.5.1 records — theft of a persisted session
key permits soliciting a signing prompt from the paired wallet, not
direct theft of funds — is why the `open` default is tolerable. It
is not a reason the subsystem needs no off switch. An operator who
learns of a relay compromise, a weakness in the pairing flow, or a
vulnerability in a wallet their users pair with has today no way to
remove the exposure short of waiting for a release. This section is
that way.

**Which part of the exposure the switch removes, and which it does
not.** Being precise here matters more than the switch sounding
useful, because an operator acting in an incident will act on this
paragraph.

- *Removed by the switch.* Everything prospective: no relay
  connection, no new pairing, no settlement of a proposed session,
  no persisted session restored into memory, and no signing request
  reaching any wallet. After a restart with the switch off, nothing
  this subsystem can do happens.
- *Not removed by the switch.* Everything already at rest. The
  plaintext session symmetric keys written under the default `open`
  value stay on disk exactly as they were — RS4.1 requires that,
  deliberately, so that re-enabling does not force every user to
  re-pair. A stolen disk image or backup taken after the switch was
  flipped is worth precisely what one taken before it was worth.
  The control that changes that is D8's encrypted at-rest record,
  not this switch.
- *Removed by the operator, through the affordance RS4.3
  specifies.* Because the at-rest material is usually what an
  operator reaching for an off switch after a key-exposure incident
  wants gone, the disabled state must not take away their ability to
  delete it. RS4.3 is that requirement, and it is why this section
  does not simply refuse all five RPCs when disabled.

> **Containment versus the actual holes (engineering judgement).** A
> switch is *containment*: it protects whoever knows to flip it, from
> the moment they know. Closing a key-handling gap protects
> *everyone*, including every operator who never reads this chapter.
> There are **two** such gaps and they are not interchangeable:
> **D8** — the encrypted at-rest record — answers the exposure this
> section opens with, a session key readable from a stolen disk image
> or a backup; **R5 / D3** — zeroize-on-drop for the in-memory
> session-key type — answers in-memory disclosure through core dumps,
> swap and freed-heap reads. Both are wanted, neither substitutes for
> the other, and **both come before this section's switches**. Which
> of the two comes first is a maintainer choice about which threat is
> being answered, and this section does not make it: it only records
> that a reader sequencing against the *at-rest* exposure described
> above takes **D8**, not R5 / D3. Nothing in this section addresses
> either gap, and this section must not be read as having reduced
> the priority of either.

### 22.9C.2 Not compatibility switches, and why the default is *available*

**These are not `RELOADED_VS_GLEEC` switches.** The compatibility
convention of `COMPAT_SWITCHES.md` exists for one purpose: when
reloaded behaviour *diverges from GLEEC*, an operator must be able
to recover the GLEEC-compatible behaviour, and the divergence must
be recorded in `GLEEC_COMPATIBILITY.md`. The switches of this
section do the opposite job. They let an operator **reduce their own
attack surface**; they describe no divergence, there is no
GLEEC-compatible value to recover, and their lifecycle is the
lifecycle of the threat rather than of a compatibility window. They
are therefore deliberately a **separate mechanism**: they shall not
be added to `COMPAT_SWITCHES.md`, shall not be given a row in
`GLEEC_COMPATIBILITY.md`, and shall not be framed as
`RELOADED_VS_GLEEC` entries. Where operator-facing security controls
accumulate enough to need a central home of their own, that home is
a document distinct from the compatibility chapters; the decision of
whether to create one belongs to the implementation plan
(`docs/plans/walletconnect-switches-and-presigned-refund.md`) and is
deliberately not taken here.

**The default-side rule does not reach these switches, and that is
the point.** The project's standing policy on which side of a
compatibility divergence the default falls is recorded in
[`docs/COMPAT_SWITCHES.md`](../COMPAT_SWITCHES.md), *"The
default-side rule"* (anchor `#the-default-side-rule`); it is not
restated here. What matters for this section is the carve-out that
accompanies it: the same document's *"What this convention is not"*
states that the rule is **not** a rule about operator-facing
security controls, that a security control's default is a security
decision for the feature that owns it rather than a parity question,
and that the rule does not reach it. The switches of this section
are exactly that kind of control. They touch **no wire format** (no
WC2 envelope, no CAIP identifier, no request payload of §22.8.1, no
on-disk record of §22.5.3 changes) and affect **no other node** (a
disabled subsystem makes a node offer fewer of its own capabilities;
it changes nothing another node observes or relies on), so enabling
or disabling them diverges from nothing — which is the condition
under which `COMPAT_SWITCHES.md` says such a control enters that
convention at all. It therefore does not enter it, and the
default-side rule has nothing to decide.

**This section owns the default, and it is *available*.** The reason
is not parity and not a rule appealed to from elsewhere: it is that
the status quo must not change under an operator who configures
nothing. Any other default would turn an upgrade into a silent
withdrawal of a capability their users already have, and RS1 and
RS9's first case are where that is bound. The restrictive reading is
available to an operator who asks for it, in one key. This
reasoning is recorded here so that a later reader does not
reconstruct it, and so that the absence of a
`GLEEC_COMPATIBILITY.md` row is not later mistaken for an
oversight.

### 22.9C.3 The global switch

**RS1 — A global availability switch exists.** A single
node-configuration switch shall govern whether the WalletConnect
subsystem is available at all. Its default value is **available**,
so an operator who configures nothing sees exactly the behaviour
they see today.

**RS2 — Two enforcement points, both mandatory.** The global switch
shall be enforced at exactly these two points, and a conforming
implementation shall enforce it at both:

1. *Activation.* Activation of any coin whose key-source policy
   names a WalletConnect session (RP8, any of the three request
   shapes) shall be refused with a structured error, never a panic,
   when the switch is off. This is the point that keeps a disabled
   subsystem from acquiring new dependents.
2. *Session establishment and restore.* When the switch is off, the
   subsystem shall not create a new pairing, shall not settle a
   proposed session, shall not restore a persisted session into
   memory, and shall not establish the relay connection. The public
   RPC handlers of §22.9A.2 shall refuse with a structured error
   rather than returning an empty result, so that "disabled" is
   distinguishable from "no sessions" — with the two exceptions
   RS4.3 specifies, which read and delete local records only and
   establish nothing.

**RS3 — The switch fails closed.** Where the effective value of the
switch cannot be determined — an unreadable or unparsable
configuration block, a value of the wrong type, an enforcement point
that cannot reach the resolved value — the subsystem shall be
treated as **disabled**. No code path may proceed past either
enforcement point on an undetermined value.

This is not in tension with the *available* default of RS1, and the
distinction is load-bearing: **absence of configuration is a
determinate answer** (the operator expressed no policy, so the
status-quo default applies), whereas **a configuration error is not
an answer at all** (the operator expressed a policy that could not
be read, and the safe reading of an unreadable security policy is
the restrictive one). An implementation that conflates the two
cases fails this requirement whichever way it conflates them.

**RS4 — The global switch short-circuits every per-family switch.**
When the global switch is off, the per-family map of §22.9C.5 shall
not be consulted and no per-family value can re-enable anything. A
per-family switch set to *available* under a disabled subsystem is
not an error and is not a conflict; it is simply unreachable.

**RS4.1 — Disabling is not destructive, and does not weaken R8.**
Disabling the subsystem shall not delete, rewrite, or expire any
persisted session record, so that re-enabling restores the prior
session set rather than requiring every user to re-pair. There is
no conflict with the unconditional-load rule R8 / §22.5.2: R8 binds
what the loader does *about `wc_session_persistence`* when the
loader runs — it must read every record irrespective of that
setting and auto-detect its format — and RS2(2) decides whether the
subsystem connects to the relay and therefore whether the loader
runs at all. A disabled subsystem reads nothing because it starts
nothing; it does not read selectively. R8 is unweakened and
unamended.

This requirement binds what **disabling** does, not what the
operator may do while disabled. An operator who deletes a session
record through the affordance RS4.3 specifies is not the switch
acting destructively; they are exercising a capability they have
while the subsystem runs and must not lose by turning it off.

**RS4.3 — Disabling shall not take away the purge.** When the global
switch is off, two of the five RPCs of §22.9A.2 shall remain
available in a **local-records-only** form, and the rest shall
refuse per RS2(2):

- the session **enumeration** RPC shall answer from the persistence
  backend alone, listing the persisted records so that the operator
  can see what is still on disk and name it; and
- the session **deletion** RPC shall delete the named persisted
  record from the persistence backend alone and return success.

Both forms shall establish no relay connection, create no pairing,
restore nothing into memory, and send no `wc_sessionDelete`
courtesy message to the wallet — a disabled subsystem has no live
session to send one on, and reaching the relay to be polite would
undo the thing the operator just asked for. The deletion RPC's
disabled form therefore needs a storage-only path that does not go
through the handle construction RS2(2) refuses; specifying and
building that path is part of the work D12 tracks, and it is the one
place where this section requires a code route that exists in no
form today.

The reasoning, stated because the asymmetry will otherwise read as
an oversight: refusing deletion while disabled serves no threat
model. Deleting a session record is a *reduction* of exposure, not a
use of the subsystem; it emits no relay traffic, cannot acquire a
dependent, and cannot solicit a signature. Refusing it would mean
that an operator who flips the switch after a key-exposure incident
is left with the plaintext keys they were worried about unreachable
to them and unchanged on disk, their only recourse being to
hand-edit SQLite or IndexedDB. Enumeration is exempted with it
because an operator who cannot list the records cannot name one to
delete, and the enumeration answers from those same local records.

**RS4.2 — Restart scope.** The switch is resolved from the node
configuration **once, at the resolution point RS11 specifies**, and
is **not** hot-reloadable: shutting the subsystem off requires a
node restart, not a release. Read RS11 before implementing this:
that resolution point does not exist today and is new work, because
the whole `walletconnect` configuration object is resolved lazily at
the first WalletConnect RPC. Without RS11 this requirement's
"at startup" is not satisfiable and RS10's and ACS6's
"stops startup" are not either. A
runtime-reconfiguration or RPC-level kill switch would need its own
authorization story — an RPC that can disable a security control can
also be the thing an attacker calls — and is deliberately not
specified here (D13).

### 22.9C.4 No dormant security switches

**RS5 — No dormant security switches (binding).** A switch that does
not gate anything tells an operator they are protected when they are
not, which is worse than having no switch at all: the operator stops
looking. Therefore:

- every switch specified in this section **MUST** gate at an
  enforcement point in the shipped artifact; and
- every switch specified in this section **MUST** have a test that
  proves it gates — a test that performs the operation with the
  switch off and asserts the structured refusal, and performs it
  with the switch on and asserts it proceeds.

A switch for which the second bullet cannot be written is evidence
that the first bullet is false, and the switch shall not ship.

*Acceptance criterion.* ACS4 of §22.9C.8 is the discharge of this
requirement, and it is per switch, not per section: the per-switch
test matrix is complete when every switch §22.9C.5 specifies has
both halves of its own test. A conforming implementation that adds
a switch without its test does not satisfy this chapter.

**RS5.1 — Sequencing consequence.** Because signing is not reachable
by a user at all today (D10, §22.9B), a per-family switch shipped
before the operation it gates is reachable would be dormant by
construction and would breach RS5. Each per-family switch shall
therefore ship **in or after** the change that makes its operation
reachable, and never before it. The global switch of §22.9C.3 is
not subject to this constraint: its session-establishment
enforcement point (RS2(2)) gates operations that are reachable
today — pairing, settlement, restore, and the five RPCs of §22.9A.2
— so it is non-dormant from the moment it ships, and its
activation enforcement point (RS2(1)) becomes live with D10 without
ever having been dormant in the sense RS5 forbids.

**RS5.2 — The forward half of the rule.** If a family/operation
combination that §22.9C.5 refuses a switch for later becomes
reachable, the change that makes it reachable **shall** add that
switch, its enforcement at the chokepoint, and its RS5 test in the
**same** change. Making an operation reachable without that is a
silent security decision taken by omission, which is exactly what
§22.9C.5's refusals are chosen to avoid.

### 22.9C.5 Which family/operation switches exist, and which do not

**RS6 — Switches exist only where the operation is reachable.** A
per-family, per-operation switch shall be specified for a
combination if and only if that operation can actually occur for
that family under a WalletConnect key source. Reachability is
determined by RP9's per-operation record together with the binding
rules RP10, RP11, RP11.1, D11 and
[Chapter 47](47-metamask-integration.md) R47.5.13 / R47.5.13a.
(RP9 is an **informative** record of the reference release, §22.9B;
it is used here as evidence of what is reachable, and the binding
rules just named are what govern.) The determination is:

| Family (CAIP-2) | Operation | Switch | Basis |
|---|---|---|---|
| `bip122` (UTXO) | withdrawal | **yes** | Reachable in reloaded by requirement. RP9 records the reference refusing it, but **D11 binds reloaded to make it reachable** through the same delegated PSBT signing operation the legacy swap path already uses, and §22.9B.4 finding 1 calls it the one RP9 `no` reloaded should turn into a `yes`. The switch ships with that wiring (RS5.1), not before it. |
| `bip122` (UTXO) | legacy (v1) swap HTLC | **yes** | Reachable. RP9 marks UTXO the one family the reference announces as swap-capable, and RP11 gives the structural reason: the UTXO HTLC public key *is* the activated address's public key, no per-swap key is derived, and every HTLC signature is obtained by a no-broadcast PSBT request with the framework retaining broadcast control. Chapter 47 R47.5.13a carves exactly this case out of its swap ban. |
| `bip122` (UTXO) | swap-protocol-v2 HTLC | **no** | Unreachable, and gated **by requirement** — RP11.1 — rather than by anything the tree enforces today. A switch would gate nothing, but the basis is narrower than it first looks, and RS6.2 closes the difference. The family has **two independent** accessors on two different code paths, and neither calls the other: the per-swap *key* accessor (`get_htlc_key_pair_v2` in `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`) builds a key pair from a local secret and errors for a policy that has none, while the per-swap *public-key* accessor (`try_derive_htlc_pubkey_v2` in `mm2src/coins/utxo/utxo_standard_swap_v2.rs`) **answers** for the one non-key-pair policy in the tree, returning the enabled HD address's public key for `Trezor`. The second is the same hazard shape RS6.1 identifies for Tendermint, one path over: an accessor that answers instead of refusing, with the refusal deferred to a later signing step. |
| `eip155` (EVM) | withdrawal | **yes** | Reachable. RP9 records `yes` on both the legacy and the task withdrawal route. |
| `eip155` (EVM) | swap (either protocol) | **no** | Structurally unreachable, and bindingly gated for a different reason than operator policy. Chapter 47 R47.5.13 records that the EVM-family HTLC paths require a per-swap HTLC key pair derived from a **local** secret, which no key-less policy can produce, and R47.5.13a extends its early structured rejection to *every* non-keypair signing policy for the EVM-family HTLC paths — carving out only a family that needs no per-swap key and can delegate every HTLC signature, which RP11 establishes is the UTXO case and not this one. RP9 agrees: swap-protocol-v2 `no`, and for legacy v1 only "transaction-send path is wired; not announced as supported". §22.9B.4 finding 3 adds that an unresolved account-nonce serialization hazard would have to be closed before EVM swaps could run over this policy at all. A switch here would be a permanent no-op reporting protection that R47.5.13a provides for an unrelated reason, and it would silently decide the security question if the combination ever became reachable — RS5.2 is how that question gets decided instead. |
| `cosmos` (Tendermint) | withdrawal | **yes** | Reachable. RP9 records `yes`. |
| `cosmos` (Tendermint) | swap (either protocol) | **no** | Unreachable. The legacy Tendermint HTLC path takes its signing key from the activated local secret — `get_htlc_key_pair` in `mm2src/coins/tendermint/tendermint_swap_ops.rs`, which resolves `activated_key_or_err` on the activation policy — and a key-less policy cannot satisfy it; swap-protocol-v2 is refused by RP11.1 as it is for every family. RP9 records only "generic send path is wired; not announced as supported" for the v1 row, which is not reachability. See RS6.1: this is the one refusal the existing record does not already gate explicitly, and it is closed with a gate rather than with a switch. |
| all three | activation; learning the address and public key from the wallet | global only | Reachable for all three families (RP9 row 1) and governed by the global switch's activation enforcement point, RS2(1). A **per-family** activation switch would also be reachable and is deliberately left unspecified (D13) rather than specified and left dormant; RS5.2 governs if it is added. |
| all three | watcher participation | **no** | RP10 **requires** a coin whose signing policy holds no local secret to report itself unsupported by the watcher subsystem, unconditionally and for reasons unrelated to operator policy. A switch would gate nothing. (Required, not yet enforced: D10 lists RP10's watcher gate among the port's remaining work.) |
| all three | stand-alone message-signing RPC | **no** | RP9 / RP9.1: wallet message signing is reachable only as the activation-time public-key-recovery step of RP8.3; it is wired to no user-facing message-signing RPC for any family. Nothing to gate. |
| all three | private-key export / display | **no** | RP9 records it refused for all three families (the framework cannot export a key it does not hold; Chapter 47 R47.5.14 is the same rule for the MetaMask policy). Nothing to gate, and nothing an operator could usefully tighten. |
| all three | any other delegated operation | **none, and the operation is refused** | RP9's "ordinary transaction build → wallet signature → broadcast" row is `yes` for all three families but does **not** enumerate the family-specific operation classes it covers (Tendermint staking and IBC transfer, EVM contract calls, and so on). Several are live RPC routes in this tree already — `experimental::staking::delegate`, `undelegate` and `claim_rewards` reach `TendermintCoin::delegate` through `add_delegation` — and every one of them **moves funds**. They are therefore *not* governed by the global switch alone and *not* governed by a per-family `withdraw` switch either: they reach the chokepoint in the residual class of RS7.1 and RS7.3 **refuses** them. Making one of them available to a WalletConnect wallet means enumerating its operation class and adding its switch under RS5.2, in the change that makes it reachable. |

Four switches are therefore specified: `bip122` withdrawal,
`bip122` legacy swap, `eip155` withdrawal, `cosmos` withdrawal.

**Reading the *Basis* column.** The column distinguishes two things
a reader must not conflate, because §22.9C.5 is where a reader comes
to learn what already protects them. "Required" means a rule of this
document set binds it (RP10, RP11.1, R47.5.13a); "enforced" would
mean the tree refuses it today. For every `no` row above the basis
is **required**, and for RP10's watcher gate and RP11.1's early
rejection it is *only* required — D10 lists both among the port's
remaining work. The reachability conclusions hold either way (an
operation that is unreachable needs no switch), but no row of this
table should be read as evidence that a path is closed in the
shipped artifact.

**RS6.2 — Gate the UTXO swap-protocol-v2 public-key accessor too
(gap closure).** RP11.1 requires a key-less coin to be refused entry
to the swap-protocol-v2 HTLC paths at the earliest lifecycle point,
and the UTXO per-swap *key* accessor would refuse a policy with no
local secret. The per-swap *public-key* accessor would not: its
HD-wallet arm answers for a non-key-pair policy by returning the
enabled address's public key, and RP11 establishes that for this
family the HTLC public key *is* the activated address's compressed
public key with no per-swap derivation — which is exactly the shape
a WalletConnect UTXO policy would be written against. An
implementation that adds a WalletConnect arm there by mirroring the
existing non-key-pair arm reproduces RS6.1's failure mode: a
**silent wrong answer** (a public key announced to the counterparty)
rather than a refusal, with the refusal deferred to a signing step
that then cannot be served. A UTXO coin activated under a
WalletConnect key source shall therefore be refused at the
swap-protocol-v2 public-key accessor as well as at the key accessor,
at the **earliest** lifecycle point and on the same terms RP11.1
states; the work is tracked as D15. Like RS6.1 this is a gate, not a
switch: unconditional and not operator-configurable, which is part
of why this combination gets no `bip122` swap-v2 switch.

**RS6.1 — Gate the Tendermint legacy HTLC paths too (gap closure).**
The early structured rejection RP11.1 requires covers the
swap-protocol-v2 HTLC paths and the watcher-message paths, and
Chapter 47 R47.5.13a names the EVM-family HTLC paths, the
swap-protocol-v2 HTLC paths and the watcher-message paths. Neither
names the **Tendermint legacy (v1) HTLC paths**, and RP11's
carve-out is explicitly about the UTXO family only. That leaves one
path for which the record states no gate while the implementation
cannot serve it: `get_htlc_key_pair` returns `None` for a policy
with no activated key, and a `None` there does not fail — it means
"this coin has no key of its own", on which the swap machine
substitutes the node's own persistent public key
(`derive_htlc_pubkeys` in `mm2src/mm2_main/src/lp_swap.rs` maps the
absent key onto the node key before calling `derive_htlc_pubkey`,
whose 33-byte width is bound by
[Chapter 51](51-legacy-v1-swap-state-machine.md) R63). For a
key-less Tendermint coin that fallback announces a key the paired
wallet cannot sign with, which is a silent wrong answer rather than
a refusal. A Tendermint coin activated under a WalletConnect key
source shall therefore be refused entry to the legacy HTLC paths at
the **earliest** lifecycle point with a structured
unsupported-operation error, on the same terms and at the same
lifecycle point as RP11.1 and R47.5.13a require for the paths they
name; the work is tracked as D14. This is a gate, not a switch: it
is unconditional and not operator-configurable, which is precisely
why `cosmos` gets no swap switch.

### 22.9C.6 One enforcement chokepoint

**RS7 — Signing goes through a single chokepoint.** Enforcement of
the switches shall happen at exactly two places: the activation-time
refusal of RS2(1), and **one** chokepoint through which every
WalletConnect signing request for a coin operation passes. Every
delegated-signing route — withdrawal, legacy swap HTLC, ordinary
send, and any route added later — shall reach a `WcCoinSigner`
signing method (§22.3) only through that chokepoint. Scattered
per-call-site checks are **forbidden**, including as a transitional
state.

The chokepoint **performs** the signing call; it does not hand back
a signer for the caller to use. A chokepoint that returns the signer
has, by returning it, permitted the signing method to be invoked
outside itself, which is precisely the property RS7.2 exists to deny
— so "select a signer and give it to the caller" is not a
conforming shape, however natural it reads. The chokepoint takes the
coin, the session, the operation class of RS7.1 and the
chain-specific unsigned-transaction description, and returns either
the signature / signed transaction / transaction hash, or a
structured refusal.

The reason is not tidiness. Scattered checks produce *partial*
enforcement, and partial enforcement is the worst outcome available
here — worse than uniform absence, because it is
**indistinguishable from protection**. An operator who sets a
switch and observes one route refuse has no way to learn that a
second route does not consult it; absence of the feature at least
tells the truth. A single chokepoint makes the enforcement property
auditable by reading one function instead of by enumerating call
sites, and makes a newly added call site inherit enforcement rather
than need to remember it.

**RS7.1 — The chokepoint knows the operation.** The per-family,
per-operation decision cannot be made from the coin and the session
alone, so the chokepoint shall take the **operation class** as an
explicit argument supplied by the caller, never inferred from
context. Its variants shall be exactly the switched classes of
§22.9C.5, plus the activation-time wallet interaction, plus **one
residual variant** whose meaning is "this call site has not declared
an enumerated class". Adding a new operation class is the trigger
for RS5.2.

The residual variant is **not** a catch-all that passes on the
global switch. It is the subject of RS7.3, and the distinction is
the whole of this requirement's security value: a class argument
that a careless or hurried call site can set to "other" and thereby
reach the wallet would move the enforcement hazard rather than
remove it — from a missing per-call-site check, which RS7 forbids,
to a mislabelled call site, which nothing in this section would
catch.

**RS7.2 — The chokepoint is the only route, and the restricted
surface is the outbound signing funnel.** The property RS7 asserts
shall be enforced structurally rather than by convention. Two things
have to be right about *how*, and both are places where the obvious
answer fails.

*Rust visibility is not the instrument, and must not be named as
one.* `WcCoinSigner` is a public trait of the WalletConnect crate
and is implemented in a different crate, one implementation per
family. A trait method's visibility **is** the trait's visibility:
there is no way to keep a cross-crate public trait implementable
elsewhere while making its methods callable only from one module,
and the sealed-supertrait pattern restricts who may *implement*, not
who may *call*. The instrument shall instead be a **capability
witness**: a token type whose only constructor is private to the
chokepoint's module, which is neither `Clone` nor `Copy` nor
`Default` nor constructible by deserialization, and which the
restricted methods take **by value** as a parameter. Code outside
the chokepoint module can name the methods but cannot produce the
argument, so the restriction holds across crates and is enforced by
the compiler rather than by a reviewer.

*The trait is not the whole surface.* A check that constrains only
`WcCoinSigner` would certify a property that does not protect the
signing channel, because the subsystem's own outbound funnel is
reachable without the trait: the handle's public session-request
method takes an arbitrary JSON payload that becomes the
`wc_sessionRequest` params, and the public session index hands out a
session's symmetric key, so any holder of the handle can emit
`signPsbt`, `eth_signTransaction` or `cosmos_signDirect` with no
`WcCoinSigner` in sight. **Every public entry point that can emit a
`wc_sessionRequest` or hand a session symmetric key to a caller
shall therefore carry the same witness requirement** as the trait's
signing methods — concretely, in this tree,
`WalletConnectCtx::send_session_request` and the public session-key
accessor reached through `WalletConnectCtx::sessions()` and
`SessionManager::transport_for`. The subsystem's own non-signing
lifecycle traffic — the session-delete courtesy publish, the ping,
the expiry extension, the inbound decrypt loop — is unaffected and
shall use crate-private accessors rather than the witness-gated
public ones; it emits no `wc_sessionRequest` and is not a signing
route.

Where a particular family's module layout makes the witness
impractical, an equivalent mechanical check that fails the build or
a test is acceptable. A code-review convention is not.

**RS7.3 — The residual operation class is a refusal, and the call
sites are enumerated.** Two obligations, and neither substitutes for
the other.

1. *The residual class refuses.* The chokepoint shall refuse any
   call whose operation class is the residual variant of RS7.1,
   with a structured unsupported-operation error, **irrespective of
   the global switch and of every per-family value**. There is no
   configuration under which the residual class reaches a wallet.
   The reason is RS5's rule read the other way round: a per-family
   `withdraw: false` that leaves a fund-moving route open through an
   unenumerated class is a switch reporting protection it does not
   provide, which is the condition RS5 forbids, reached through the
   catch-all instead of through a dormant switch and harder to see
   because the per-switch test matrix is green. Refusing the
   residual class is also the conservative direction: nothing
   regresses, because no delegated signing route is reachable by a
   user today at all (D10).
2. *The call sites are enumerated and the enumeration is
   mechanical.* Every call site that reaches the chokepoint shall
   declare its operation class, and the set of call sites together
   with their declared classes shall be checked mechanically — a
   build-time or test-time check, not a review convention — so that
   a newly added call site either declares an enumerated class or
   fails the check. A call site cannot satisfy the check by
   declaring the residual class, because obligation 1 makes that a
   refusal at run time: the two obligations close the gap from both
   ends, one at the chokepoint and one at the call site.

The consequence is deliberate and is the cost of the rule: making a
new delegated operation available over WalletConnect is never a
matter of routing it through the chokepoint and relying on the
global switch. It requires enumerating the operation class, deciding
whether it needs its own switch, and landing both with the
reachability — which is RS5.2, now with teeth rather than as an
instruction a later author must remember.

### 22.9C.7 Configuration shape

**RS8 — One nested block, not a flat list of booleans.** The
switches shall be expressed as nested keys of the **existing**
`walletconnect` object in the node configuration (`MM2.json`) —
the object that already carries `project_id` and `relay_address` —
and the per-family map shall be keyed by the **CAIP-2 namespace**
identifiers §22.8 already binds as this subsystem's family taxonomy,
so that a family added later needs no new top-level key and no new
spelling convention:

```json
{
  "walletconnect": {
    "project_id": "<unchanged>",
    "relay_address": "<unchanged>",
    "enabled": true,
    "unnamed_family_policy": "deny",
    "families": {
      "bip122": { "withdraw": true, "swap": true },
      "eip155": { "withdraw": true },
      "cosmos": { "withdraw": true }
    }
  }
}
```

The two existing keys keep their present meaning and their present
lenient handling; this section adds keys to the object and amends
nothing already in it. Note in particular that `project_id` is
**required** today, so a node that configures none already fails to
bring the subsystem up. That is not an off switch and must not be
treated as one: it is a misconfiguration that surfaces as an error
at first use and it does not refuse activation. Making the off state
**explicit** and **early** is part of what RS1–RS3 and RS11 are
for — explicit, because `enabled: false` says what an absent
`project_id` only implies; early, because RS11 resolves it at
startup rather than at the first RPC.

**What this section does *not* claim, and what dropping the claim
costs.** It does not claim that an API caller can tell the
deliberate off state from a broken configuration **on the wire**.
RP7 fixes this subsystem's RPC surface at three `error_type`
tokens, one of which covers every initialisation and precondition
failure, and states that the human-readable message each token
carries is diagnostic and not part of the contract. A
disabled-by-configuration refusal therefore reaches a caller as the
same token, with the same status code, as a missing `project_id` or
an unreachable relay; a GUI could separate them only by
string-matching a message the contract disclaims. The off state is
distinguishable to the **operator**, in the node log and in the
configuration file they wrote, and that is the whole of it. The cost
is specific and worth naming rather than glossing: a GUI cannot
render "your administrator turned this off" differently from
"someone forgot to configure this", so the improvement RS1–RS3
deliver over the status quo reaches the operator and not the
operator's tooling. §22.9C.9 item 2 records what changing that
would take.

A flat list of independent booleans is explicitly rejected as the
shape. Flattened over the three families and the two operations plus
a global flag, it would be **seven booleans — 128 nominal states**,
of which a handful will ever be tested, and nothing in the shape
would say which handful. The nested shape collapses that
deliberately: `enabled: false` short-circuits the whole map (RS4),
so the states that need testing are the global off state, each
individual per-family switch off, and the unnamed-family case. The
test matrix is small by construction rather than by hope, and the
four switches §22.9C.5 actually specifies are fewer than the six the
flat shape would have invited — the two it does not specify are
exactly the ones RS5 forbids.

| Key | Type | Meaning |
|---|---|---|
| `walletconnect` | object | The existing block. Absent, or present without any key this section introduces, means "no policy expressed"; see RS9. |
| `walletconnect.enabled` | bool | The global switch of RS1. Default `true`. |
| `walletconnect.families` | object | Map from CAIP-2 namespace to that family's per-operation switches. |
| `walletconnect.families.<ns>.withdraw` | bool | Withdrawal switch for that family, where §22.9C.5 specifies one. Default `true`. |
| `walletconnect.families.<ns>.swap` | bool | Swap switch for that family, where §22.9C.5 specifies one. Default `true`. |
| `walletconnect.unnamed_family_policy` | `"deny"` / `"allow"` | What applies to a family the `families` map does not name, when the map is present. Default `"deny"`. |

**RS9 — Missing keys.** Absence is a determinate answer and shall
be resolved as follows:

| Case | Resolution |
|---|---|
| `walletconnect` absent, or present with none of this section's keys | The subsystem is available and every switch §22.9C.5 specifies is available. This is exactly today's behaviour, and is why the feature breaks no existing deployment. |
| `walletconnect` present, `families` absent | As above for the per-family switches; `enabled` is read as given. The operator expressed a global policy and no per-family policy. |
| `families` present, a family **named**, an operation key absent | That operation takes its default, available. |
| `families` present, a family **not named** | `unnamed_family_policy` governs, default `deny`. Writing a `families` map at all is therefore an **allowlist**, which is what keeps a family added to the code in a later release from becoming available by omission in an existing operator's configuration. |
| `enabled` absent | `true`. |
| `unnamed_family_policy` absent | `"deny"`. |

The asymmetry between the first case and the last is deliberate and
is stated so it is not later read as an inconsistency: an operator
who has written **no** WalletConnect policy gets the status quo,
including for a family added later, because that is what "default
available" means and anything else would silently change behaviour
on upgrade; an operator who has written a policy gets allowlist
semantics within it, because having written one they are entitled to
have it mean what it says. An operator who wants the strict reading
globally sets `enabled: false`, or writes a `families` map and
leaves `unnamed_family_policy` at its default.

**RS10 — Unknown keys and unswitchable combinations are startup
errors.** The configuration block shall be parsed strictly. Each of
the following shall stop startup with an explanatory error naming
the offending key, at the resolution point RS11 specifies — which is
what makes "stop startup" a statement about this build rather than
an aspiration. (R7 and D8 state the same intent for the
unimplemented `encrypted` value of `wc_session_persistence`, and
this section was drafted by analogy to them; but the code resolves
that value lazily too, so they are not the working precedent they
look like. §22.9C.9 item 1 records that as a pre-existing matter and
this section does not rely on it.) The strictness applies to the
keys this section introduces and to everything nested under them;
`project_id` and `relay_address` keep their existing handling and
are not made strict here.

1. an unknown key nested under `families`, or under a family entry,
   or an unrecognised sibling of `enabled` /
   `unnamed_family_policy` / `families` that is not one of the two
   pre-existing keys;
2. a family key that is not a CAIP-2 namespace this build supports;
3. an operation key that is not one of the operation names above;
4. a value of the wrong type;
5. an operation key naming a family/operation combination for which
   §22.9C.5 specifies **no** switch — for example `eip155` with a
   `swap` key.

Case 5 is the configuration-level expression of RS5: silently
accepting `{"eip155": {"swap": false}}` would hand the operator a
dormant switch through the configuration file rather than through
the code, and would report protection that does not exist. Cases 1
and 3 matter for the same reason in the dangerous direction: a
misspelled operation key that is silently ignored turns an intended
*deny* into an *allow*. When a refused combination becomes
reachable, RS5.2 requires the switch, its enforcement, its test
**and** its acceptance in this parser to land in one change.

The cost of strictness is that a configuration file shared across
node versions can fail on the older binary. That is accepted
deliberately: a security switch an operator believes they set is
worth more than a configuration file that loads everywhere, and the
failure mode chosen is the loud one.

**RS11 — One resolution point, one resolved value, one parser.**
Three enforcement points must agree on the same answer: the
activation refusal of RS2(1), the session-establishment refusal of
RS2(2), and the chokepoint of RS7. RS3 requires an enforcement point
that cannot reach the resolved value to fail closed, which is the
right behaviour but is not a design: it says what to do when the
value is unreachable without saying where the value lives. This
requirement says.

1. *One resolution point, and it is at startup.* The
   `walletconnect` object shall be resolved into the section's
   resolved-configuration value exactly once per node run, during
   node startup, before any RPC is served. A parse failure under
   RS10 stops startup there.
2. *The caller is named, and it is new work.* The resolution shall
   be invoked from the node's existing startup configuration-
   validation path — in this tree, `lp_init` in
   `mm2src/mm2_main/src/lp_native_dex.rs`, alongside the netid and
   passphrase-form validation it already performs and which already
   fails startup. **No such call exists today**, and nothing else in
   the tree resolves this object at startup: the only existing
   resolution runs inside the WalletConnect handle's construction,
   which is reached only from the lazy, cached first-use path behind
   the five public RPCs. An implementation that resolves the switch
   where the handle is built satisfies neither RS4.2 nor RS10 nor
   ACS6, however natural the placement looks.
3. *One cached value, read by all three points.* The resolved value
   shall be stored on the node context and read from there by every
   enforcement point. In particular the activation point of RS2(1)
   shall **not** re-derive it: when the switch is off no handle
   exists to read it from — ACS1 requires exactly that — so a
   second resolution in the activation crate is the implementation
   an implementer will otherwise reach for.
4. *Exactly one parser.* There shall be exactly one function in the
   tree that maps the `walletconnect` object to the resolved value.
   Two parsers for one security policy is the partial enforcement
   RS7 calls the worst outcome available here, reintroduced across
   *enforcement points* rather than across call sites: they agree on
   the easy cases and diverge on the hard ones — a wrong type, an
   unknown key, a `families` map present without the coin's
   namespace — and the divergence is invisible until it matters.

Resolving at startup also discharges RS8's claim that the off state
is surfaced **early**: with RS11 the off state and a broken
configuration are both known before the first RPC, which is the
improvement over `project_id`'s surface-at-first-use behaviour that
RS8 claims.

### 22.9C.8 Acceptance criteria

- ACS1. With `enabled: false`, activation of a coin naming a
  WalletConnect session fails with a structured error for each of
  the three request shapes of RP8, and no `WcCoinSigner` is
  constructed.
- ACS2. With `enabled: false`, no relay connection is established,
  no pairing is created, no persisted session is restored, and the
  three RPCs of §22.9A.2 that RS4.3 does not exempt refuse with a
  structured error rather than returning an empty result. The
  persisted records are still present afterwards, and a subsequent
  run with `enabled: true` restores them (RS4.1).
- ACS3. A `walletconnect` block that cannot be parsed into the
  resolved configuration results in the subsystem being disabled or
  startup being refused — never in the subsystem running with an
  assumed value (RS3). The resolver unit test is stated so that it
  is writable under either choice: on an error input the resolver
  **either yields *disabled* or returns an error that stops
  startup, and never yields *available***. (RS10 and RS11 make
  startup refusal the choice for the keys this section introduces,
  so that is the arm to expect; the criterion is written to the
  property rather than to the mechanism because RS3 permits both and
  refusing startup is the stronger of the two.) A separate test
  asserts that the **absent** block yields *available* (RS9).
- ACS4. **Per switch**, for each of the four switches of §22.9C.5:
  the operation fails with a structured error when that switch is
  off, and proceeds when it is on, with no other switch changed.
  Four pairs of assertions, one pair per switch. This criterion is
  the discharge of RS5 and is not satisfied by a sample of the
  switches.
- ACS5. With `families` present and not naming `cosmos`, a
  Tendermint withdrawal under a WalletConnect key source is refused
  (`unnamed_family_policy` default `deny`); with
  `unnamed_family_policy: "allow"` and the same map, it proceeds.
- ACS6. Each of the five cases of RS10 stops startup with an error
  naming the offending key — *startup*, at RS11's resolution point,
  not the first WalletConnect RPC; a test that drives the resolver
  in isolation does not discharge this criterion, because the defect
  it exists to catch is a misconfigured node running happily until
  somebody calls a WalletConnect RPC. In particular
  `{"eip155": {"swap": false}}` is rejected rather than accepted as
  a dormant switch.
- ACS7. The chokepoint is the only route, tested over the **funnel**
  and not only over the trait (RS7.2). A compile-fail test (or an
  equivalent build-time check) fails to build a caller outside the
  chokepoint's module that attempts each of: a `WcCoinSigner`
  signing or sign-and-send method; `send_session_request`; and the
  public session-key accessor. Three cases, because a check that
  covers only the first certifies a property the second and third
  defeat. The criterion is not satisfied by a test that asserts the
  chokepoint refuses when a switch is off — that is ACS4 — nor by
  one that asserts the trait alone is unreachable.
- ACS8. With no `walletconnect` block in the configuration, every
  operation §22.9C.5 names behaves exactly as it does without this
  feature. No existing deployment changes behaviour on upgrade.
- ACS9. A Tendermint coin activated under a WalletConnect key
  source is refused entry to the legacy HTLC paths at order
  placement / swap start with a structured unsupported-operation
  error, and never reaches the node-public-key fallback of
  `derive_htlc_pubkey` (RS6.1). The criterion covers a Tendermint
  **token** whose platform coin is so activated as well as the
  platform coin itself: the token forwards the per-swap key accessor
  to its platform coin, so a test that exercises only the platform
  coin does not discharge this criterion.
- ACS10. A UTXO coin activated under a WalletConnect key source is
  refused at the swap-protocol-v2 per-swap **public-key** accessor
  with a structured unsupported-operation error, and never returns
  a public key from it (RS6.2). The negative half matters as much as
  the positive one: the test asserts that no public key is produced,
  not merely that a later signing step fails.
- ACS11. Two halves, both required (RS7.3). (a) A call at the
  chokepoint carrying the residual operation class is refused with a
  structured unsupported-operation error, and is refused with the
  global switch *on* and with every per-family switch at its
  available default — so the test proves the refusal is not the
  global switch firing. (b) A mechanical check enumerates every call
  site that reaches the chokepoint together with its declared
  operation class, and fails when a call site is added without an
  enumerated class. A reviewer can read the enumeration and compare
  it to the fund-moving RPC routes; that comparison is the thing
  ACS4's per-switch matrix cannot make.
- ACS12. With `enabled: false`, the operator can still complete a
  purge (RS4.3): the enumeration RPC lists the persisted records,
  the deletion RPC removes a named one, and a subsequent
  enumeration no longer shows it. The test also asserts the
  negatives that make the exemption safe — no relay connection is
  established and no outbound message is published by either call —
  so that the exemption cannot be satisfied by quietly building the
  handle the rest of RS2(2) refuses.
- ACS13. One resolution point and one parser (RS11). A mechanical
  check — the same kind ACS7 and ACS11(b) ask for — asserts that
  exactly one function in the tree maps the `walletconnect` object
  to the resolved value, and that the activation enforcement point
  of RS2(1) reads the cached resolved value rather than parsing the
  configuration itself. Separately, a test asserts that a
  configuration rejected under RS10 stops startup **before** any
  RPC is served, which is the half of ACS6 that a resolver unit test
  cannot reach.

### 22.9C.9 Pre-existing matters this section depends on (maintainer decision)

Two things this section leans on are inaccurate or insufficient in
**pre-existing** material. In both cases the requirements of this
section have been written so that they are true and implementable on
their own, without relying on the pre-existing statement; neither
pre-existing requirement is amended here, because amending binding
material that other work depends on is a larger decision than
revising this section. They are recorded so that the next reader
finds the problem described rather than rediscovers it, and so that
the decision is visibly outstanding rather than silently taken.

**1. "Stops startup" is not what the code does for
`wc_session_persistence`.** §22.5.1's `encrypted` row, R7 and D8 all
state that selecting `encrypted` must stop startup with an
explanatory error. It does not: the setting is parsed inside the
WalletConnect configuration resolution, which runs inside the
handle's construction, which is reached only from the lazy, cached
first-use path behind the five public RPCs. A node configured with
`encrypted` therefore starts normally and fails the **first
WalletConnect RPC**. §22.9C was drafted by analogy to that pattern
and inherited the inaccuracy; RS4.2, RS10, ACS3, ACS6 and RS11 have
been rewritten so that they stand on RS11's own startup resolution
point instead, and they no longer cite R7 / D8 as a precedent the
code delivers.

*What a real fix requires, and why it is not taken here.* Either
(a) §22.5.1 / R7 / D8 are amended to say "at first use", which makes
the chapters match the code and weakens a stated protection against
a misconfigured node running on an unimplemented at-rest format; or
(b) the `wc_session_persistence` resolution moves to the startup
resolution point RS11 introduces, which makes the code match the
chapters and changes the behaviour of an existing, implemented,
released path. Option (b) is cheap once RS11 exists — the resolver
is already written and would simply be called earlier, with the
handle reading the already-resolved value — and it is the option
this section's author would choose. It is still a behaviour change
to implemented material under a requirement marked implemented,
which is a maintainer's call and not a revision's. **Decision
needed: (a) or (b).** Until it is taken, R7 / D8 and §22.5.1 stand
as written and the discrepancy is this item.

**2. RP7's three error tokens cannot carry an on-the-wire "disabled
by configuration" signal.** RP7 fixes the subsystem's RPC error
surface at three `error_type` tokens — an initialisation /
precondition failure (400), a session-request failure (500) and a
generic internal failure (500) — and states that the human-readable
messages are diagnostic and not part of the contract. Every
initialisation failure collapses into the first token today, so a
missing `project_id`, a bad `wc_session_persistence`, an unreachable
relay and a deliberate off state are indistinguishable to a caller
except by string-matching a disclaimed message. RS8 has been
written to drop the on-wire distinguishability claim and to state
what dropping it costs; nothing in this section now requires a
property RP7 forecloses.

*What a real fix requires, and why it is not taken here.* Either
(a) a **fourth** `error_type` token for "disabled by configuration",
which amends a pre-existing wire contract marked implemented; or
(b) a contractual discriminator inside `error_data`. Option (b) is
worth noting because it is cheaper than it first looks: RP7
constrains `error_type` and is silent about the *content* of
`error_data`, so admitting a discriminator there arguably widens
nothing RP7 states. It is still a change to the implemented wire
shape — all three tokens carry a bare string payload today, so a
structured payload for one of them is a shape change a client could
be parsing — and therefore still a wire-contract decision rather
than a revision's. **Decision needed: (a), (b), or accept that the
off state is distinguishable only in the log.** The third option is
what this section is written against today, so no work is blocked by
leaving the decision open; what is lost is one of the three
advantages RS8 claims over the status quo, and RS8 now says so.

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
  designations (see *Forbidden corpus* below). For §22.9C
  additionally, and for that section exclusively: this repository's
  own post-baseline source tree, read for the reachability facts the
  section's switch determination rests on (the per-swap key
  accessors of the UTXO and Tendermint families, the legacy swap
  machine's absent-key substitution, the activation-policy types,
  and the WalletConnect handle's own request surface); this document
  set's own chapters 09, 47, 49, 51 and 55 and the earlier sections
  of this chapter; and the project's own in-tree conventions
  `COMPAT_SWITCHES.md`, `GLEEC_COMPATIBILITY.md` and
  `CODING_STANDARDS.md`, consulted in order to state that §22.9C is
  *not* governed by them. §22.9C took no input of any other kind
  (see *Clean-room-original material* below).
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
  R31 source (see *Forbidden corpus* below). §22.9C rests on
  **R7 (independent work)** together with R1 and this project's own
  post-baseline clean-room output and cross-chapter contracts, and
  on no other class.
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

  The four exceptions above describe §22.5.3, §22.7.1/R11, §22.8.1
  and §22.9B. None of them reaches §22.9C, and this record is not
  widened to cover it — the next item states §22.9C's position
  instead, and states a different thing.
- *Clean-room-original material:* this item scopes §22.9C in full
  (RS1 through RS11 with their sub-numbered parts, and ACS1–ACS13),
  the §22.0 paragraph headed
  *Operator availability switches*, deferrals D12 through D15, and
  the two §22.12 additions that describe them — and nothing else.
  (It is therefore disjoint from the *Dirty Gate clearance* item
  below, which scopes the §22.0 paragraph headed *What is not yet
  reachable* and the §22.9B material.) This material derives from
  **no forbidden-class input** of any of the classes R8–R10. It is
  not a port, a restatement, or a reconstruction of any external
  behaviour: it is this project's own design for its own
  operator-facing security controls, and it makes no claim, positive
  or negative, about whether any other implementation offers
  anything like it — no such question was asked, and answering one
  would have changed this record. Its inputs are the ones the
  *Inputs* item scopes to §22.9C — this repository's own
  post-baseline tree, this document set's own chapters, and the
  project's own conventions — which is R7 independent work. It therefore required
  **no** §1.4A two-team pass: no Spec Reader authored it, no Dirty
  Gate cleared it, and R38's mandatory route does not apply because
  no forbidden-class input was taken to establish any of its
  content. No clearance is pending for it, and the absence of a
  clearance record is the correct state rather than an omission. The
  one judgement in §22.9C that reads an external record at all —
  RS6's use of RP9 as evidence of reachability — reads it from
  §22.9B of this chapter as already-cleared chapter text, not from
  any forbidden-class source.
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
