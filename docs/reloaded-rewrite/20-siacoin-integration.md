# Chapter 20 -- Siacoin Network Integration

**Status:** driving-spec

> **One-sentence claim:** the project shall provide a Siacoin
> coin-support module that activates a single-address Sia wallet
> over a walletd HTTP backend, reports balance and broadcasts
> value transfers in Sia's native units, and participates in the
> V1 atomic-swap protocol by realising the HTLC as a native Sia
> spend policy whose success path is unlocked by a SHA-256
> preimage and whose refund path is unlocked by a block-height
> time lock -- conforming on the wire to the public Sia
> consensus/transaction formats and to the walletd HTTP API.

## 20.0 Executive Summary

Siacoin support is a coin-support module within the workspace's
multi-protocol coin crate. It is **not** a UTXO or EVM family
member: Sia has its own consensus model, its own address and
unit encodings, and its own daemon-side wallet (walletd) that
holds chain state. The module therefore owns no local persistent
chain store; it queries walletd on demand.

The integration delegates **all chain-side primitives** -- key
handling, address derivation, transaction construction and
signing, spend-policy HTLC construction, and the walletd HTTP
client -- to an external Sia Rust library bound through that
library's public API. This chapter's subject is the *KDF-side
adapter*: the code that makes a Sia wallet present as a coin to
the rest of the workspace (the coin trait surface, the
activation flow, the swap-protocol participation, the withdraw
path, and the deferred-work boundary).

The bound surface covers:

- a public coin type, generic over its HTTP backend, that the
  workspace's coin enum carries as a dedicated variant;
- a coin-activation RPC surface and a one-time DEX-fee address
  resolution keyed to the active network id;
- the protocol primitives and unit conventions Sia dictates;
- the atomic-swap HTLC realised as a native Sia spend policy,
  and the V1 swap-protocol obligations the module must meet;
- the walletd HTTP endpoints the module consumes;
- the value-transfer (withdraw) behaviour, including the
  transaction carrier the withdraw response and the legacy
  broadcast method exchange; and
- the explicitly deferred pieces (multi-account HD, history
  persistence, swap-spend search, message signing, V2 swap
  protocol, watcher eligibility).

> **Binding scope (R36).** Throughout this chapter, requirements
> bind observable *behaviour*, the *public* cross-crate interface
> the rest of the workspace calls, and externally *dictated*
> interop (the Sia consensus/transaction wire formats, the
> walletd HTTP API, the unit and address encodings, the SLIP-44
> coin type, the SHA-256 HTLC secret hash, and the public
> activation RPC method strings -- marked R29/R31/R33). The
> private types, field layouts, helper decomposition, control
> flow, local names, transaction-builder call chains, and
> diagnostic wording an implementation uses to meet these
> requirements are informative, not mandated. Where a fragment is
> reproduced because the wire format, the daemon API, or a public
> crate API dictates it, that is stated explicitly and
> distinguished from the discretionary Rust shape around it.

## 20.1 Baseline Verification

