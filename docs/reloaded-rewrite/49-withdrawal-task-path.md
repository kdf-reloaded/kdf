# 49. Withdrawal task path

**Status:** driving-spec.

## 49.1 Scope

This chapter covers the withdrawal JSON-RPC surface for the direct `withdraw`
method and the task family under `task::withdraw::*`.

The direct `withdraw` method returns a completed transaction payload when the
operation completes in the request/response round trip. The task family uses the
standard mmrpc 2.0 task pattern: `init` returns a `task_id`, `status` returns a
task-status object, `user_action` supplies hardware-wallet input when requested,
and `cancel` aborts an in-flight task.

## 49.2 Init request and sender selection

R49.1. `task::withdraw::init` shall accept the canonical withdrawal request
object used by the direct `withdraw` method. The common request fields are:

- `coin` (string, required) -- source coin ticker.
- `to` (string, required) -- destination address.
- `from` (object, optional) -- wallet-family sender selector; see R49.2.
- `amount` -- requested withdrawal amount when `max` is false.
- `max` (boolean, optional, default false) -- request a full spendable-balance
  withdrawal.
- `fee` (object, optional) -- coin-family fee descriptor.

Coin families may bind additional public fields already accepted by the shared
withdrawal request, such as memo, broadcast, protocol-channel, or expiry
controls. The task endpoint shall not add a second withdrawal-specific request
shape.

> **Fee-rail selector (pointer, 2026-11-08).** The optional `fee_method`
> (`native` | `gasless` | `auto`) and `gasless` fields of the shared request,
> the gasless fee-details shape, the gasless task progress phases and the
> nested gasless error family under the shared withdraw error are bound by
> [Chapter 63](63-tron-gasfree-fee-delegation.md) §63.3.4–§63.3.6 and §63.9.
> They are inert for every non-Tron coin. Chapter 63 passed the KDF Dirty Gate
> on 2026-11-08.

> **The `fee` descriptor's EVM forms (cross-reference).** The descriptor is a
> single enumeration shared by every coin family, tagged by a `type` member.
> Its EVM forms, the rule for an absent descriptor, the gas-limit estimation
> that follows, and the envelope type the resolved form implies are bound by
> [Chapter 57](57-evm-fee-inputs.md) §57.7, which also records that reloaded
> accepts only the legacy EVM form today and that the enumeration is missing
> the Cosmos-family form entirely. This chapter binds the request's shape and
> the task lifecycle around it, not the per-family fee semantics. The two
> Cosmos-family forms -- the explicit one of the released reference and the
> priority one of an unmerged reference draft -- are bound in §49.2A (gated
> text; passed the KDF Dirty Gate on 2026-11-08), with their arithmetic in
> [Chapter 18](18-tendermint-ibc-htlc.md) §18.6B.
>
> *As built 2026-11-09 (clean-side note): implemented; see the As-built note of §49.2A.*

R49.2. When `from` is present, it shall be an HD-address selector in exactly one
of these public JSON wire forms:

- Address-id form:

```json
{
  "account_id": 0,
  "chain": "External",
  "address_id": 0
}
```

- Derivation-path form:

```json
{
  "derivation_path": "m/44'/0'/0'/0/0"
}
```

The address-id form uses `account_id` and `address_id` as unsigned integers.
The `chain` value shall be the public BIP-44 chain discriminant `External` or
`Internal`. The derivation-path form shall identify a full standard HD address
path for the activated coin. A selector with a mismatched coin type, mismatched
coin path, unknown account, unsupported chain, or address that is not activated
for withdrawal shall fail the withdrawal with a structured withdrawal error.

R49.3. For Iguana or other legacy single-key wallet activations, `from` shall be
omitted. If a single-key wallet receives an explicit `from` selector, the
withdrawal shall fail with a structured withdrawal error whose public
discriminant identifies an unexpected sender selector. The server shall not
silently ignore an explicit `from` on a single-key wallet.

R49.4. For HD UTXO-family withdrawals, on `task::withdraw::init` and on the
direct `withdraw` method alike, `from` selects the sender. If `from` is
present, the implementation shall resolve it to the selected activated HD
address and use that address as the only withdrawal sender. If `from` is
omitted, the withdrawal shall use the coin's activated address as the only
withdrawal sender. That is the address at the activation's `path_to_address`,
by default account `0`, chain `External`, address `0`, the same address the
coin's swaps use (chapter 05 §5.12C R42). Under the hardware-wallet policy,
which does not honor `path_to_address` (chapter 55 §55.5), the default stays
account `0`, chain `External`, address `0`. *(Amended 2026-11-04 and
2026-11-06; see the divergence note.)*

> **Reloaded divergence (2026-11-04).** Until this amendment R49.4 required an
> omitted `from` to fail with a structured error saying that no sender address
> was supplied. Reloaded has used the enabled address instead since 0.1.x, so
> that a wallet client whose withdraw preview omits `from` works. The divergence
> from GLEEC KDF, which rejects an omitted `from`, is recorded in
> [`RELOADED_VS_GLEEC.md`](../../RELOADED_VS_GLEEC.md). The maintainer kept it
> on 2026-11-04 (decision BY in plan
> [`hd-key-unification`](../plans/hd-key-unification.md)); no switch is
> planned. On 2026-11-06 the maintainer set the default sender to the coin's
> activated address, the address at `path_to_address` (decision CU). Before,
> it was always account `0`, chain `External`, address `0`; the two differ
> only for a non-default `path_to_address`. The direct `withdraw` method
> serves an HD UTXO wallet in the same way: with `from` as at the reference,
> and without `from` like the task route (decision BZ, plan item H5).

> **Reference behavior (passed the KDF Dirty Gate on 2026-11-04, measure
> 0.01; Spec Reader 2026-11-04, rows `v2.6.0-beta` and `v3.0.0-beta` of
> chapter 02 §2.15A, identical at both).** The reference fails an HD
> UTXO-family withdrawal that omits `from` with the structured error whose
> discriminant says the sender address was not found (`FromAddressNotFound`,
> R49.27a), on the task route **and** on the direct `withdraw` method, which
> share one sender-resolution step; with `from` present, both routes sign
> with the key derived at the selected address's path and return change to
> that address. **Compatibility (this repository; not corpus-sourced):**
> when `from` is absent, Reloaded uses the coin's activated address on both
> routes, where the reference fails. This is a documented divergence,
> recorded in `RELOADED_VS_GLEEC.md`, in the 0.1.x interoperability note for
> the Komodo DeFi wallet, and in the CHANGELOG. R49.4 states that default
> since 2026-11-04 (plan [`hd-key-unification`](../plans/hd-key-unification.md)
> item H0). The maintainer kept it as a divergence on that date (decision BY)
> and set the address on 2026-11-06 (decision CU). Plan item H5 serves the
> direct method with `from`, as the reference does, and without `from` the
> same way as the task route (decision BZ).

