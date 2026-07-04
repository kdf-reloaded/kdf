# 50. EVM Trezor / hardware-wallet transaction signing

**Status:** driving-spec (required port). This chapter specifies the
**implementation-level signing architecture** behind the EVM (EthCoin)
Trezor / hardware-wallet withdrawal path. It is the internal-behaviour companion
to the already-gated RPC/activation contracts:

- ch. 35 (R35.1.4 / R35.3.2) -- the `priv_key_policy` Trezor hardware policy and
  the `task::enable_eth` task variant that hosts its interactive user actions;
- ch. 47 (§47.5) -- the EVM signing-policy seam that already carries a `Local`
  variant and a WASM-only external-wallet (MetaMask) variant;
- ch. 49 (R49.6-R49.9, R49.17-R49.18, R49.22; T49.8-T49.11, T49.20-T49.22) --
  the EVM Trezor withdraw RPC contract, task status vocabulary, and PIN /
  passphrase user-action contract.

> **One-sentence claim:** the project shall add a **Trezor** variant to the EVM
> signing-policy seam (sibling to §47.5 `Local` / `Metamask`) so that an EVM coin
> activated under the Trezor hardware policy (R35.1.4) derives its address and
> account public key from the **device** at activation and, for a
> `task::withdraw::init` withdrawal (R49.6), builds an unsigned EIP-155 legacy
> transaction, drives the **public Trezor Ethereum wire protocol** to have the
> device sign it (surfacing device connect / confirm / PIN / passphrase through
> the withdrawal task's in-progress and awaiting-user-action states), assembles
> the returned `(v, r, s)` into the same RLP-encoded signed transaction a
> software signer would produce, and returns the standard transaction-details
> payload -- while the framework holds **no local secret**.

> **Treatment:** **T-PORT.** The reloaded base already ships: the EVM
> signing-policy seam with `Local`/`Metamask` variants (§47.5, ch. 47 substrate);
> the legacy EIP-155 EVM transaction builder and RLP encoder; the withdrawal task
> family and its status vocabulary (ch. 49); the shared hardware-wallet
> awaiting-status / user-action types (`EnterTrezorPin` / `EnterTrezorPassphrase`
> and their `TrezorPin` / `TrezorPassphrase` action payloads, R49.18 / R49.22);
> the `trezor` crate with its public Ethereum message set; and the
> `task::enable_eth` Trezor activation task (R35.3.2). What is **missing** and
> specified here is (a) the `Trezor` signing-policy variant and (b) the
> device-driven signing flow inside the withdrawal task.

> **Binding scope.** Requirements bind observable behaviour, the public mmrpc-2.0
> withdraw wire already fixed by ch. 49, and externally **dictated** interop: the
> public Trezor Ethereum wire protocol (`EthereumGetAddress`/`EthereumAddress`,
> `EthereumGetPublicKey`/`EthereumPublicKey`, `EthereumSignTx`,
> `EthereumTxRequest`, `EthereumTxAck`), EIP-155 replay protection, RLP encoding,
> and BIP-32/BIP-44 derivation paths. Those public contracts are the source of
> truth, not this project's code. Private Rust types beyond the named seam,
> helper decomposition, and internal module structure are informative and are
> **not** bound by this chapter.

> **Source of truth (informative).** The withdraw method strings, JSON field
> names, task-status wire values, and user-action discriminants are governed by
> ch. 49 and the published KDF API documentation. The Trezor Ethereum message
> names and field semantics are governed by the published Trezor firmware /
> protobuf protocol. Where this chapter and those public contracts disagree, the
> public contracts govern. Where they are silent, behaviour is distilled from
> current framework behaviour and flagged as an **Upstream divergence
> (informative)** note.

---

## 50.0 Executive summary

Under the Trezor hardware policy an EVM coin has **no local private key**; the
account key material never leaves the device. The framework's role is reduced to
(1) learning the account's address and public key from the device at activation,
(2) building an unsigned EVM transaction, (3) conducting the public Trezor
Ethereum signing exchange, and (4) assembling the device's signature into the
final signed transaction.

| Concern | Contract |
| --- | --- |
| Signing seam | a `Trezor` variant on the EVM signing-policy seam (§47.5), sibling to `Local` / `Metamask` |
| Key custody | none local -- address + account public key come from the device (R50.1) |
| Withdraw entrypoint | `task::withdraw::init` only (R49.6); direct `withdraw` is not required to support Trezor |
| Transaction type built | legacy EIP-155 (the reloaded EVM withdraw shape) (R50.5) |
| Device exchange | public Trezor Ethereum protocol (R50.8-R50.11) |
| Task states | ch. 49 withdraw status vocabulary + `EnterTrezorPin` / `EnterTrezorPassphrase` (R50.12-R50.14) |
| TRON / TRC20 | rejected as unsupported; never enters a signing user action (R50.20, mirrors R49.9 / T49.11) |
| Output | transaction-details payload byte-shape-identical to a software-signed withdrawal (R49.25, R50.22) |

The signing device is required to run the acceptance tests, so T50.* are
validated against a Trezor **emulator** harness (implementation/test-infra note,
§50.8).

**Implementation status (informative).** The signing architecture of this
chapter -- the `EthSigner::Trezor` seam variant, device-driven legacy EIP-155
signing over the public Trezor Ethereum protocol, byte-shape parity, the
withdrawal-task status progression, and the PIN/passphrase user-action flow
(R50.4-R50.23) -- is implemented and validated end-to-end against a Trezor
emulator (the `trezor-emulator-tests` acceptance suite covers T50.1, T50.2,
T50.4, T50.6, T50.7, T50.10, T50.11; T50.3 is subsumed by T50.2; T50.5 (PIN
round-trip) is an emulator-harness `#[ignore]` pending a DebugLink PIN-matrix
helper; T50.8/T50.9 device-availability faults need transport-level fault
injection). **Activation** under the Trezor hardware `priv_key_policy`
(R50.1-R50.4) is also implemented and emulator-validated: `EthActivationPolicy::Trezor`
activates an EVM platform coin through the `task::enable_eth` task variant,
deriving the coin's address and account public key from the device (interactive
connect / PIN / passphrase surfaced as the platform task's awaiting states,
realizing the ch. 48 R48.6.2 awaiting-user-action machinery). The whole Trezor
EVM path -- activate then withdraw -- is therefore reachable through the public
task RPC surface.