The Siacoin integration is a **post-2022** feature. It is absent
from the baseline anchor of [Chapter 02](02-baseline-state.md)
(commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`, 3 June 2022):
no Siacoin coin-support module, no Sia activation path, and no
Sia variant in the workspace coin enum exist at that revision.
The classification here is by *component role and first-
introduction epoch only*; it is derived from the project's own
revision history and does **not** transcribe code.

| Component (role) | Basis |
| --- | --- |
| Siacoin coin-support module (coin type, activation, swap-ops, withdraw) | Introduced after the baseline anchor. |
| Sia variant in the workspace coin enum | Introduced after the baseline anchor. |
| Sia coin-activation task surface | Introduced after the baseline anchor. |

Per §1.3, the entire module is in clean-room remediation scope
(it is **not** baseline-carryforward). This chapter is therefore
a clean-room driving-spec: it states the integration's contract
by behaviour and public/dictated interface only, and does not
reproduce the authorial expression of the post-2022 module.

## 20.2 Subsystem Placement

The Sia coin-support module lives inside the workspace's
multi-protocol coin crate, alongside the UTXO, EVM, and
Tendermint families. The split of responsibilities is binding;
the file layout that realises it is discretionary (R36):

- a **public coin type** and its cross-crate trait
  implementations (§20.3, §20.7);
- a **configuration / activation-request** surface and the
  builder that produces an activated coin (§20.4);
- a **protocol-primitive / unit** surface mediating between Sia's
  native units and the workspace's decimal-amount type (§20.5);
- a **swap-protocol** surface realising the HTLC lifecycle
  (§20.6, §20.7);
- a **withdraw** surface (§20.9).

All chain-side primitives are supplied by an external Sia Rust
library consumed through its public API. The module binds that
library; it does not vendor or reproduce its internals. The
network-id-keyed DEX-fee configuration is read from the
workspace's network-config substrate (see
[Chapter 06](06-network-id-seed-node.md)).

## 20.3 Public Coin Type and Cross-Crate Surface

The module exposes a public coin type that is **generic over its
HTTP backend**: the coin is parameterised by the Sia API client
trait(s) it talks to, so that a test build can substitute a mock
client without compiling out production behaviour, and a public
type alias fixes the concrete production client for normal use.

> **Binding scope (R36).** The genericity over the API-client
> trait and the existence of a concrete production alias are
> binding architectural facts. The exact struct name, its field
> set, the interior-mutability choices (e.g. atomic vs. locked
> storage of the confirmation count), and the private field used
> to cache the fee address are discretionary realisation details.

Binding cross-crate facts:

**R-T1.** The coin type MUST implement the workspace's standard
coin trait surface -- the general coin trait, the market/
chain-operations trait, and the V1 swap-operations trait -- so
that the swap engine, the RPC layer, and the activation layer
interact with Sia through the same abstractions they use for
every other coin. These traits are the *public* cross-crate
contract; their method semantics are bound by the coin-trait
and swap chapters, not redefined here.

**R-T2.** The coin type MUST be carried by the workspace coin
enum as a dedicated Sia variant, so that coin-generic call sites
(swap engine, RPC dispatch, balance reporting) can dispatch on
it.

**R-T3.** The coin MUST carry a cross-coin private-key policy
value (the same policy abstraction used by other coins: legacy
single-key, derived HD account, hardware-wallet placeholder).
At the time of writing only the single-key and the single-
address HD-account paths reach a successfully activated coin;
every other policy value MUST fail activation with a typed
error rather than panicking.

**R-T4.** The DEX-fee destination address MUST be resolved
exactly once, at activation, and cached on the coin; per-swap
code MUST reference the cached value rather than re-deriving it.

## 20.4 Activation and Fee-Address Resolution

Siacoin activates through the workspace's **standalone-coin
task-activation** mechanism. The public RPC method strings are
dictated by the activation framework's namespace convention and
are part of the external API contract:

| Method string | Role |
| --- | --- |
| `task::enable_sia::init` | Begin Sia activation (long-running task) |
| `task::enable_sia::status` | Poll activation status |
| `task::enable_sia::user_action` | Supply user action (e.g. HW interaction) |
| `task::enable_sia::cancel` | Cancel activation |
| `enable_sia` | Legacy one-shot activation |

> **Binding scope (R36).** The method strings above are dictated
> public API (R33). The activation-request shape is a public
> deserialization contract: it MUST carry at least the walletd
> client configuration (base URL and auth, owned by the client)
> and the required-confirmations setting, and it MAY carry a
> single-address-mode gap-limit field that is currently accepted
> but not yet acted upon. The concrete Rust type name and field
> identifiers of that request are discretionary except where a
> field name is itself part of the JSON contract a caller sends.

**R-A1 (activation flow).** Activation MUST:

1. Parse the coin's static configuration (ticker, required
   confirmations) from the coin's JSON config entry.
2. Derive the active Sia keypair from the supplied private-key
   policy: a single-key policy yields the key directly; an
   HD-account policy yields a single derived address via SLIP-10
   ed25519 derivation (§20.4.1); any other policy fails with a
   typed error.
3. Construct the walletd HTTP client from the request's client
   configuration. The client owns the walletd base URL and auth;
   **no walletd URL is embedded in the module.**
4. Resolve the DEX-fee address for the active network id
   (§20.4.2) and cache it on the coin.

### 20.4.1 HD derivation path (dictated coin type)

When the HD-account policy is used, the module derives a single
address using SLIP-10 ed25519 derivation under the Sia BIP-44/
SLIP-44 registered **coin type 1991**. The coin-type value is
dictated by the public SLIP-44 registry; operating in
single-address mode (one derived address per HD account, rather
than full multi-account discovery) is a project convention bound
as behaviour (§20.10 D1 names the multi-account gap). The
account/change/index components below the coin type are fixed
for single-address mode.

> **Binding scope (R36).** Coin type `1991` and the SLIP-10
> ed25519 scheme are dictated (R29/R33). The exact derivation-
> path string constant, the lazy-static carrier, and the
> single-address-mode index choices are discretionary.

### 20.4.2 DEX-fee address resolution (dictated by fee routing)

The module MUST look up the DEX-fee public key configured for
the active network id (via the network-config substrate of
[Chapter 06](06-network-id-seed-node.md) / the fee-routing engine
of [Chapter 08](08-fee-routing-engine.md)), decode it as an
ed25519 public key, and derive the corresponding Sia address.
This is the only place the fee address is computed. The set of
per-network fee public keys is dictated by the fee-routing
configuration, not by this module.

## 20.5 Protocol Primitives and Units (dictated interop)

The following are dictated by the public Sia network protocol and
its public Rust library API; the module binds them, it does not
define them. They are reproduced here only as the interop contract
(R29/R31/R33):

| Primitive | Dictated form |
| --- | --- |
| Signature scheme | ed25519 |
| Public key | 32-byte ed25519 public key |
| Address | derived from the public key; Sia hex address with checksum |
| HTLC secret hash | SHA-256 of the swap preimage |
| Transaction model | Sia V2 transactions, JSON-serialised on the walletd wire |
| Transaction id | Sia transaction hash (`Hash256`) |
| Smallest unit | hastings; **1 SC = 10^24 hastings** |

**R-U1 (units).** All amounts the module exchanges with the rest
of the workspace MUST be denominated in whole SC using the
workspace decimal-amount type, converting to/from hastings at the
dictated `10^24` ratio. Balance reporting, withdraw amounts, and
swap amounts all cross this boundary.

> **Binding scope (R36).** The ed25519 scheme, the SHA-256 secret
> hash, the address/units encodings, and the V2-transaction JSON
> wire form are dictated by Sia and walletd. The Rust types that
> carry them (supplied by the external Sia library) are that
> library's public API; the module's private unit-conversion
> helpers and constants are discretionary.

## 20.6 Atomic-Swap HTLC as a Native Spend Policy

Sia has no Bitcoin-style locking script and no Solidity contract.
The HTLC is realised as a **native Sia spend policy** constructed
through the external Sia library's public API. The behavioural
contract the module MUST meet:

**R-H1 (HTLC output).** A swap payment MUST fund a transaction
output locked to a spend policy that encodes: the two
counterparties' ed25519 public keys, a block-height time lock,
and the SHA-256 secret hash. The funding output MUST be placed at
a fixed, deterministic position in the funding transaction so
that validation and spend code can locate it positionally without
scanning.

**R-H2 (success path).** The spending counterparty MUST be able
to claim the output by satisfying the policy with the secret
preimage together with a valid signature. A claim is only valid
when `SHA-256(preimage)` equals the secret hash embedded in the
output.

**R-H3 (refund path).** The original funder MUST be able to
reclaim the output after the time lock elapses by satisfying the
policy with a signature plus the elapsed time-lock condition.

**R-H4 (refund eligibility).** Whether the refund path is yet
spendable MUST be decided by comparing the chain's median block
timestamp against the locktime, using the walletd consensus
tip-state (§20.8), not local wall-clock time.

> **Binding scope (R36).** The atomic-swap spend-policy shape,
> its success/refund satisfaction forms, and the SHA-256 binding
> are dictated by the Sia consensus model and the external
> library's public spend-policy API (R29/R31). The local
> variable names, the builder/satisfier call chains, and the
> fixed output-index constant are discretionary.

## 20.7 V1 Swap-Protocol Obligations

The module implements the workspace's **V1** swap-operations
trait (the V2 protocol is deferred, §20.10 D5). This section
binds the swap obligations behaviourally; it does **not**
enumerate internal method bodies. The trait method *names* are
the public cross-crate contract defined by the coin crate and
bound by the swap chapters ([Chapter 13](13-swap-version-negotiation.md));
their per-coin realisation for Sia MUST provide:

**R-S1 (DEX fee).** Send the taker DEX fee as a Sia value
transfer to the resolved fee address (§20.4.2), carrying the swap
UUID in the transaction's arbitrary-data channel so the fee can
be correlated to the swap and validated.

**R-S2 (payments).** Send maker and taker HTLC funding payments
per §20.6 (R-H1).

**R-S3 (spends).** Spend a counterparty's HTLC payment via the
success path (R-H2), supplying the secret preimage; mirror for
both maker and taker roles.

**R-S4 (refunds).** Reclaim one's own HTLC payment via the refund
path (R-H3) once eligible (R-H4); mirror for both roles.

**R-S5 (validation).** Validate an observed DEX-fee transaction
(output destination, amount, embedded UUID, minimum height) and
an observed HTLC payment (output amount and that the output is
locked to the expected spend policy for the agreed parameters).

**R-S6 (secret extraction).** Given a spend transaction that took
the success path, recover the secret preimage from the satisfied
policy's revealed preimage(s) by matching `SHA-256(preimage)`
against the expected secret hash.

**R-S7 (payment lookup).** Answer "has my payment been sent" by
querying walletd for the relevant address event(s).

**R-S8 (swap-spend search).** The swap-spend search operations MUST
distinguish three outcomes, not two: a payment spent via the revealed
secret, a payment refunded via the timelock, and a payment that is
genuinely still unspent -- by walking the HTLC address's walletd
event log for the event that consumes the payment's HTLC output and
classifying it against the expected secret hash, as §20.10 D3
describes. Only a genuinely unspent payment MAY report "not found";
a query failure (the walletd lookup itself erroring) MUST NOT be
folded into "not found", because `recover_funds` (chapter 51 R33)
treats "not found" as a confirmed negative and acts on it -- collapsing
a real error into that outcome was the deferred-implementation-era gap
§20.10 D3 records, now closed.

**R-S9 (negotiated HTLC key).** The module MUST answer the two
coin-layer key operations the legacy negotiation depends on --
key derivation and counterparty-key validation, bound by
[Chapter 51](51-legacy-v1-swap-state-machine.md) R63 -- with its
own ed25519 key rather than with the node's secp256k1 key, which
Sia cannot sign with. Both sides use the fixed 33-byte field
width of chapter 51 R62, filled by the ed25519 convention that
chapter's R64 dictates: on send, the 32 native key bytes in the
leading positions with the final byte zero; on validate, exactly
33 bytes whose leading 32 bytes are a well-formed curve point; on
use, the leading 32 bytes. This is what makes the keys the two
peers negotiate agree with the keys the spend policies of §20.6
are built from.

**R-S10 (negotiated secret hash MUST be the 32-byte algorithm).**
The `secret_hash` a Sia-involved legacy swap negotiates and that
this module's payment-argument parsing consumes (R-S1 through
R-S5) MUST be the 32-byte `SHA-256(secret)` value, never the
20-byte default -- [Chapter 51](51-legacy-v1-swap-state-machine.md)
R72 makes this selection mandatory, on either the maker or the
taker side, precisely because §20.6's native spend-policy hash
lock (R-H1, R-H2) has no provision for being satisfied by, or
constructed from, any other width. This module's own argument
parsing is not the place that width is enforced or relaxed: it is
entitled to assume chapter 51's negotiation layer already
delivered a 32-byte value, and MUST continue to reject a
`secret_hash` of any other length as a parse failure rather than
attempt to pad or reinterpret it. Chapter 51 §51.9.1 records, as a
code-quality finding, that this repository's negotiation layer
does not yet perform the R72 selection for Sia; until it does,
this module's own correct 32-byte-only parsing cannot by itself
make a Sia-involved legacy swap complete.

> **Binding scope (R36).** This section binds swap *behaviour*
> and references the public trait surface only. No per-method
> table keyed to private helpers, no method bodies, and no
> diagnostic strings are part of this contract.

## 20.8 Walletd HTTP Surface (dictated API)

All chain interaction goes through the external Sia library's
walletd HTTP client. The endpoint paths below are the **public
walletd HTTP API** (dictated interop, R29/R33); the module
consumes them, and the base URL plus auth come entirely from the
activation request -- no URL is embedded in the module:

| Endpoint | Module use |
| --- | --- |
| `GET /api/consensus/tip` | Current chain height and state |
| `GET /api/consensus/tipstate` | Recent block timestamps; the median is the refund-eligibility cutoff (R-H4) |
| `GET /api/addresses/:addr/events` | Address event/transaction history |
| `GET /api/addresses/:addr/outputs/siacoin` | UTXO set for funding |
| `GET /api/outputs/siacoin/:output_id/spent` | Locate a spending transaction |
| `GET /api/txpool/transactions` | Mempool inspection |
| `POST /api/txpool/broadcast` | Broadcast a signed transaction |

## 20.9 Withdraw (Value Transfer) Behaviour

The module provides a withdraw path producing a signed,
broadcastable Sia transaction for a plain value transfer. The
behavioural contract:

**R-W1.** Resolve the source address from the active keypair and
the destination address by parsing the request string as a Sia
address.

**R-W2.** Fetch the source address's UTXO set from walletd and
assemble a V2 transaction with one output of the requested amount
and a change output back to the source address.

**R-W3.** Set the miner fee from a size-aware estimate: a probe
transaction's real serialized weight times walletd's current
hastings-per-byte rate, recomputed as each candidate input is
added (§20.10 D6, closed). The request does not carry a
fee-override value for this path.

**R-W4.** Sign the transaction with the active keypair and return
the serialised transaction together with fee/amount details
denominated in SC (converted from hastings per R-U1). The
withdraw (and tx-history) response's `fee_details` object MUST
carry the fee amount under the field name `total_amount`, not
`amount`. This is a bound wire-shape fact, not a discretionary
Rust field-naming choice: every other coin family's single-value
fee-details shape in this workspace's withdraw/tx-history response
contract names its comparable field `amount` (the convention this
project's own 2019-baseline UTXO fee-details shape established,
and that every later single-value fee-details shape except Sia's
has followed), but Sia's fee-details shape has been checked
against the reference corpus at both the stable-legacy and current
v3 reference points (AGENTS.md §3) and is confirmed, consistently
at both points, to name this field `total_amount`. Preserving that
name is required by this project's RPC-response wire-compatibility
policy; it is not an accidental repo-internal naming inconsistency
to be corrected by renaming to `amount`.

> **Binding scope (R36).** The `fee_details.total_amount` field
> name bound above is dictated wire-shape (a public JSON-RPC
> response contract field), sourced as Interop/wire-compatibility
> reuse, not private expression. The Rust struct name that carries
> it, its derive list, and its module location remain discretionary
> (§20.0, §20.3).

**R-W5.** This withdraw path is the coin-generic `WithdrawOps`
implementation the framework's shared withdraw RPC surface drives.
It is therefore reachable both through the direct `withdraw` method
and through the generic `task::withdraw::{init,status,user_action,
cancel}` family, on the same terms as every other coin family that
does not define coin-specific task-withdraw behaviour of its own;
the request/response wire contract for both paths is bound by
[Chapter 49](49-withdrawal-task-path.md), not by this chapter. This
chapter binds only the Sia-specific fee/signing behaviour of
R-W1--R-W4, the Sia-specific transaction-carrier and
record-identity fields of R-W6--R-W11, and the SC-specific part of
the legacy broadcast method's request contract (R-W8).

### 20.9.1 Transaction carrier in the withdraw response (dictated interop)

**R-W6 (`tx_hex` -- mandatory, authoritative).** The completed
transaction-details object a Sia withdrawal returns MUST carry
`tx_hex`, non-empty, on exactly the mandatory terms
[Chapter 49](49-withdrawal-task-path.md) R49.25 states for every
coin family. Sia is the case where a coin's *native* serialisation
of a signed transaction is **JSON text** rather than a binary
encoding, so for this coin `tx_hex` is the lowercase hex encoding
of the **UTF-8 bytes of that Sia-native transaction JSON** -- hex
of JSON text, not hex of a binary wire form. Hex-decoding `tx_hex`
MUST yield exactly the byte sequence the bound Sia library
(§20.2) produces when it serialises the signed V2 transaction, so
that the decoded bytes parse directly as that library's V2
transaction type. A withdrawal that cannot produce that
serialisation MUST fail with a structured withdrawal error;
returning an empty, default, or otherwise placeholder `tx_hex`
while reporting success is a defect, because R49.25 makes the
field mandatory for a completed withdrawal and every downstream
broadcast path reads it.

**R-W7 (`tx_json` -- additional carrier).** In addition to R-W6,
the Sia transaction-details object MUST carry a field named
exactly `tx_json`, placed at the **top level** of that object (a
sibling of `tx_hex`, `tx_hash`, `from`, `to`, `fee_details`, ...,
not nested inside any of them). Its value is the Sia-native
signed-transaction **JSON object itself** -- the identical
serialisation R-W6 hex-encodes, emitted unencoded. For one
completed withdrawal, hex-decoding `tx_hex` and parsing the result
as JSON MUST produce the same JSON value as `tx_json`: the two
fields are two encodings of one transaction and never two
different transactions. `tx_json` is coin-specific. It is absent
from the transaction-details object of a coin family that has no
native JSON transaction form, and a consumer MUST read its absence
as "this coin has no JSON carrier", never as an error.

> **Deliberate superset (informative).** The two external contracts
> this project tracks for SC disagree about the carrier. The
> platform's published public API reference documents `tx_hex` as
> the sole transaction carrier for a withdrawal and defines no
> `tx_json` field for any coin. The deployed interoperability
> reference for SC does the opposite: for this one coin it carries
> the signed transaction only as a top-level `tx_json` object and
> emits no `tx_hex` at all. This project resolves the disagreement
> by emitting **both** -- `tx_hex` stays authoritative and
> mandatory (R-W6), `tx_json` is added alongside it (R-W7) -- which
> is a strict superset of both shapes, so an integration written
> against either one keeps working unchanged. The addition is
> purely additive: it removes no field and changes no existing
> field's name, type, or meaning. A client that has only ever read
> `tx_hex` is unaffected; a client written against the SC-only
> shape finds `tx_json` where it expects it.

**R-W8 (broadcast contract -- `send_raw_transaction`).** The legacy
`send_raw_transaction` JSON-RPC method MUST accept a Sia
transaction in **either** carrier, so that a caller can feed back
whichever half of the R-W6/R-W7 pair it kept:

| Request field | JSON type | Interpretation for SC |
| --- | --- | --- |
| `tx_hex` | string | hex; the decoded bytes are the Sia-native transaction JSON text |
| `tx_json` | object | the Sia-native signed-transaction JSON object, taken as-is |

The contract is:

- **Precedence.** When both fields are present, `tx_hex` wins and
  `tx_json` is ignored. One deterministic precedence rule applies
  to every coin, so a caller that echoes back a whole
  transaction-details object -- which for SC carries both -- always
  broadcasts the same transaction regardless of which coin it
  holds.
- **Neither present.** The call MUST fail with a request-validation
  error that names both accepted fields, and MUST NOT broadcast.
- **Selected carrier unparseable.** When the selected carrier is
  present but cannot be decoded -- `tx_hex` is not valid hex, or
  `tx_json` is not a JSON object, or the resulting bytes do not
  parse as a signed Sia V2 transaction -- the call MUST fail with a
  structured error and MUST NOT silently fall through to the other
  carrier. A malformed `tx_hex` is an error, not a reason to try
  `tx_json`. Nothing is broadcast on any failure path.
- **Success.** On success the response carries the broadcast
  transaction's id under `tx_hash`, unchanged from the shape every
  other coin family returns from this method.

No chapter of this document set presently binds
`send_raw_transaction`'s request or response shape; the method is
recorded only as a supported legacy method in the document set's
RPC-method census
([`rpc-method-census.md`](rpc-method-census.md)), and
[Chapter 47](47-metamask-integration.md) refers to it for the EVM
MetaMask follow-up case only. R-W8 is therefore the binding
statement of the SC-specific part of that method's request
contract; the coin-generic part of the method is unchanged by this
chapter.

### 20.9.2 Record-identity fields in the withdraw response

**R-W9 (`transaction_type`).** A completed SC withdrawal's
transaction-details object MUST set `transaction_type` to the
v2-transaction wire value [Chapter 53](53-sia-transaction-history.md)
R53.5.10 reserves for Sia (`SiaV2Transaction`). It MUST NOT be
left at the shared enumeration's default standard-transfer member.
The reconciliation with ch. 53 is that the wire value denotes
*what the record is* -- a Sia v2 transaction -- and not *which
subsystem produced it*: a record the withdraw path returns and a
record the history path later projects from the same transaction's
walletd event (ch. 53 §53.5) describe one transaction and MUST
agree on this field. The deployed interoperability reference also
reports the Sia v2-transaction value on its withdraw path, so this
is compatibility restoration, not a divergence. Emitting the
default standard-transfer member for an SC withdrawal is a defect
against both ch. 53 R53.5.10 and the reference behaviour.

**R-W10 (`internal_id`).** A completed SC withdrawal's
transaction-details object MUST set `internal_id` to the **raw
bytes of the signed transaction's id** -- the same 32-byte value
whose lowercase hex form the same object reports as `tx_hash`.
This is the identity rule ch. 53 R53.5.2 already binds for a Sia
*history* record, applied to the withdraw path so that the two
paths agree: the record a caller receives from a withdrawal and
the record that later appears in that address's history for the
same transaction carry the same primary key, and a caller can join
them without re-deriving one from the other.

> **Upstream divergence (informative).** This is not
> compatibility restoration. The deployed interoperability
> reference leaves `internal_id` **empty** on its SC withdraw
> response, exactly as this project's current withdraw path does;
> only its history path populates the field. R-W10 is therefore a
> deliberate, documented improvement over the reference behaviour,
> not parity with it. It is safe as an additive change -- the
> field is mandatory in the R49.25 field set either way, and the
> reference merely returns an empty value for it, so populating it
> supplies information where a caller previously had none rather
> than changing the meaning of a value a caller could already rely
> on. A consumer that ignored `internal_id` on SC withdrawals
> because it was always empty is unaffected.

### 20.9.3 What a client can do with `tx_json` outside this project

**R-W11 (basis caveat).** The Sia-native V2-transaction JSON that
R-W6 hex-encodes and R-W7 emits does **not** include the
chain-index "basis" the walletd broadcast endpoint
(`POST /api/txpool/broadcast`, §20.8) requires alongside the
transaction: the basis is a broadcast-time parameter of the
endpoint, not a field of the Sia transaction structure, and it is
deliberately excluded from the transaction's serialised form. A
consumer that takes `tx_json` (or the decoded `tx_hex`) and
broadcasts it to walletd through its own client MUST therefore
supply a basis itself -- normally the current chain tip. This
chapter's own broadcast path (R-W8) and the bound Sia library
resolve the missing basis to the current tip, so a caller that
round-trips the transaction back through `send_raw_transaction`
never has to think about it; a caller that leaves this project's
RPC surface does. The transaction remains valid for broadcast only
while the inputs it spends remain unspent, so a `tx_json` held for
later is subject to the same staleness as any other pre-signed
transaction.

### 20.9.4 The same carrier pair on `get_raw_transaction`

This subsection sits with the other carrier rules rather than
with §20.10 D8's raw-transaction-fetch entry, because what it
binds is the carrier convention itself and a reader checking how
Sia carries a transaction should find every rule in one place.

**R-W12 (`get_raw_transaction`).** A successful
`get_raw_transaction` response for SC MUST carry the fetched
transaction in both carriers, on exactly the terms R-W6 and R-W7
bind for the withdraw path: `tx_hex` holds the lowercase hex of
the UTF-8 bytes of Sia's native transaction JSON, and a top-level
`tx_json` field holds that same JSON unencoded. Hex-decoding
`tx_hex` and parsing the result MUST yield the same JSON value as
`tx_json`. As in R-W7, `tx_json` is coin-specific: it is absent
from the response of a coin family with no native JSON
transaction form, and a consumer MUST read its absence as "this
coin has no JSON carrier", never as an error.

A failure to serialise the fetched transaction MUST be reported
as a structured error. Returning a success response carrying an
empty or otherwise placeholder `tx_hex` is a defect for the same
reason R-W6 gives for the withdraw path: every consumer of this
method reads that field, and an empty one is not a transaction.

> **Upstream divergence (informative).** The deployed
> interoperability reference returns only `tx_hex` from this
> method for SC, hex-encoding the same native JSON. Adding
> `tx_json` alongside it is additive on the same reasoning R-W7
> records for the withdraw path -- no field is removed and no
> existing field changes name, type or meaning -- and it removes
> an inconsistency this project would otherwise carry, where one
> method offers the JSON carrier and another does not for the
> same coin and the same transaction.

### 20.9.5 Verification

**T-W1.** Complete an SC withdrawal and inspect the returned
transaction-details object. It carries a non-empty `tx_hex` and a
top-level `tx_json` object. Hex-decode `tx_hex`, parse the result
as JSON, and compare with `tx_json`: the two are the same JSON
value (R-W6, R-W7).

**T-W2.** Take the same completed withdrawal and broadcast it
three ways through `send_raw_transaction`: with `tx_hex` alone,
with `tx_json` alone, and with both present. All three select the
same transaction and report the same `tx_hash`. Repeat with a
`tx_hex` that is not valid hex while a well-formed `tx_json` is
also present: the call fails and does not fall through to
`tx_json` (R-W8).

**T-W3.** Call `send_raw_transaction` for SC with neither `tx_hex`
nor `tx_json` present: the call fails with a request-validation
error and broadcasts nothing (R-W8).

**T-W4.** Compare a completed SC withdrawal's transaction-details
object with the history record that the history path later
produces for the same transaction (ch. 53 §53.5). Both report the
same `transaction_type` v2-transaction wire value, the same
`tx_hash`, and the same `internal_id` bytes (R-W9, R-W10; ch. 53
R53.5.2, R53.5.10).

**T-W5.** Fetch the same transaction through `get_raw_transaction`
and inspect the response. It carries a non-empty `tx_hex` and a
top-level `tx_json`; hex-decoding `tx_hex` and parsing the result
yields the same JSON value as `tx_json`, and that value equals the
`tx_json` the withdraw response reported for the same transaction
(R-W12).

> **Code-quality finding (informative) -- resolved.** The failure
> branch of serialising the signed transaction into `tx_hex` used
> to substitute an empty default instead of propagating an error,
> so a serialisation failure would have returned a *successful*
> withdrawal response carrying an empty `tx_hex` -- contradicting
> R-W6 and ch. 49 R49.25, and substituting a placeholder value in
> a funds-moving path. Both the withdraw path and the
> `get_raw_transaction` path (R-W12) now propagate a structured
> error instead, and the same serialisation feeds `tx_json` in
> each, so one fix covers both fields in both methods.
>
> One instance of the pattern deliberately remains, recorded as
> §20.10 D9: the coin-generic transaction trait's own
> hex-serialisation method returns a byte vector with no error
> channel, so its Sia implementation still substitutes an empty
> vector on failure. Closing it requires changing a trait shared
> by every coin family, which is out of proportion to a
> serialisation that cannot fail for this type in practice.

## 20.10 Deferred Work

The following are explicit gaps, documented as deferred work
rather than defects. None is a correctness claim.

- **D1 -- Multi-account HD.** *Closed.* `HDWalletOps`/`HDAccountOps`
  are implemented for Sia (`sia_hd_wallet.rs`), with gap-limit
  honouring and address discovery, mirroring the existing UTXO/ETH
  trait-conformance shape. Architecturally, SLIP-10 ed25519 (unlike
  BIP32 secp256k1) has no extended-*public*-key derivation, so
  `SiaHDAccount` carries the account-level *private* signing key
  rather than an xpub -- a discretionary consequence of the
  already-dictated derivation scheme (§20.4.1), not a deviation from
  it. A dedicated test proves multi-account (account=0, External,
  address=0) derives the exact same address the single-address path
  (§20.4.1) already uses for the same seed, so the two mechanisms
  cannot silently disagree. `SiaCoinBuilder`/activation,
  `CoinWithDerivationMethod`, and the HD RPC surface
  (`HDWalletRpcOps`/`AccountBalanceRpcOps`/`InitCreateHDAccountRpcOps`)
  now consume this module: `SiaCoinGeneric` carries a
  `derivation_method` field, and activation (`sia_activation.rs`)
  reports `wallet_balance` via the same generic
  `EnableCoinBalanceOps` shape UTXO/ETH use (ch.46 R46.2.4), branching
  automatically on Iguana vs. HD-wallet mode. Per ch.46 R46.1.3,
  activation itself never grows the wallet past account 0; multi-
  account growth happens entirely through the post-activation HD RPC
  surface. `InitCreateHDAccountRpcOps` could not delegate to the
  shared cross-coin helper verbatim (it always forwards a hardware-
  wallet xpub extractor, which Sia's ed25519 derivation has no
  software-*or*-hardware public-key-only path for) -- a Sia-specific
  software-only account-creation path was written instead. While
  wiring this, a **pre-existing, coin-agnostic bug** was found (not
  fixed, not Sia-specific): `coins::rpc_command::init_create_account.rs`
  constructs its xpub extractor eagerly rather than lazily, so
  `task::create_new_account` fails for *any* software-keyed coin
  (UTXO/QTUM/ETH included) without a connected hardware wallet --
  Sia inherits this identically once wired. The existing
  single-address activation path is observably unaffected either
  way. Code: `mm2src/coins/siacoin/sia_hd_wallet.rs`,
  `siacoin_helpers.rs`, `mod.rs`,
  `mm2src/coins_activation/src/sia_activation.rs`.

- **D2 -- History persistence.** *Closed by*
  [Chapter 53](53-sia-transaction-history.md), which binds Sia
  transaction history as a projection of walletd's per-address
  event log into the coin-generic runtime history store and gives
  the history-sync loop its contract. Sia still creates no
  per-coin SQL history tables and is still not classified through
  the shared history-coin-type contract; that residue is chapter
  53's D53.1.
- **D3 -- Swap-spend search.** *Closed.* The event-walk now walks
  the HTLC address's walletd event log (reusing
  `siacoin_history.rs`'s `fetch_all_events` paging), finds the event
  that consumes the payment's HTLC output, and classifies it
  spent-via-secret vs. refunded-via-timelock by reusing R-S6's
  secret-comparison logic. `search_for_swap_tx_spend_my`/`_other`
  return `Ok(Some(FoundSwapTxSpend::Spent(_)|Refunded(_)))` or
  `Ok(None)` (genuinely unspent) instead of the unconditional error
  this D3 entry previously described. `recover_funds` (chapter 51
  R33) can now correctly determine "already spent" instead of
  refusing outright. Code: `mm2src/coins/siacoin/siacoin_swap_ops.rs`.

- **D4 -- Message signing.** *Closed.* `sign_message`/`verify_message`
  are wired to Sia's ed25519 keypair, using the bound Sia client
  library's only signing primitive (no dictated message-prefix
  convention exists to follow). `verify_message`'s third parameter is
  a Sia public key, not a wallet address -- a plain ed25519 signature
  cannot be verified against an address (a one-way hash, not
  invertible to a pubkey); this crate's other ed25519 coin
  ([Chapter 40](40-solana-coin.md)) resolves the identical constraint
  the same way. Code:
  `mm2src/coins/siacoin/siacoin_market_ops.rs`.

- **D5 -- V2 swap protocol.** Neither V2 swap-operations trait is
  implemented for Sia; swaps involving Sia run only over the V1
  protocol ([Chapter 13](13-swap-version-negotiation.md)). Binding
  design drafted in [Chapter 54](54-sia-swap-v2-path.md) (status:
  DRAFT, not yet approved for implementation).
- **D6 -- Size-aware fee estimation.** *Closed.* The withdraw path
  now estimates the fee from a probe transaction's real serialized
  size (`V2TransactionBuilder::weight()`) times the live
  `GET /api/txpool/fee` rate, recomputed as each candidate input is
  added, rather than a flat constant. Sia's V2 wire encoding writes
  every `Currency` as a fixed-width `u128`, so weight depends only on
  input/output counts, never amounts -- a zero-valued placeholder
  transaction is exact, not approximate. Commit `836a6278c`.

- **D7 -- Watcher eligibility.** *Closed, partial by design.* Four of
  the five non-trivial `WatcherOps` methods are implemented for real:
  `watcher_validate_taker_fee` (partial -- `WatcherValidateTakerFeeInput`
  carries neither a swap uuid nor an amount, so it can only check the
  fee tx exists and pays the right address, not that it belongs to
  *this* swap or is the right amount), `watcher_validate_taker_payment`,
  `create_taker_payment_refund_preimage` (sound because Sia's V2
  signature hash provably excludes each input's `satisfied_policy`
  bytes, so a refund can be signed ahead of time using only this
  node's key), and `watcher_search_for_swap_tx_spend`.
  `create_maker_payment_spend_preimage` is judged infeasible and
  deliberately left on the trait's own graceful default rather than
  `unimplemented!()`: the trait supplies only `secret_hash`, never the
  secret, and this method is called before the secret exists to the
  caller; completing it needs `swap_watcher.rs`'s `SpendMakerPayment`
  state to splice a revealed secret into a pre-built transaction before
  broadcast, which it does not do for any coin today (a pre-existing,
  coin-agnostic gap, not Sia-specific). Commit `73ef8b608`.

- **D8 -- Raw-transaction fetch.** *Closed.* `get_raw_transaction` now
  parses the requested txid and tries a confirmed lookup via
  `sia_rust`'s `ApiClientHelpers::get_transaction` (verified, not
  assumed, against the bound library's own source: it is exactly
  `get_event(txid)` filtered to `EventDataWrapper::V2Transaction`, the
  same txid-scoped lookup D7's `watcher_validate_taker_fee` already
  uses), falling back to `get_unconfirmed_transaction` for a
  mempool-only transaction. Commit `02341d5d4`. The response's transaction carriers are bound
  by §20.9.4 R-W12, which this method satisfies on the same terms
  the withdraw path does.


- **D9 -- Placeholder byte vector in the coin-generic transaction
  trait.** The workspace's shared transaction trait exposes a
  hex-serialisation method returning a byte vector with no error
  channel, so Sia's implementation substitutes an empty vector if
  serialisation fails. This is the one surviving instance of the
  pattern §20.9.5's resolved code-quality note describes; the
  withdraw path and `get_raw_transaction` both propagate a
  structured error instead and no longer route through it.
  Serialising this type cannot fail in practice -- it is a plain
  struct with string-keyed maps and no non-finite numbers -- so the
  residual risk is theoretical. Closing it properly means giving
  that trait method a fallible signature, which touches every coin
  family and is therefore out of scope for a Sia change. Left open
  deliberately rather than worked around locally.

## 20.11 Baseline Verifications

The following are verifiable from the baseline state defined in
[Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree contains **no** Siacoin coin-support
    module. A directory listing of the baseline coin crate
    (`git ls-tree`) shows no Sia module directory, and a
    tree-wide `git grep -li 'siacoin'` against the baseline
    returns no coin-support matches, confirming the feature is
    post-2022.

V2. The baseline workspace coin enum carries no Sia variant, and
    the baseline activation layer registers no `enable_sia` /
    `task::enable_sia::init` surface. The activation method
    strings of §20.4 are therefore additions, dictated by the
    standalone-coin task-activation namespace convention.

V3. The unit ratio, ed25519 scheme, address encoding, walletd
    endpoint paths, and the SHA-256 HTLC secret hash of §20.5 and
    §20.8 are properties of the public Sia protocol and the
    walletd HTTP API, not artefacts of any project lineage.

## 20.12 Provenance Footer

- *Inputs:* the project's own revision history (used for the
  epoch classification of §20.1 and the baseline verifications of
  §20.11, by component role and first-introduction epoch only --
  no code transcribed); the baseline anchor of
  [Chapter 02](02-baseline-state.md); the public Sia network
  protocol (consensus/transaction V2 formats, ed25519 signatures,
  spend-policy model, hex address + checksum encoding, the
  hastings unit and the `1 SC = 10^24 hastings` ratio); the
  public walletd HTTP API (the endpoint paths of §20.8); the
  public SLIP-44 registry (Sia coin type `1991`) and SLIP-10
  ed25519 derivation; the public Sia Rust library API the module
  binds (the key/address/transaction/spend-policy and API-client
  types); the platform's published public API reference for the
  withdrawal response's transaction carrier (§20.9.1); and
  cross-chapter contracts (Chapters 06, 08, 13, 47, 49, 51, 53).
- *Permitted-input classes used:* baseline source (epoch
  classification and absence verification only); external public
  specifications (the Sia protocol and consensus/transaction
  formats, the walletd HTTP API, SLIP-44/SLIP-10, the public Sia
  Rust library API); cross-chapter contracts (Chapters 06, 08,
  13, 47, 49, 51, 53); Interop / wire-and-API-bound reuse (R29/R31/R33) for the
  dictated fragments embedded in §20.4 (the `enable_sia` /
  `task::enable_sia::*` public method strings), §20.4.1 (the
  SLIP-44 coin type and SLIP-10 scheme), §20.5 (the ed25519
  scheme, SHA-256 secret hash, address/units encodings, and V2
  transaction wire form), §20.6 (the spend-policy HTLC shape and
  its success/refund satisfaction forms), §20.8 (the walletd
  endpoint paths), and -- added in this revision -- §20.9 (the
  `fee_details.total_amount` withdraw/tx-history response field
  name) and -- added in this revision -- §20.9.1 (the `tx_hex` /
  `tx_json` withdraw-response carrier fields, their top-level
  placement, and the `tx_hex` / `tx_json` request fields and
  precedence of the legacy broadcast method) and §20.9.2 (the Sia
  v2-transaction `transaction_type` wire value, already bound by
  chapter 53 R53.5.10) -- whose authoritative source is the bytes and
  calls any conforming Sia node, walletd instance, or library
  consumer must exchange for interoperability, not the historical
  lineage's discretionary expression.
- *Sibling-allowlist consultations:* Chapter 06 (the network-id
  configuration the fee-address resolution reads); Chapter 08
  (the fee-routing engine that owns the per-network DEX-fee
  public keys); Chapter 13 (the swap version-negotiation path and
  the V1/V2 trait boundary the Sia swap-ops sit on); Chapter 51
  (the legacy negotiation's fixed-width key field, the two
  coin-layer key operations, and the ed25519 padding convention
  that R-S9 applies, and -- added in this revision -- the
  coin-pair secret-hash-algorithm-selection contract of R71
  through R73 that R-S10 applies); Chapter 49 (the mandatory
  completed-withdrawal field set of R49.25 that §20.9.1 R-W6
  extends for this coin); Chapter 53 (the record-identity and
  transaction-type rules R53.5.2 / R53.5.10 that §20.9.2
  R-W9 / R-W10 reconcile the withdraw path with).
- *Forbidden corpus:* consulted **only** to recover the
  externally-dictated public method strings of §20.4, the
  dictated derivation/coin-type and protocol/units facts of
  §20.4.1 and §20.5, the dictated spend-policy HTLC contract of
  §20.6, the dictated walletd endpoint paths of §20.8, and --
  added in this revision -- the `fee_details.total_amount` wire
  field name of §20.9 R-W4, the withdraw-response transaction-carrier
  facts of §20.9.1 (that the reference shape for this coin carries
  the signed transaction as a top-level `tx_json` object and emits
  no `tx_hex`, that its legacy broadcast method accepts either
  `tx_hex` or `tx_json` with `tx_hex` taking precedence, and that
  neither-present is a request-validation failure) and the
  record-identity facts of §20.9.2 (that the reference withdraw
  response reports the Sia v2-transaction `transaction_type` value
  and leaves `internal_id` empty), checked at both the `v2.6.0-beta`
  stable-legacy reference and the current `dev`/v3 reference per
  AGENTS.md §3, with no disagreement between the two -- embedded
  as Interop/wire-compatibility reuse. While checking §20.9's
  withdraw-response construction for the same category of
  unvalidated wire-shape choice, the module's DEX-fee-address
  resolution (activation-time lookup keyed by network id, already
  bound at §20.4.2) was also compared against the corpus and found
  to carry no additional wire-visible field or response-shape fact
  beyond what §20.4.2 already binds; it is an internal computation,
  not a JSON-RPC-visible contract, so no further R-W-series or
  R-A-series rule was added for it. R-S10 embeds no
  further corpus fact beyond what chapter 51 already records; it
  is a cross-reference stating this module's obligation given
  chapter 51's R72. No discretionary
  expression from the post-2022 module -- no struct field lists,
  internal type or enum definitions, private method names or
  bodies, transaction-builder/satisfier call chains, local
  variable names, per-method tables keyed to internal names,
  control-flow transcription, or diagnostic / panic / log string
  literals -- crosses into this chapter. All other content is
  clean-room driving-spec stated by behaviour and public/dictated
  interface. Any residual similarity of a conformant realisation
  to the historical lineage is governed by the R35 gate and the
  R36 binding-scope notes that head every code-bearing section of
  this chapter.