> **As built (2026-11-07, clean-side record).** Plan item H5.1.
> `MmCoin::withdraw` of the UTXO standard coin and QTUM calls
> `utxo_common::withdraw_with_hd_sender`. Under a software HD key policy with
> an HD derivation method it builds
> `StandardUtxoWithdraw::new_with_hd_sender`. Every other coin keeps the
> single-key `StandardUtxoWithdraw::new`, including a coin under the Trezor
> policy (which answers as before, with the internal error that the Iguana key
> is unavailable, and uses no device) and BCH. The sender comes from
> `resolve_withdraw_sender`, which both this route and `InitUtxoWithdraw::new`
> call. It calls `get_withdraw_sender_address` → `get_withdraw_hd_sender`, the
> single place that decides `from` and the default. The key that signs is
> derived at the sender's path by `hd_sender_key_pair`, which the task route's
> HD signing arm also uses, and the change returns to the sender. Without
> `from`, under a software HD policy, the sender is `enabled_hd_address_info`,
> the address at `path_to_address` with its full path; if its account is not
> held, the answer is `FromAddressNotFound`. Under the Trezor policy it stays
> account `0`, chain `External`, address `0`. BCH has no HD policy in this
> repository (its activation always builds the Iguana key), so it is not
> served under HD here (plan item H7). Tests, in module
> `utxo_tests::hd_coin_swap_key_tests::direct_withdraw`:
> - T49.3 and T49.4:
>   `t49_3_direct_address_id_selector_spends_from_that_activated_address`,
>   `t49_4_direct_derivation_path_selector_resolves_to_the_same_sender`.
> - T49.5: `t49_5_direct_without_from_default_path_spends_from_address_zero`,
>   `t49_5_direct_without_from_non_default_path_spends_from_that_address`,
>   `t49_5_the_shared_sender_step_without_from_is_the_activated_address`,
>   `t49_5_trezor_policy_default_is_unchanged_and_the_direct_route_does_not_sign_with_a_device`,
>   `t49_5_direct_without_from_and_a_missing_activated_account_answers_from_address_not_found`.
> - T49.6: `t49_6_direct_unknown_account_answers_unknown_account`,
>   `t49_6_direct_address_not_activated_answers_unexpected_from_address`,
>   `t49_6_direct_mismatched_coin_type_answers_unexpected_from_address`.
> - QTUM: `qtum_direct_withdraw_with_from_spends_from_the_selected_address`.

R49.5. EVM-family software-HD native-coin and fungible-token withdrawals shall
be supported through both direct `withdraw` and `task::withdraw::init`. When
`from` is omitted, both paths shall use the activated software key/address for
balance checks, nonce selection, signing, and the completed transaction's
sender. When `from` is present, both paths shall resolve it as an HD address
selector and use the selected activated HD address and derived key instead.

R49.6. EVM-family Trezor/hardware-wallet native-coin and ERC20-like fungible
token withdrawals shall be supported through `task::withdraw::init`. The direct
legacy `withdraw` method is not required to support Trezor user-action signing;
clients that need Trezor PIN, passphrase, or device-confirmation handling shall
use the task path.

R49.7. For an EVM-family Trezor/hardware-wallet task withdrawal with `from`
omitted, the task shall use the enabled HD address and derivation path already
associated with the activated wallet. That address shall be used as the sender
for balance checks, nonce selection, hardware-wallet signing, and the completed
transaction's `from` field. Unless activation has selected a different enabled
address, the default enabled HD address is the address-id tuple `account_id: 0`,
`chain: External`, `address_id: 0`.

R49.8. For an EVM-family Trezor/hardware-wallet task withdrawal with `from`
present, the task shall resolve `from` as an HD address selector according to
R49.2. The selected activated HD address shall be used as the sender for
balance checks and nonce selection, and the selected derivation path shall be
used for hardware-wallet signing. The completed transaction-details object
shall report the selected sender in `from`. A selector that cannot be resolved
to an activated compatible EVM HD address shall fail the task with a structured
withdrawal error.

R49.9. TRON-family withdrawals are out of scope for the EVM Trezor task-signing
path. If a TRON-family coin or TRC20-like token is activated with a
Trezor/hardware-wallet signing policy and submitted through
`task::withdraw::init`, the task shall fail with a structured unsupported
withdrawal error and shall not enter a Trezor signing user-action flow.

Implementation status. The EVM Trezor task-withdraw signing path specified by
R49.6 through R49.9 and T49.8 through T49.11, and the PIN/passphrase user-action
contract of R49.18/R49.22 (T49.20 through T49.22), are implemented in reloaded
and validated end-to-end against a Trezor emulator (see chapter 50 and the
`trezor-emulator-tests` acceptance suite). Software-HD EVM withdrawal support is
also implemented. EVM coin *activation* under the Trezor hardware
`priv_key_policy` (obtaining a Trezor-policy EVM coin via `task::enable_eth`,
ch. 35 R35.1.4 / R35.3.2, ch. 50 R50.1-R50.4) is also implemented and
emulator-validated, so the Trezor EVM withdraw path is now reachable through the
public activation RPC.

R49.10. `task::withdraw::init` shall return only the standard task-init response:

- `task_id` (integer) -- identifier for subsequent `status`, `user_action`, and
  `cancel` calls.

No transaction payload shall be returned by `init`.

T49.1. Start a single-key withdrawal task without `from`. The init call shall
return `task_id`, and the task shall proceed to a status result other than a
sender-selector validation failure.

T49.2. Start a single-key withdrawal task with either accepted `from` wire form.
The task shall reject the request with a structured withdrawal error indicating
an unexpected sender selector.

T49.3. Start an HD UTXO-family withdrawal task with an address-id selector for
an activated external address. The task shall use that address as the sender.

T49.4. Start an HD UTXO-family withdrawal task with a derivation-path selector
that resolves to the same activated address as T49.3. The task shall use the
same sender address.

T49.5. Start an HD UTXO-family withdrawal without `from`, on
`task::withdraw::init` and on the direct `withdraw` method. Each shall use the
coin's activated address (R49.4) as the sender:
- account `0`, chain `External`, address `0` for a coin activated with the
  default `path_to_address`;
- for a coin activated with a non-default `path_to_address` (for example
  account `0`, chain `External`, address `1`), that address and not address
  `0`.

*(Amended 2026-11-04 and 2026-11-06, R49.4.)* This tests the documented
divergence of R49.4 (decisions BY, BZ and CU). The reference fails the same
request (the reference-behavior note under R49.4).

T49.6. Start an HD withdrawal task with a selector for an unknown account, a
mismatched coin path, or an address that has not been activated. The task shall
reach a terminal error status whose public withdrawal-error discriminant
identifies an invalid or unexpected sender selector.

T49.7. Start EVM-family software-HD withdrawals without `from` through both the
direct `withdraw` method and `task::withdraw::init`. Each withdrawal shall use
the activated software key/address as the sender. Repeat the task withdrawal
with a valid `from` selector for a different activated HD address; the task
shall use the selected address.