---

## 50.1 Activation binding: device-sourced address and key

R50.1. An EVM coin activated under the Trezor hardware `priv_key_policy`
(R35.1.4) shall obtain its address and account public key from the **connected
device**, not from any locally held seed or secret. The framework shall hold no
local private key for such a coin; this is the sibling property to the
external-wallet no-local-key model of §47.5 (R47.5.14). Activation shall bind the
coin to an HD wallet whose root identity is the connected device (its public-key
fingerprint), so every subsequent derivation resolves against the device's public
nodes.

R50.2. Trezor-policy EVM activation shall require an initialized hardware-wallet
context (a connected, unlocked device). Because establishing that context may
require interactive user actions (device connect, PIN, passphrase, public-key
confirmation), Trezor-policy activation shall run through the `task::enable_eth`
task variant (R35.3.2), not the one-shot `enable_eth_with_tokens` call. If no
hardware-wallet context is available at activation, activation shall fail with
the platform-activation error contract of R35.1.5 (a hardware-context-not-
initialized / unexpected-device-policy class discriminant), rather than falling
back to a local key.

R50.3. Trezor-policy EVM activation shall be gated on coin configuration
declaring hardware-wallet support for the coin (the published per-coin
hardware-support marker and the coin's BIP-44 coin-path). A coin whose
configuration does not declare hardware-wallet support shall fail activation with
the platform-activation error contract of R35.1.5 (a coin-does-not-support-
hardware-wallet class discriminant) and shall not attempt any device exchange.

R50.4. The enabled HD address determines the withdrawal sender and the signing
derivation path. Unless activation selects a different enabled address, the
default enabled HD address shall be the address-id tuple `account_id: 0`,
`chain: External`, `address_id: 0` (consistent with R49.7). When activation
selects a non-default enabled address (via the standard HD account/chain/address
selector), that selected address and its derivation path shall be honored as the
default withdrawal sender and signing path. A `task::withdraw::init` request that
supplies an explicit `from` selector (R49.8) shall override the enabled default
for that withdrawal, resolving `from` to an activated compatible EVM HD address
per R49.2; an unresolvable or foreign selector shall fail per R50.19.

> **Cross-reference.** R50.1-R50.4 refine R35.1.4 / R35.3.2 (policy + task) and
> R49.7 / R49.8 (sender selection) at the signing-architecture level. The
> no-local-key property parallels §47.5 (MetaMask), differing in that the key
> lives on a **local USB/WebUSB device** driven by the Trezor protocol rather
> than a browser wallet driven by EIP-1193.

---

## 50.2 Transaction type and fields presented to the device

R50.5. For an EVM withdrawal the framework shall build the **legacy EIP-155**
transaction shape (the transaction form the reloaded EVM withdraw path already
constructs and RLP-encodes) and shall ask the device to sign that legacy form.
The framework shall not require the device to sign an EIP-1559 typed transaction
for the reloaded withdraw path.

R50.6. The set of transaction fields handed to the device for a legacy EVM
signing request shall be exactly the fields that define the EIP-155 signing
payload:

- account derivation path (the enabled or selected signing path, R50.4);
- `nonce`;
- `gas_price`;
- `gas_limit`;
- recipient address (`to`) -- absent for a contract-creation action;
- `value`;
- transaction data / payload (`data`) -- empty for a plain native-coin send,
  or the ABI-encoded call for a token transfer (§50.3);
- `chain_id` (the coin's EIP-155 chain id).

Numeric fields shall be presented as big-endian unsigned byte sequences with
leading zero bytes trimmed, per the public Trezor Ethereum field encoding.

R50.7. EIP-155 chain-id handling: the `chain_id` shall be supplied to the device
in the signing request so the device applies EIP-155 replay protection when
computing the recovery value. On the return path the framework shall normalize
the device-returned recovery value back to the canonical recovery parameter and
re-apply EIP-155 replay protection when RLP-encoding the final signed
transaction with the same `chain_id`, so the encoded transaction matches the
EIP-155 form a software signer produces for that chain (R50.22). A device-returned
recovery value that indicates an invalid signature shall fail the withdrawal per
R50.18.

> **Upstream divergence (informative).** Upstream can also drive the device's
> EIP-1559 signing message, and explicitly rejects the EIP-2930 access-list
> transaction form for the device as unsupported. Because the reloaded EVM
> withdraw builds the legacy EIP-155 shape (R50.5), this chapter binds only the
> legacy signing contract; an EIP-1559 device-signing path, if later required,
> shall be specified as an extension and mapped onto the device's EIP-1559
> message with the analogous field set (nonce, max-fee, max-priority-fee,
> gas-limit, to, value, data, chain-id, access-list). EIP-2930 device signing is
> out of scope and shall be rejected as unsupported.

---

## 50.3 Public Trezor Ethereum device exchange

This section is a functional description of the **public** Trezor Ethereum wire
protocol as used by the signing architecture. It is not a transcription of the
project's device-driver code.

### 50.3.A Obtaining an address / public key for a derivation path

R50.8. To learn the account address for a derivation path the framework shall
issue the public `EthereumGetAddress` request (carrying the BIP-32 path and an
optional on-device display flag) and read the address from the returned
`EthereumAddress` response. This is used at activation (R50.1) and whenever the
enabled/selected address must be confirmed against the device.

R50.9. To learn the account extended public key for a derivation path the
framework shall obtain the device's secp256k1 public node for that path. The
public protocol exposes `EthereumGetPublicKey` / `EthereumPublicKey` for this
purpose; the extended public key so obtained yields the account public key and
address used to bind the coin (R50.1).

> **Upstream divergence (informative).** Some device firmware returns an
> unreliable result for the Ethereum-specific public-key request. Upstream
> therefore obtains the equivalent secp256k1 public node through the device's
> generic BIP-32 public-node request (Ethereum and Bitcoin share the `m/44'`
> BIP-44 purpose), yielding the same extended public key. Reloaded MAY use the
> same equivalent request to obtain the account public node; the **requirement**
> is that a correct secp256k1 extended public key for the derivation path is
> obtained from the device, not the specific message used to fetch it.

### 50.3.B Signing a transaction (with data-chunk streaming)

R50.10. To sign a legacy EVM transaction the framework shall issue the public
`EthereumSignTx` request carrying the fields of R50.6, including the total
payload length and the **initial** payload chunk (the public protocol transmits
at most the first 1024 bytes of `data` in this first message). The device
responds with an `EthereumTxRequest`.

R50.11. The `EthereumTxRequest` response is either a **request for more payload**
or the **final signature**, per the public protocol:

- If the response indicates a further payload length is needed, the framework
  shall answer with an `EthereumTxAck` carrying the next payload chunk (each
  chunk at most the protocol's per-message limit) and read the next
  `EthereumTxRequest`. This continues until the device has consumed the whole
  payload. This streaming path is exercised whenever the payload does not fit in
  a single message -- for example an ERC20 `transfer(address,uint256)` call (a
  4-byte selector plus two 32-byte arguments) or any larger contract call.
- When the device has the whole payload it returns the signature components
  (recovery value `v`, and `r`, `s`).

R50.12. On receiving the signature components the framework shall (a) normalize
the recovery value and re-apply EIP-155 replay protection (R50.7), (b) assemble
`(v, r, s)` with the original unsigned transaction fields into a signed
transaction, and (c) RLP-encode it to obtain the final signed-transaction bytes
(`tx_hex`) and its keccak-256 transaction hash (`tx_hash`). The assembled signed
transaction shall be identical in structure and encoding to one produced by the
local signer for the same unsigned transaction and chain id (R50.22).

> **Cross-reference.** R50.8-R50.12 describe the public Trezor Ethereum protocol
> functionally; the protocol is the source of truth (published Trezor firmware /
> protobuf definitions), not this project's driver.

---

## 50.4 Withdrawal-task status progression and user actions

R50.13. A Trezor EVM withdrawal driven through `task::withdraw::init` shall
surface progress using the withdrawal task's existing status vocabulary (R49.16,
R49.17), reusing the same in-progress status values already established for the
UTXO Trezor withdraw path. The observable in-progress progression shall cover, in
order: transaction preparation / generation; waiting for the device to connect;
waiting for the user to confirm the signing on the device; the signing step; and
finishing. The specific in-progress status wire values are those of the shared
withdraw status set (preparing / generating-transaction / waiting-for-Trezor-to-
connect / waiting-for-user-to-confirm-signing / signing-transaction / finishing);
clients shall treat any in-progress value as a progress indicator and continue
polling (R49.17).

R50.14. When the device requests a PIN or a passphrase, the task shall transition
to `status: "UserActionRequired"` (R49.18) carrying the matching awaiting-status
discriminant: a PIN request as `EnterTrezorPin` and a passphrase request as
`EnterTrezorPassphrase`. The client shall collect the requested input and submit
it through `task::withdraw::user_action` (R49.22) with the matching action
payload:

- `{"action_type": "TrezorPin", "pin": "<pin-matrix-response>"}`
- `{"action_type": "TrezorPassphrase", "passphrase": "<passphrase>"}`

The PIN value is the pin-matrix positional response defined by the public Trezor
protocol, not the literal PIN digits.

R50.15. Submitting a Trezor action that does not match the currently requested
action (a passphrase action while a PIN is awaited, or vice versa) shall fail with
the structured task-action error identifying the expected action type (R49.22 /
T49.22). A user action submitted while the task is not in `UserActionRequired`
shall fail with the structured task-action error (R49.23).

R50.16. Device connect and each awaiting-user-action state shall be bounded by a
timeout consistent with the shared hardware-wallet connect/interaction budget
already used by the UTXO Trezor path. A task that exceeds its budget shall reach a
terminal timeout-class withdrawal error (mapping to the request-timeout status).
The interactive states of R50.13 do not themselves require a `user_action` round
trip (they are informational "confirm on device" states); only the PIN and
passphrase requests of R50.14 require a `user_action` submission.

> **Cross-reference.** R50.13-R50.16 refine R49.17 / R49.18 / R49.22 / R49.23 for
> the EVM Trezor case and reuse the withdrawal task status vocabulary shared with
> the UTXO Trezor withdraw path. No new task-status or user-action wire values are
> introduced by this chapter.

---

## 50.5 Error and discriminant mapping

R50.17. Device availability and identity failures shall surface as terminal
structured withdrawal errors using the public withdrawal-error discriminants
already defined for hardware-wallet withdrawals. The bound condition-to-
discriminant mapping is:

| Condition | Public `error_type` discriminant | HTTP status class |
| --- | --- | --- |
| No device present / no hardware context | `NoTrezorDeviceAvailable` | 410 |
| Device disconnected mid-operation | `TrezorDisconnected` | 410 |
| Unexpected / foreign device (identity mismatch) | `FoundUnexpectedDevice` | 410 |
| Device-internal / device-reported failure, including a device-side user cancellation surfaced as a device failure | `HardwareWalletInternal` | 500 |
| Operation exceeded its time budget (R50.16) | `Timeout` | 408 |

The human-readable message text carried alongside the discriminant is not part of
the contract.

R50.18. A device-returned signature that is structurally invalid (for example an
invalid recovery value, R50.7) shall fail the withdrawal with the device-internal
class discriminant of R50.17 rather than emitting a malformed signed transaction.

R50.19. An unresolvable or foreign `from` HD-address selector for a Trezor EVM
withdrawal shall fail the task with the structured sender-selector withdrawal
errors already bound by ch. 49 (R49.2 / R49.8): a selector with a mismatched coin
path, unknown account, or non-activated address maps to the unexpected-`from` /
unknown-account discriminants (`UnexpectedFromAddress`, `UnknownAccount`), and an
absent-but-required sender maps to the no-sender discriminant
(`FromAddressNotFound`). Selector validation shall occur **before** any device
exchange, so a bad selector never triggers a device prompt.

R50.20. TRON-family native coins and TRC20-like tokens are unsupported under the
Trezor signing policy (mirroring R49.9). A `task::withdraw::init` request for a
TRON-family coin activated under a Trezor policy shall fail with a **structured
unsupported withdrawal error** and shall **not** enter any Trezor signing user-
action flow -- no device connect, no PIN or passphrase prompt, no
`EthereumSignTx` exchange. The rejection shall occur at the earliest practical
point (before device interaction), consistent with the early-clean-rejection
principle of §47.5 (R47.5.13a).

> **Upstream divergence (informative).** Upstream surfaces the TRON-under-Trezor
> case as an unsupported-operation withdrawal error and does not route it into the
> device flow. Reloaded binds the same behaviour and requires the rejection to be
> raised before any device exchange. The precise discriminant is the reloaded
> unsupported-withdrawal discriminant; only the **condition** (structured
> unsupported error, no user action) is bound here.

R50.21. All error surfacing shall be structured. Under the withdrawal **task**
path a terminal failure is reported as `status: "Error"` with the serialized
withdrawal error in `details` (R49.20); the RPC call itself remains successful
(R49.15). Clients classify failure from `result.status == "Error"` and
`result.details.error_type`.

---

## 50.6 Interop constraints (byte / wire compatibility)

R50.22. A Trezor-signed EVM withdrawal transaction shall be **byte-for-byte
indistinguishable** from a software-signed one for the same unsigned transaction
and chain id. Specifically:

- the signed-transaction RLP encoding (`tx_hex`) shall be the standard EIP-155
  9-field legacy encoding, identical to the local-signer output for the same
  fields, with the same EIP-155 replay-protected recovery value (R50.7);
- the transaction hash (`tx_hash`) shall be the keccak-256 of that encoding;
- the completed transaction-details object shall use the same shape and field set
  as any other completed withdrawal (R49.25): at least `tx_hex`, `tx_hash`,
  `from`, `to`, `total_amount`, `spent_by_me`, `received_by_me`,
  `my_balance_change`, `block_height`, `timestamp`, `fee_details`, `coin`,
  `internal_id`, and `transaction_type`.

R50.23. The `from` field shall report the enabled or selected sender address
(R50.4); balance checks and nonce selection shall use that same sender address, so
the account the device signs for is the account whose balance and nonce were
validated. No Trezor-specific field shall be added to the withdraw request or to
the completed transaction-details payload; the withdraw request wire stays exactly
the ch. 49 shape.

> **Cross-reference.** R50.22 / R50.23 make the signing-policy choice invisible on
> the wire: the completed payload contract of R49.25 and the request contract of
> R49.1 / R49.2 are unchanged. This is the same "policy is invisible on the
> completed-payload wire" property that ch. 49 requires across signing policies
> (T49.25).

---

## 50.7 Signing-seam placement (informative)

R50.24 (placement, informative). The `Trezor` signing behaviour shall be added as
a variant of the existing EVM signing-policy seam introduced in §47.5 (the seam
that already carries `Local` and, on WASM, `Metamask`). The seam's send-time and
offline-signing entrypoints shall route the `Trezor` variant to the device-driven
signing flow of §50.3 for the withdrawal path, and shall reject `Trezor` for the
swap / HTLC send and detached-offline-signature entrypoints (the same
unavailable-key-derivation gating §47.5 applies to non-local-key policies,
R47.5.13 / R47.5.13a) -- a hardware device driven only through the interactive
withdrawal task cannot satisfy the swap protocol's framework-scheduled,
non-interactive HTLC signing and detached-refund pre-signing requirements. This is
a placement/seam note; the internal decomposition is not bound.

> **Upstream divergence (informative).** Whether atomic swaps are offered for an
> EVM coin under the Trezor policy is treated here exactly as for MetaMask
> (§47.5, R47.5.12-R47.5.13): **UNSUPPORTED**, gated early and cleanly, because
> the interactive device-signing model cannot produce the pre-signed,
> framework-scheduled and detached refund/spend signatures the swap protocol
> requires. If the published API later defines a hardware-wallet swap flow, the
> docs govern and this note shall be revisited.

---

## 50.8 Acceptance tests

The Trezor signing tests require a signing device; they are validated against a
Trezor **emulator** harness driven through the same task RPCs a real device
would use (implementation / test-infra note). Each test below aligns with the
ch. 49 EVM Trezor acceptance tests it references.

T50.1 (aligns with T49.8). Activate an EVM native coin under the Trezor policy
via the `task::enable_eth` task variant and start a native-coin withdrawal task
without `from`. The coin's address and account public key shall come from the
device (no local key material); the task shall sign with the enabled default HD
address (`account_id: 0`, `chain: External`, `address_id: 0`) and derivation
path; the successful terminal `Ok` payload shall carry a transaction-details
object whose `from` is that enabled address and whose `tx_hex` and `tx_hash` are
present. (R50.1, R50.4, R50.5, T49.8)

T50.2 (aligns with T49.9). Start an ERC20-like fungible-token Trezor withdrawal
task with a valid `from` **derivation-path** selector for a non-default activated
HD address, so the payload is a `transfer(address,uint256)` ABI call. The task
shall sign with the selected derivation path via the public `EthereumSignTx`
exchange (streaming the payload in follow-up `EthereumTxAck` chunks if it exceeds
one message); the terminal payload's `from` shall be the selected address and
`tx_hex` / `tx_hash` shall be present. (R50.6, R50.10, R50.11, R50.12, T49.9)

T50.3 (aligns with T49.10). Start a Trezor withdrawal with a valid **address-id**
selector that resolves to the same sender as a derivation-path selector. Both
selectors shall yield the same sender address and the same transaction-details
field contract. (R50.4, R50.22, T49.10)

T50.4. Assert byte-shape parity: for the same unsigned EVM transaction and chain
id, a Trezor-signed withdrawal's `tx_hex` (EIP-155 legacy 9-field RLP, with
EIP-155 replay protection re-applied) and `tx_hash` are identical to the local
signer's output, and the completed transaction-details field set matches R49.25.
(R50.7, R50.22, R50.23, T49.25)

T50.5 (aligns with T49.20). Drive a Trezor EVM withdrawal task to
`UserActionRequired` with a **PIN** request (`EnterTrezorPin`), submit
`{"action_type": "TrezorPin", "pin": "<pin-matrix-response>"}` via
`task::withdraw::user_action`, and continue polling. The task shall leave the
awaiting state and either progress or terminate. (R50.14, T49.20)

T50.6 (aligns with T49.21). Drive a Trezor EVM withdrawal task to
`UserActionRequired` with a **passphrase** request (`EnterTrezorPassphrase`),
submit `{"action_type": "TrezorPassphrase", "passphrase": "<passphrase>"}`, and
continue polling. The task shall leave the awaiting state and either progress or
terminate. (R50.14, T49.21)

T50.7 (aligns with T49.22). Submit a passphrase action while the task awaits a
PIN, or a PIN action while it awaits a passphrase. The call shall fail with the
structured task-action error identifying the expected action type. (R50.15,
T49.22)

T50.8. Start a Trezor EVM withdrawal with no device connected / no hardware
context. The task shall reach a terminal `status: "Error"` whose
`details.error_type` is the no-device discriminant (`NoTrezorDeviceAvailable`),
and no device prompt shall be issued. (R50.17)

T50.9. Present a foreign / unexpected device (identity mismatch) for a Trezor EVM
withdrawal. The task shall reach a terminal error whose discriminant is the
unexpected-device discriminant (`FoundUnexpectedDevice`). Disconnect the device
mid-operation in a separate run and observe the disconnected discriminant
(`TrezorDisconnected`); cause a device-side cancellation and observe the
device-internal discriminant (`HardwareWalletInternal`). (R50.17, R50.18)

T50.10. Start a Trezor EVM withdrawal with an unresolvable or foreign `from`
selector (mismatched coin path, unknown account, or non-activated address). The
task shall fail with the ch. 49 sender-selector discriminant
(`UnexpectedFromAddress` / `UnknownAccount` / `FromAddressNotFound`) **before any
device exchange** -- no device prompt shall be issued. (R50.19)

T50.11 (aligns with T49.11). Start a TRON-family native or TRC20-like token
withdrawal task under a Trezor signing policy. The task shall reach a terminal
structured **unsupported** withdrawal error and shall **not** require or issue a
Trezor PIN, passphrase, or any device-signing exchange. (R50.20, T49.11)

---

## 50.9 Open questions

O-1. Does the published KDF API documentation require an EIP-1559 device-signing
path for EVM Trezor withdrawals, or is the legacy EIP-155 form (R50.5) sufficient?
The reloaded EVM withdraw currently builds legacy; if the docs mandate EIP-1559
for the withdraw path, R50.5-R50.7 shall be extended to the device's EIP-1559
message with the analogous field set.

O-2. Does the published API define an atomic-swap flow for EVM coins under a
hardware-wallet policy? This chapter records the verdict as **UNSUPPORTED**
(R50.24, mirroring §47.5), distilled from current behaviour and the structural
impossibility of framework-scheduled non-interactive HTLC signing on an
interactive device. If the docs define such a flow, they govern.

O-3. Are external Ethereum network/token on-device definition blobs (the optional
network/token descriptors the public protocol lets a host pass so the device can
display chain/token names) required for the reloaded target chains, or optional?
They are an optional display aid in the public protocol; if a target chain
requires one for the device to sign, that is a per-chain configuration concern
outside the signing-behaviour contract bound here.
