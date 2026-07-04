# Chapter 40 -- Solana Coin

**Status:** driving-spec (as-built; documents shipped reloaded behaviour).

> **One-sentence claim:** the project shall support Solana as a platform coin and
> SPL tokens as its child tokens, activating them through the
> platform-coin-with-tokens model, querying balances and the current slot over
> Solana's JSON-RPC, and exposing address/withdraw/transaction operations against
> the Solana and SPL-Token programs.

> **Treatment:** **T-DOC.** Reloaded ships a Solana coin module and SPL-token
> support wired into activation (`enable_solana_with_tokens`, `enable_spl`). This
> chapter documents that shipped capability.

> **Historical note (informative).** Solana support in this lineage was removed
> at one point as an unmaintained proof-of-concept and later reintroduced. The
> requirements below describe the *reintroduced* implementation present in
> reloaded; they do not bind the earlier removed version.

## 40.0 Executive Summary

Solana is integrated as a **platform coin** (the native SOL coin) that carries a
set of **SPL token** child coins. Activation follows the same
platform-coin-with-tokens pattern used elsewhere in the project: one request
enables the platform plus an inline list of its tokens. Balances and chain height
come from Solana's JSON-RPC; addresses and signing follow Solana's ed25519 /
base58 account model and the SPL Token program.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> activation RPC method strings and their request/response field names, and
> externally *dictated* interop: the **Solana JSON-RPC** method set, the SPL
> **Token program** account/instruction model, ed25519 keypairs, base58 address
> encoding, and lamport units. Those are the source of truth, not this project's
> code. Private types and helper structure are informative.

## 40.1 Activation -- platform coin with tokens

R40.1.1 The public RPC `enable_solana_with_tokens` shall enable the Solana
platform coin together with an inline list of SPL tokens in a single call. Its
request shall carry:
- the Solana platform activation parameters (flattened into the request), and
- a `spl_tokens_requests` list, each entry naming a token to activate.

R40.1.2 The success result shall report:
- `current_block` -- the current Solana slot/height;
- `solana_addresses_infos` -- a map from address to its SOL balance info; and
- `spl_addresses_infos` -- a map from address to its per-token balances.

R40.1.3 The public RPC `enable_spl` shall enable a single SPL token against an
already-active Solana platform coin.

R40.1.4 An SPL token's configuration shall carry at least its decimals and its
on-chain token mint (contract) address; a token is bound to its platform coin at
activation.

## 40.2 Balances & chain state (R31 dictated by Solana JSON-RPC)

R40.2.1 SOL and SPL balances shall be read via Solana JSON-RPC, converting
lamports / token base units to decimal using the coin's decimals. The platform
balance shall aggregate the SOL balances across the wallet's addresses.

R40.2.2 The current slot/height shall be read via Solana JSON-RPC and surfaced as
`current_block`.

## 40.3 Addresses, withdraw & transactions (R31 dictated by Solana/SPL)

R40.3.1 Addresses shall follow Solana's ed25519 keypair / base58 account model;
SPL balances shall be read from the owner's associated token accounts for each
mint.

R40.3.2 The coin shall support constructing, signing, and broadcasting SOL and
SPL transfers and shall decode transaction details returned by the node into the
project's common transaction-details shape.

## 40.4 Acceptance criteria

- `enable_solana_with_tokens` activates SOL plus one or more SPL tokens and
  returns `current_block`, `solana_addresses_infos`, and `spl_addresses_infos`
  (R40.1).
- `enable_spl` adds a token to an active Solana platform coin (R40.1.3).
- SOL and SPL balances and the current slot are read correctly via Solana
  JSON-RPC (R40.2).
- A SOL transfer and an SPL transfer can be built, signed, broadcast, and their
  details decoded (R40.3).