T49.8. Start an EVM-family Trezor/hardware-wallet native-coin withdrawal task
without `from`. The init call shall return `task_id`. The task shall sign with
the enabled HD address and derivation path associated with the activated wallet,
and successful terminal status shall return a transaction-details object whose
`from` contains that enabled address and whose `tx_hex` and `tx_hash` are
present.

T49.9. Start an EVM-family Trezor/hardware-wallet ERC20-like fungible-token
withdrawal task with a valid `from` derivation-path selector for a non-default
activated HD address. The task shall sign with the selected derivation path, and
successful terminal status shall return a transaction-details object whose
`from` contains the selected address and whose `tx_hex` and `tx_hash` are
present.

T49.10. Start an EVM-family Trezor/hardware-wallet withdrawal task with a valid
address-id selector that resolves to the same sender as a derivation-path
selector. Both selectors shall result in the same sender address and successful
transaction-details field contract.

T49.11. Start a TRON-family native or TRC20-like token withdrawal task under a
Trezor/hardware-wallet signing policy. The task shall reach a terminal
structured unsupported withdrawal error and shall not require a Trezor PIN or
passphrase user action.

### 49.2A Cosmos-family fee descriptors (gated text; passed the KDF Dirty Gate on 2026-11-08)

> **Provenance status: passed the KDF Dirty Gate on 2026-11-08, measure 0.03**
> (chapter 01 R40; attestation in the Provenance Footer). This subsection was
> written by the dirty-side specification author on 2026-11-08 (Spec Reader
> round 0.4.0-11) from the reference state `tendermint-priority-fee (as of
> 2025-07-24)` compared against its base `dev (as of 2025-07-18)`, with the
> released rows `v2.6.0-beta` and `v3.0.0-beta` read for the explicit form
> only (all rows of chapter 02 §2.15A), and from this repository's own tree.
> R49.28 and R49.30 describe behavior **both released designations** have;
> R49.29 describes the unmerged, never-approved draft only. The maintainer
> decided on 2026-11-08 (decision FN (a)) to include the feature; the names
> are open under chapter 36 D36.4. Rules continue this chapter's sequence
> (R49.28 onward) although they sit before §49.3.

R49.28. *Explicit Cosmos form.* For a Tendermint platform coin and a
Tendermint token, the direct `withdraw` method, `task::withdraw::init`, the
staking requests of [Chapter 18](18-tendermint-ibc-htlc.md) §18.6A
(`add_delegation`, `remove_delegation`, `claim_staking_rewards` and their
namespaced aliases, through their optional `fee` member) and a withdraw that
is routed as an IBC transfer (chapter 18 §18.4) shall accept the fee
descriptor

```json
{"type": "CosmosGas", "gas_limit": 125000, "gas_price": 0.03}
```

where `gas_limit` is an unsigned integer that becomes the signed
transaction's gas limit (chapter 18 R18.6B.3) and `gas_price` is a number in
the platform's base denom per gas unit that becomes the selected gas price
(chapter 18 R18.6B.1 item 1). The fee **amount** is still simulation-derived
(chapter 18 R18.6B.2): `gas_limit × gas_price` is **not** the amount, unlike
the EVM `EthGas` form of [Chapter 57](57-evm-fee-inputs.md) R57.7.1. A token
withdraw pays this fee in the platform denom (chapter 18 R18.11.11). Both
members are required; a descriptor missing one fails request parsing like
any malformed `fee` object (the request-parse failure of
[Chapter 55](55-coin-activation-wire-contract.md) §55.1). The tag spelling is
`CosmosGas` exactly (the same serialization rule as chapter 57 R57.7.1's
note). This form exists at both released designations.

R49.29. *Priority Cosmos form (draft row only).* The same requests shall
accept the descriptor

```json
{"type": "CosmosGasPriority", "gas_limit": 125000, "gas_price_option": "High"}
```

where `gas_limit` is as in R49.28 and `gas_price_option` is one of exactly
four case-sensitive strings — `Low`, `Average`, `High`, `Default` — naming
a level of the coin's `priority_gas_prices` ([Chapter 36](36-tendermint-v2-activation-rpcs.md)
R36.3.5): the first three select positions 0, 1 and 2 of the array;
`Default`, and every level on a coin whose `protocol_data` has no array,
selects the coin's default gas price (chapter 18 R18.6B.1 item 2). A token
withdraw selects from its platform coin's array (chapter 36 R36.3.6). Any
other `gas_price_option` string, or a missing member, fails request parsing
as in R49.28. No level is ever an error: an unhonorable level is the
default price. The tag and member spellings are those of the reference
draft, kept under chapter 36 D36.4 option (a) and to be renamed
consistently with chapter 18 §18.6B and chapter 35 R35.6.8 if (b) or (c) is
chosen.

R49.30. *A foreign fee form on a Cosmos request.* At both released
designations and at the draft row, a `fee` descriptor of a **non-Cosmos**
type that parses (`UtxoFixed`, `UtxoPerKbyte`, `EthGas`, `EthGasEip1559`,
`Qrc20Gas`) on a Tendermint withdraw, staking or IBC request is **ignored
without error**: the default gas price and the operation's default gas
limit are used as if no descriptor had been sent. Reloaded shall **not**
copy this: such a descriptor shall be refused with the structured
withdrawal error of R49.20, discriminant `InvalidFeePolicy`, HTTP 400 (the
arm R49.27a already lists for a fee descriptor of a type the coin does not
take, [Chapter 38](38-utxo-coin-maintenance-and-features.md)
R38.6.6l/R38.6.6m), on the direct method and in `details` on the task
route, and the staking requests shall answer their own structured error
with the same meaning. *(Reloaded divergence; D49.1.)*

*Reloaded status (2026-11-09):* by the maintainer's decision of 2026-11-09
(D49.1, option (b)) such a descriptor is ignored without error, for parity,
on the direct method, on the task route and on the staking requests: the
default gas price and the operation's default gas limit apply. The refusal
above records the state before that decision.

R49.31. *What the completed payload shows.* For a Tendermint withdraw the
`fee_details` member of the transaction-details object (R49.25) carries the
fee `amount` as a decimal string in the platform coin's display precision —
the integer of chapter 18 R18.6B.2 scaled by the platform's `decimals` —
the platform `coin` ticker, and the `gas_limit` the transaction was signed
with. T18.6B.1 of chapter 18 asserts the amount through this member.

> **Reloaded status (informative; this repository's tree, 2026-11-08).** The
> shared fee descriptor has neither Cosmos form (chapter 57 §57.7 finding),
> so a Tendermint withdraw, staking or IBC request carrying either fails
> **request parsing** today, and the Tendermint fee computation ignores the
> descriptor it is handed (a code comment in this repository's Tendermint
> helpers records the missing form). Adding both forms, the refusal of
> R49.30 and the level selection of chapter 18 §18.6B closes the gap. The
> `fee_details` shape of R49.31 already matches.
>
> *As built 2026-11-09 (clean-side note): implemented; the status note after R49.30 records the decided
> behavior (ignore, D49.1 option (b)); see the As-built note of §49.2A.*

**D49.1 (maintainer decision; resolved).** *Resolved: the maintainer's decision of 2026-11-09, option (b), ignore for parity (R49.30); to be reassessed after field use.* *Foreign fee form on a Cosmos
request.* R49.30 binds a refusal where every reference state silently
ignores the descriptor. Options: (a) refuse with `InvalidFeePolicy` as
written (recommended: the UTXO and EVM families already refuse, R49.27a,
and a caller who sent an EVM descriptor to a Cosmos coin has made a
mistake worth reporting); (b) ignore as the reference does, for parity.
No peer observes the outcome, so no compatibility switch is engaged
either way (`docs/COMPAT_SWITCHES.md`, default-side rule arm 1).

T49.28. *Explicit Cosmos form (R49.28, R49.31).* Against a mocked backend
whose simulate query reports a fixed `gas_used`: a Tendermint platform-coin
withdraw with the `CosmosGas` descriptor, on the direct method and on
`task::withdraw::init`, signs a transaction whose gas limit equals the
descriptor's `gas_limit` and whose `fee_details.amount` equals the chapter
18 R18.6B.2 figure for the descriptor's `gas_price`; the same descriptor
with a different `gas_limit` yields the same `amount`; a Tendermint token
withdraw with the descriptor pays the fee in the platform denom; a
descriptor missing `gas_price` or `gas_limit` fails request parsing on both
routes; `add_delegation` with the descriptor signs with the descriptor's
gas limit.

T49.29. *Priority Cosmos form (R49.29).* On a platform coin with
`priority_gas_prices` the four `gas_price_option` values select the three
configured figures and the default price respectively (amounts as in
chapter 18 T18.6B.1); on a platform coin without the array every value
selects the default price and no error is raised; on a token the platform's
array is used; `gas_price_option: "high"` (lower case) and
`gas_price_option: "Medium"` each fail request parsing on both routes.

T49.30. *Foreign form refused (R49.30; Reloaded-original).* A Tendermint
withdraw carrying `{"type": "EthGas", "gas_price": 1, "gas": 21000}` answers
HTTP 400 with `error_type: "InvalidFeePolicy"` on the direct method and
`details.error_type == "InvalidFeePolicy"` on the task route, and no
transaction is simulated or broadcast; the same with a `UtxoFixed`
descriptor; `add_delegation` with an `EthGas` descriptor answers its
structured error and broadcasts nothing.

*Reloaded status (2026-11-09):* by the maintainer's decision of 2026-11-09
(D49.1, option (b)) the test is built as: the same withdraw with an `EthGas`
descriptor succeeds on the direct method and on the task route, signed with
the coin's default gas price and the operation's default gas limit, exactly
as the same withdraw without a descriptor; the same with a `UtxoFixed`
descriptor; a delegation fee computed with an `EthGas` descriptor uses the
delegation's default gas limit. The sentence above records the state before
that decision.

> **As built (2026-11-09; R49.28 -- R49.31, T49.28 -- T49.30).** The shared fee
> descriptor has two more variants: `CosmosGas {gas_limit: u64, gas_price: f64}` and
> `CosmosGasPriority {gas_limit: u64, gas_price_option}`, with the tags spelled exactly so and the four
> case-sensitive level strings `Low`, `Average`, `High`, `Default`; a missing member or another string fails
> request parsing. The Tendermint withdraw (coin and token), the staking requests and the task route (which calls
> the same `withdraw`) read them through the fee-intent function of chapter 18 §18.6B; any other descriptor on a
> Tendermint request is ignored without error (R49.30, D49.1 option (b)). The UTXO, SLP, EVM, QRC20 and Siacoin
> withdraw paths refuse both forms through their existing wrong-fee-type arm (`InvalidFeePolicy`; the NFT
> withdraw's `InvalidRequest`); the shielded coin refuses any `fee` as before. **Not changed:** the TRON
> and Solana (SOL, SPL) withdraws never inspect `fee`. **Note:** `EthGasEip1559`, named in R49.30, is not a variant of this
> repository's descriptor, so such a request fails parsing before it reaches any coin. `gas_price` is read as a
> JSON number (`f64`), so a numeric string fails parsing.
> Test coverage: the staking requests, the task route and the swap sends are covered through the fee computation
> they share, not end to end (`docs/UNIT_TESTS.md`).

*2026-11-10 (clean-side note): a TRON withdraw now refuses any `fee` descriptor with `InvalidFeePolicy`
(ch.21 R-WP6); Solana still does not read it.*

## 49.3 Status request and retention

R49.11. `task::withdraw::status` shall accept:

- `task_id` (integer, required) -- task identifier returned by R49.10.
- `forget_if_finished` (boolean, optional, default true) -- whether a terminal
  task shall be removed after this status read.

R49.12. When `forget_if_finished` is omitted, it shall behave as `true`. If the
requested task is terminal and `forget_if_finished` is true, the status call
shall return the terminal status once and remove the task from the task
registry. A later status call for the same `task_id` shall fail as an unknown
task.

R49.13. If `forget_if_finished` is false, a terminal status read shall not remove
the task. Repeated status calls for the same `task_id` shall continue to return
the same terminal task-status object until the task is forgotten by another
terminal read with `forget_if_finished` true or by implementation-defined
registry cleanup outside the compatibility contract.

R49.14. A status call for a task that is cancelling or no longer registered
shall fail as an unknown task. Cancellation is not represented as a public
withdraw task-status variant.

T49.12. Poll an in-progress withdrawal without `forget_if_finished`. The request
shall be accepted, and omission shall be equivalent to `forget_if_finished:
true` for any later terminal read.

T49.13. Poll a successful terminal withdrawal without `forget_if_finished`. The
first call shall return the terminal success status, and the next call for the
same `task_id` shall fail as an unknown task.

T49.14. Poll a terminal withdrawal with `forget_if_finished: false` twice. Both
calls shall return the same terminal status and shall not forget the task.

T49.15. Poll a terminal withdrawal with `forget_if_finished: false`, then poll
with `forget_if_finished: true`, then poll again. The second poll shall return
the terminal status and forget the task; the third poll shall fail as an
unknown task.

## 49.4 Status response wire shape

R49.15. `task::withdraw::status` shall return the standard mmrpc 2.0 response
envelope. For any registered task status, including terminal task failure, the
RPC call itself is successful and the mmrpc response contains a top-level
`result` member. Transport, decode, authorization, or unknown-task failures are
RPC errors and are not task-status objects.

R49.16. The `result` member of a successful status-RPC call shall be a
task-status object serialized with:

- `status` -- one of `InProgress`, `UserActionRequired`, `Ok`, or `Error`.
- `details` -- the payload for that status.

R49.17. `status: "InProgress"` shall carry a withdrawal progress value in
`details`. Clients shall treat the value as a progress indicator only and shall
continue polling.

R49.18. `status: "UserActionRequired"` shall carry a hardware-wallet action
request in `details`. For the EVM Trezor task path, the request may identify a
Trezor PIN request as `EnterTrezorPin` or a Trezor passphrase request as
`EnterTrezorPassphrase`. Clients shall present or collect the requested action
and submit it through `task::withdraw::user_action` (R49.22).

R49.19. `status: "Ok"` shall carry the completed transaction-details object
directly in `details`. Clients shall not expect an additional `result` field
inside `details`.

R49.20. `status: "Error"` shall carry the serialized structured withdrawal
error directly in `details`. The error payload shall include the public
`error_type` discriminator and any bound `error_data`; compatibility metadata
such as human-readable error text or trace fields may also be present. Clients
shall classify terminal task failure from `result.status == "Error"` and
`result.details.error_type`, not from the mmrpc top-level error envelope.

T49.16. Poll an in-progress withdrawal task. The mmrpc response shall contain a
top-level `result` object whose `status` is `InProgress` and whose `details`
contains a progress value.

T49.17. Complete a withdrawal successfully and poll its status. The mmrpc
response shall contain a top-level `result` object whose `status` is `Ok` and
whose `details` is the transaction-details object, including `tx_hex`,
`tx_hash`, `from`, `to`, `total_amount`, `fee_details`, `coin`, `internal_id`,
and `transaction_type`.

T49.18. Force a withdrawal task to fail after init, for example by using an
invalid sender selector on an HD wallet. The mmrpc response for the status call
shall still contain top-level `result`; inside that result, `status` shall be
`Error` and `details.error_type` shall identify the withdrawal error.

T49.19. Poll a missing or already-forgotten task. The response shall be an
mmrpc error response, not a successful task-status object.

## 49.5 User action and cancellation

R49.21. `task::withdraw::user_action` shall accept:

- `task_id` (integer, required) -- task identifier returned by R49.10.
- `user_action` (object, required) -- hardware-wallet action payload for the
  action currently requested by R49.18.

R49.22. For the EVM Trezor task path, `user_action` shall accept the matching
public action payload for the current Trezor request:

- `{"action_type": "TrezorPin", "pin": "<pin-matrix-response>"}`
- `{"action_type": "TrezorPassphrase", "passphrase": "<passphrase>"}`

Submitting a mismatched Trezor action for the current request shall fail with a
structured task-action error identifying the expected action type.

R49.23. `user_action` shall be meaningful only while the task is in the
`UserActionRequired` state. Submitting a user action for a task that is not
waiting for that action shall fail with a structured task-action error. Pure
software-signing withdrawals shall not require a `user_action` round trip.

R49.24. `task::withdraw::cancel` shall accept `task_id` and attempt to abort an
in-flight withdrawal task. Cancelling a task that has already reached a
terminal state shall fail with the standard cancel-task terminal-state error.

T49.20. Drive an EVM Trezor/hardware-wallet withdrawal task to
`UserActionRequired` with a PIN request, submit
`{"action_type": "TrezorPin", "pin": "<pin-matrix-response>"}` through
`task::withdraw::user_action`, and continue polling. The task shall leave the
awaiting-user-action state and either progress or terminate.

T49.21. Drive an EVM Trezor/hardware-wallet withdrawal task to
`UserActionRequired` with a passphrase request, submit
`{"action_type": "TrezorPassphrase", "passphrase": "<passphrase>"}` through
`task::withdraw::user_action`, and continue polling. The task shall leave the
awaiting-user-action state and either progress or terminate.

T49.22. Submit a Trezor passphrase action while the withdrawal task is awaiting
a Trezor PIN, or submit a Trezor PIN action while the task is awaiting a Trezor
passphrase. The call shall fail with a structured task-action error identifying
the expected action type.

T49.23. Submit `user_action` for a software-signing withdrawal task that is not
awaiting user input. The call shall fail with a structured task-action error.

T49.24. Cancel an in-progress withdrawal task, then poll its status. The task
shall not later return a public cancellation status; a status poll shall fail
as an unknown or unavailable task according to R49.14.

## 49.6 Completed transaction payload

> **UTXO input bookkeeping (cross-reference).** For a UTXO coin, whether the
> withdrawal path records the inputs its transaction spends, how the
> candidate inputs are listed, and what a refused broadcast looks like are
> bound by [Chapter 38](38-utxo-coin-maintenance-and-features.md) §38.5B
> (cleared by the Dirty Gate on 2026-10-30), not here.

R49.25. A successful direct `withdraw` response and a successful
`task::withdraw::status` terminal payload shall use the same transaction-details
object. A completed withdrawal shall include at least:

- `tx_hex`
- `tx_hash`
- `from`
- `to`
- `total_amount`
- `spent_by_me`
- `received_by_me`
- `my_balance_change`
- `block_height`
- `timestamp`
- `fee_details`
- `coin`
- `internal_id`
- `transaction_type`

`kmd_rewards` may also be present when the coin family supports it. Consumers
shall treat `tx_hex` and `tx_hash` as mandatory for a completed withdrawal
transaction and shall use the remaining metadata for history, balance, and
confirmation display.

R49.26. The field set of R49.25 is a **minimum**, not a closed set. A coin
family may bind *additional* top-level fields on the same transaction-details
object, and a consumer shall ignore a field it does not recognise rather than
treat it as a protocol error. Such an addition is conformant only while it is
strictly additive: it shall not remove a R49.25 field, shall not change an
existing field's name, JSON type, or meaning, and shall not make `tx_hex` or
`tx_hash` optional for the coin that adds it.

The one coin family that presently uses R49.26 is Siacoin, whose native
transaction serialisation is JSON text rather than a binary encoding. It
carries the signed transaction twice: in the mandatory `tx_hex` of R49.25, as
hex of that JSON text, and additionally in a top-level `tx_json` object holding
the same JSON unencoded. That carrier pair, its precedence when the transaction
is fed back to the legacy broadcast method, and the Sia-specific
`transaction_type` and `internal_id` values are bound by
[Chapter 20](20-siacoin-integration.md) §20.9.1-§20.9.2 (R-W6 through R-W11),
not by this chapter. `tx_json` is absent for every other coin family, and its
absence is not an error.

T49.25. For the same supported coin family and signing policy where both direct
`withdraw` and task withdrawal are supported, compare a successful direct
`withdraw` result with the `details` payload of a successful task withdrawal.
Both shall expose the same transaction-details field contract.

T49.26. For a coin family that binds an additional field under R49.26, complete
a withdrawal through both the direct `withdraw` method and the task path and
confirm that the R49.25 minimum field set is present and unchanged in both, that
`tx_hex` and `tx_hash` are non-empty, and that the additional field appears in
both payloads. Repeat for a coin family that binds no additional field and
confirm the extra field is simply absent and no error is raised.

R49.27. *Sender selection per coin family under a global-HD login (reference
contract).* *(Passed the KDF Dirty Gate on 2026-11-04, measure 0.01; Spec Reader 2026-11-04, rows
`v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both.)*
Beyond R49.3--R49.5: the SLP token withdraw spends with its platform coin's
activated key and fails a request that carries `from` with the
unsupported-operation discriminant; the shielded coin is served by the task
route only (chapter 39 R39.5A.2) and fails `from` and `fee` the same way;
the Sia withdraw spends from the coin's single address and **ignores** `from`
entirely (chapter 20 R-W1); the Tendermint withdraw accepts `from` as an HD
selector and signs with the key derived at that path, and spends from the
activated key when `from` is absent. Chapter 05 §5.9B tabulates the keys
behind these addresses.

T49.27. Under a global-HD login: an SLP withdraw with `from` and a shielded
task withdraw with `from` shall each fail with the unsupported-operation
discriminant; a Sia withdraw with a `from` selector shall succeed from the
coin's single address exactly as without it; a direct UTXO `withdraw` without
`from` shall fail with the sender-not-found discriminant under the reference
contract (Reloaded's direct `withdraw` serves an HD UTXO wallet since plan
`hd-key-unification` item H5 and, like its task route, uses the coin's
activated address when `from` is absent, R49.4 -- the test shall state which
contract it asserts).

R49.27a. *Wire form of the unsupported-operation refusal (reference
contract).* *(Passed the KDF Dirty Gate on 2026-11-04, measure 0.01; Spec Reader 2026-11-04, rows
`v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both
except where a row is named.)* Every "unsupported" refusal of R49.27 is one
arm of the structured withdrawal error of R49.20, and the arm is **shared**:
there is no per-feature discriminant. Its wire form is

- `error_type`: the wire constant `UnsupportedError`;
- HTTP status **400** on the direct `withdraw` method, where the error is
  the mmrpc error envelope; on the task route the same object arrives in
  `result.details` of a status poll whose `status` is `Error` (R49.20);
- `error_data`: **present**, a plain JSON string of free human-readable text
  naming the refused operation. The wording is not contract; a client shall
  classify by `error_type` alone and never parse `error_data`.

The refusals that answer this discriminant at the reference are: (a) `from`
on an SLP token withdraw; (b) `fee` on a shielded task withdraw and
(c) `from` on a shielded task withdraw (checked in that order, `fee` first);
a UTXO-family withdraw whose activated key is a WalletConnect signer or, in
a WASM build, a browser-extension signer (signing is not available to the
family); the legacy direct `withdraw` of an EVM coin under a hardware-wallet
key (the task route of chapter 50 is required); an EVM withdraw under a
WalletConnect key for a coin configured without a chain identifier; and, at
the `v3.0.0-beta` row only, a TRON withdraw that carries `memo`, or runs
under a hardware-wallet, WalletConnect or browser-extension key. A Sia
withdraw **never** answers it for `from` -- the selector is ignored, as
R49.27 records; Sia's one arm fires for a key policy other than single-key
or global-HD, which Sia activation already refuses, so it is unreachable in
practice. Refusals that are **not** this arm and carry their own
discriminant, all HTTP 400: a missing required sender
(`FromAddressNotFound`, T49.27), a sender selector that does not resolve
(`UnexpectedFromAddress`), an unknown HD account (`UnknownAccount`), a fee
descriptor of a type the coin does not take (`InvalidFeePolicy`, chapter 38
R38.6.6l/R38.6.6m), fee values the coin rejects (`InvalidFee`), and a
destination that does not decode (`InvalidAddress`, chapter 39 R39.5A.2).

T49.27a. *(Reloaded-original.)* Against mocked backends: an SLP token
`withdraw` carrying `from` shall answer HTTP 400 with
`error_type: "UnsupportedError"` and a string `error_data`; a shielded
`task::withdraw::init` carrying `fee`, and another carrying `from`, shall
each reach `status: "Error"` with `details.error_type == "UnsupportedError"`
and a string `details.error_data`; a Sia withdraw carrying `from` shall
succeed; an SLP withdraw with an EVM-form fee descriptor shall answer
`InvalidFeePolicy`, not `UnsupportedError`; and the direct UTXO withdraw of
T49.27 without `from` shall answer `FromAddressNotFound`, not
`UnsupportedError`, where the test asserts the reference contract. Each
assertion shall compare the discriminant and the JSON type of `error_data`
only, never the text.

> **Compatibility row (this repository, 2026-11-05; not corpus-sourced).**
> **Parity** since PR #236 for the SLP row and for the hardware-wallet and
> browser-extension rows that Reloaded serves. An SLP token `withdraw` that
> carries `from` answers `UnsupportedError`. So do the direct `withdraw` of an
> EVM coin under a Trezor key, a TRON withdraw under a Trezor key and, on a
> browser build, a TRON withdraw under MetaMask (chapters 47 and 50), each with
> its former text kept in `error_data`. Before, Reloaded ignored `from` on an
> SLP withdraw and spent from the platform coin's activated key, and it
> answered the Trezor and MetaMask cases with its own discriminants
> `UnsupportedUnderTrezor` and `UnsupportedUnderMetamask` (historical). The
> shielded rows, and the other refusals R49.27a lists, are not covered by this
> row. No peer observes the discriminant, so arm 1 of
> `docs/COMPAT_SWITCHES.md` "The default-side rule" does not bind the default,
> and the change has no switch.
>
> **Shielded rows (this repository, 2026-11-05; not corpus-sourced).** **Parity**
> since PR #256. A shielded withdraw that carries `fee` answers `UnsupportedError`,
> and so does one that carries `from`, with `fee` checked first, on the direct
> method and on `task::withdraw::init` alike. Before, Reloaded answered `fee`
> with an internal error (HTTP 500) and ignored `from`, spending from the
> wallet's own address. Tests:
> `z_coin::z_note_lock_tests::withdraw_unsupported`. As for the SLP row, no peer
> observes the discriminant, and the change has no switch.

R49.27b. *The `from` selector of a UTXO-family withdraw: what it must name
(reference contract).* *(Spec Reader 2026-11-07, rows `v2.6.0-beta` and
`v3.0.0-beta` of chapter 02 §2.15A, identical at both; **passed the KDF
Dirty Gate on 2026-11-07**, measure 0.02.)* The
direct `withdraw` method and `task::withdraw::init` resolve the sender of a
UTXO-family (UTXO, QTUM, BCH) withdraw in one shared step, so the following
holds on both routes alike, with every refusal an HTTP 400 arm of the
structured withdrawal error of R49.20:

1. *Single-key login.* An explicit `from` is refused with
   `UnexpectedFromAddress`, **whatever it names**: the selector is not
   compared with the login's address, so a selector that names the login's
   own single address -- in the address-id form or the derivation-path form
   -- is refused exactly like any other. There is no "accept as no-op". This
   is R49.3, which the reference satisfies on both routes.
2. *HD login, `from` absent.* `FromAddressNotFound` (R49.4's reference note).
3. *HD login, `from` present.* The selector is first turned into an account,
   chain and index: a derivation-path form that is not a standard
   five-level address path, or whose coin type is not the coin's, answers
   `UnexpectedFromAddress`. The account must exist in the wallet, else
   `UnknownAccount` with `account_id`. The address must be **already known**
   -- its index below the chain's known-address count -- else
   `UnexpectedFromAddress`; the step derives the address to report it in the
   error text but records nothing, so a withdraw never adds an address to the
   wallet (chapter 05 R35a item 4). A chain the coin does not support is an
   internal-class failure. When all three hold, the key at the selected path
   signs and change returns to the selected address (R49.27).

> **Compatibility (this repository, 2026-11-07; not corpus-sourced).** R49.3
> already requires item 1; whether Reloaded's direct `withdraw` and task route
> both refuse a selector that names the single address, rather than accepting
> it as a no-op, is for the clean side to confirm against this repository's
> code when plan `hd-key-unification` item H5.1 is implemented. Items 2 and 3
> are the contract behind T49.5 and T49.18.

> **As built (2026-11-07, plan item H5.1, which also covers H5.5).**
> `refuse_from_for_single_key` (`utxo_withdraw.rs`) answers any explicit
> `from` with `UnexpectedFromAddress` for a coin with a single-address
> derivation method. The task route's `get_withdraw_iguana_sender` and the
> direct route's `StandardUtxoWithdraw::new` share it. The direct `withdraw`
> of the UTXO standard coin, of QTUM and of BCH therefore refuses a selector
> that names the login's own address, in either form, before any unspent
> output is listed. Before H5.1, the direct route ignored `from`. Tests:
> - `t49_27b_single_key_direct_withdraw_refuses_from_utxo_standard`, `_qtum`
>   and `_bch`;
> - `t49_27b_single_key_shared_sender_step_refuses_from`;
> - the HD refusal tests `t49_6_direct_*`.

T49.27b. *(Reloaded-original; passed the KDF Dirty Gate together with
R49.27b on 2026-11-07.)* Under an iguana login, a direct `withdraw` and a task withdraw
that each carry `from` naming the login's own address (address-id form, then
derivation-path form) answer `UnexpectedFromAddress`, HTTP 400, on the
direct route and in `details.error_type` on the task route; under an HD login
with three known external addresses in account `0`, a `from` naming
`address_id: 3` answers `UnexpectedFromAddress` and `account_id: 1`
answers `UnknownAccount`, and `account_balance` afterwards still lists three
addresses.

## 49.7 Provenance Footer

- *Inputs:* the project's own revision history and current tree (the
  already-shipped direct `withdraw` method and `task::withdraw::*` family
  this chapter documents as-built -- by public wire contract only, no code
  transcribed); the shared mmrpc-2.0 task pattern (`init`/`status`/
  `user_action`/`cancel`, the standard task-status envelope) already
  established by sibling `task::` families; [Chapter 35](35-evm-v2-activation-rpcs.md)
  R35.1.4/R35.3.2 (the Trezor hardware policy this chapter's sender
  selection and `user_action` surface accommodate); [Chapter 50](50-evm-trezor-signing.md)
  R50.1-R50.4 (the EVM Trezor withdrawal path built directly on this
  chapter's wire); [Chapter 20](20-siacoin-integration.md) §20.9.1-§20.9.2
  (the Siacoin transaction-carrier and record-identity fields this chapter's
  R49.26 admits as the one present use of an additional coin-family field).
- *Amendment (2026-11-04) -- R49.4, its divergence note, and T49.5.* They
  now state the shipped default sender for an HD UTXO task withdrawal that
  omits `from`. Reloaded-original. *Inputs:* this repository's own withdraw
  sender selection and `RELOADED_VS_GLEEC.md`. No reference implementation
  was consulted for this amendment, and it needs no Dirty Gate pass.
- *Permitted-input classes used:* baseline/as-built source (the shipped
  withdraw wire and shared task-status envelope pattern); cross-chapter
  contracts (Chapters 20, 35, 50); Interop / wire-and-API-bound reuse (R29/R31)
  for the dictated request/response field names and task-status vocabulary.
- *Sibling-allowlist consultations:* [Chapter 20](20-siacoin-integration.md),
  [Chapter 35](35-evm-v2-activation-rpcs.md),
  [Chapter 50](50-evm-trezor-signing.md).
- *Forbidden corpus:* consulted **only** for the fact that one coin family's
  deployed reference response carries an additional top-level transaction
  carrier alongside the R49.25 minimum field set -- the fact R49.26 generalises.
  The carrier's own wire detail is recorded in
  [Chapter 20](20-siacoin-integration.md) §20.9.1, not here. No discretionary
  expression crosses into this chapter.
- *Cross-reference added 2026-10-30.* The pointer note at the head of §49.6
  to [Chapter 38](38-utxo-coin-maintenance-and-features.md) §38.5B was added
  by the Spec Reader in the pass that wrote §38.5B. It carries no reference
  content of its own; the facts it points to are recorded, and their
  clearance status tracked, in chapter 38 §38.9.
- *Scoped addition (2026-11-04) -- the reference-behavior note under R49.4,
  R49.27 and T49.27.* **KDF Dirty Gate: PASS on 2026-11-04, measure 0.01.** *Inputs:* the forbidden
  corpus at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A,
  identical at both, consulted **only** to establish the sender resolution of
  the direct and task withdraw routes under a global-HD login per coin family
  (UTXO-family, SLP, shielded, Sia, Tendermint): when `from` is required,
  refused or ignored, which key signs, and where change returns. Behavior and
  dictated request members only; no private identifier, function body, helper
  decomposition, error, log or comment text was reproduced. The compatibility
  sentences rest on this repository's own documents; T49.27 is
  Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04, measure
  0.01, no findings. *Extent:* the sender resolution of the direct and task
  withdraw routes under a global-HD login, per coin family, at `v3.0.0-beta`
  with `v2.6.0-beta` as cross-check; for the whole batch, 30 source files and
  one dependency manifest were read at `v3.0.0-beta`, 29 of them checked by
  hash against the tagged states of both rows. *Method:* side-by-side reading
  of the compared regions against every added or changed line, plus that hash
  check; the gate script was not run because the candidate has no code blocks.
  *Measure:* 0.01, read against the standard that any discretionary finding
  fails a file; dictated-interop and public-surface tokens were excluded from
  the measured set.
- *Scoped addition (2026-11-04) -- R49.27a and T49.27a.* **KDF Dirty Gate:
  PASS on 2026-11-04, measure 0.01.** *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, consulted **only** to establish the
  wire form of the withdraw family's unsupported-operation refusal -- the
  `error_type` discriminant, the HTTP status, the presence and JSON type of
  `error_data`, which refusals share the discriminant at each row and which
  carry a distinct one. The discriminant strings are externally visible
  inter-operability surface (chapter 01 R11, first carve-out class) and are
  recorded as wire constants under Interop / wire-and-API-bound reuse
  (R29/R31). No private identifier, function body, helper decomposition,
  error, log or comment text was reproduced; the human-readable text of
  `error_data` is deliberately not recorded. T49.27a is Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-04, measure
  0.01, no findings. *Extent:* the withdrawal-error wire contract and the
  withdraw entry points of the SLP token, shielded, Sia, UTXO-family and EVM
  and TRON coin families, at `v2.6.0-beta` and `v3.0.0-beta`; twelve
  file-states in all. *Method:* reading, with text search at the two tagged states;
  the gate script was not run because the candidate has no code blocks.
  *Measure:* 0.01, read against the standard that any discretionary
  identifier, error, log or comment literal, body or control-flow
  transcription fails regardless of volume; dictated wire surface was
  excluded.
- *Clean-side amendment (2026-11-05) -- the divergence note under R49.4, the
  compatibility clause of the reference-behavior note under R49.4, and
  T49.5.* They now cite the maintainer's decisions BY (the fallback stays a
  documented divergence) and BZ (the direct method with `from`) of
  2026-11-04, recorded in plan `hd-key-unification`. Written by a session
  that never read the corpus, from those decisions and this repository's own
  code. The reference facts of the note are unchanged. It takes no
  forbidden-class input and needs no Dirty Gate pass.
- *Scoped addition (2026-11-07) -- R49.27b, T49.27b and the compatibility
  note under R49.27b.* **KDF Dirty Gate: PASS on 2026-11-07, measure 0.02.** *Inputs:* the forbidden corpus at the rows
  `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, identical at both,
  consulted **only** to establish how the shared sender-resolution step of
  the UTXO-family withdraw treats an explicit `from` under a single-key login
  (refused regardless of what it names, with which discriminant and status,
  on both routes) and what a `from` must name under an HD login (an existing
  account, an already-known address; which discriminants otherwise; that
  nothing is recorded). Behavior and dictated request members and error
  discriminants only; no private identifier, function body, helper
  decomposition, error, log or comment text was reproduced. The compatibility
  note rests on this repository's own documents; T49.27b is Reloaded-original.
  *Dirty Gate clearance (chapter 01 T1, R40).* PASS on 2026-11-07, measure
  0.02, no findings. *Extent:* UTXO-family withdraw sender resolution on the
  direct and task routes, at `v3.0.0-beta` with `v2.6.0-beta` as cross-check;
  for the whole batch, about 24 source files were read in full or in the
  relevant regions at the `dev (as of 2026-03-27)` row, whose source tree is
  identical to `v3.0.0-beta`, and the same modules were diffed against
  `v2.6.0-beta`. *Method:* reading, with mechanical cross-checks (diffs
  between the tabled states; pattern searches of the added lines for paths,
  identifiers and markers); the gate script was not run because the candidate
  has no code blocks. *Measure:* 0.02, read against the standard that any
  discretionary finding fails a chapter; dictated-interop and public-surface
  tokens were excluded from the measured set.
- *Clean-side amendment (2026-11-05) -- PR #236.* A session that never read
  the corpus added the compatibility row under T49.27a. It rests on R49.27a as
  gated above and on this repository's own code, takes no forbidden-class
  input, and so does not engage chapter 01 R38.
- *Clean-side amendment (2026-11-06) -- R49.4, its divergence note, the
  compatibility clause of the reference-behavior note under R49.4, T49.5 and
  T49.27.* They record decision CU of 2026-11-06 (the default sender without
  `from` is the coin's activated address, the address at `path_to_address`)
  and decision BZ (the direct method serves an HD UTXO wallet), both in plan
  `hd-key-unification`. Written by a session that never read the corpus,
  from those decisions and this repository's own code. The reference facts
  of the note under R49.4 are unchanged. It takes no forbidden-class input
  and needs no Dirty Gate pass.
- *Scoped addition (2026-11-08) -- §49.2A (R49.28 -- R49.31, the Reloaded
  status note, D49.1, T49.28 -- T49.30) and the pointer sentences added to the
  cross-reference note under R49.1.* **KDF Dirty Gate: PASS on 2026-11-08,
  measure 0.03, no findings** (chapter 01 R40; the attestation at the end of
  this item closes it). *Inputs:* the forbidden corpus at the rows
  `tendermint-priority-fee (as of 2025-07-24)` and `dev (as of 2025-07-18)` of
  chapter 02 §2.15A, consulted **only** to establish the wire shape of the
  priority Cosmos fee descriptor (tag, members, the four level strings), which
  requests accept it, how a level resolves to a price and what an unhonorable
  level does; and the rows `v2.6.0-beta` and `v3.0.0-beta`, consulted **only**
  to confirm the explicit Cosmos descriptor's tag and members, that it is
  accepted by the same requests, that its amount is simulation-derived, that a
  foreign fee form is ignored without error, and that neither released row
  carries the priority form. The draft's review state is recorded as a fact
  only. Behavior and dictated request members, tag values and error
  discriminants only; no private identifier, function body, helper
  decomposition, error, log, comment, test body or review text was reproduced.
  The Reloaded status note, D49.1 and T49.30 rest on this repository's own
  tree and documents and are Reloaded-original. *Dirty Gate clearance (chapter
  01 T1, R40):* PASS, 2026-11-08. Extent: the Tendermint fee-computation and
  fee-descriptor surface — the Cosmos withdraw, staking and IBC fee
  descriptors, the swap fee-policy RPC pair and its policy enum, and the
  Tendermint protocol descriptor's gas-price members — compared at the row
  tendermint-priority-fee (as of 2025-07-24) against its base row dev (as of
  2025-07-18) over the complete difference between those two states (nine
  source files, two of them test files), together with the public description,
  two review comments and three discussion comments of the draft pull request
  those rows record; and at the rows v2.6.0-beta and v3.0.0-beta for the
  released fee-policy method strings, request member and enum values, the
  explicit Cosmos fee descriptor, the handling of a foreign fee form on a
  Cosmos request, and the staking and IBC gas-limit defaults. Method: reading
  of the full inter-state difference and of the release-state passages,
  combined with mechanical text search of the candidate's added lines against
  identifier, numeric-constant, string-literal, name and number lists drawn
  from the compared states; no code-block tool run was needed (the candidate
  embeds only two one-line JSON wire examples). Measure:
  discretionary-similarity 0.03, read against the per-token forced-vs-free
  standard of chapter 01 R35/R39 (dictated wire, config, error-discriminant
  and contract-constant tokens excluded; FAIL on any private identifier, body
  or control-flow transcription, error/log wording, internal module tree or
  internal test name) — below threshold, PASS.
- *Clean-context addition (2026-11-09, forbidden corpus not consulted):* the
  recorded resolution of D49.1, the dated status notes after R49.30 and
  T49.30 (the decided behavior; the cleared text above them is unchanged), the
  as-built note of §49.2A with its test-coverage sentence, and the two
  clean-side notes after the cross-reference under R49.1 and the Reloaded
  status note describe the maintainer's decision of 2026-11-09 and this repository's code.
  They take no forbidden-class input, so chapter 01 R38 is not engaged and no
  gate pass is required.
