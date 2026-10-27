# Plan: Tron GasFree fee-delegated TRC20 withdraw rail

**Status:** parked (branch-only; gated on pinning the GasFree proxy creation bytecode) — **Last reviewed:** 2026-10-18

> **Context:** written 2026-10-18, when a review of three stale June-2026
> branches found a complete, test-covered GasFree implementation and an
> 807-line gated CRD chapter living only on `origin/feat/tron-gasfree`, with
> **no trace of either on `dev`** -- not in the CRD, not in `ROADMAP.md`, not in
> `CHANGELOG.md`, not in `RELOADED_VS_GLEEC.md`. The maintainer decided to park
> the branch: it is neither to be merged nor deleted. This plan, not a dangling
> ref, is what keeps the work findable.

> **Beta availability (2026-10-26).** By maintainer decision TRON (TRX, TRC20)
> activation is switched off on `dev` for the coming beta: while
> `TRON_ACTIVATION_AVAILABLE` (`coins/lp_coins_ops.rs`) is `false`, every TRON
> activation route answers "TRON support is not available in this build" (CRD
> ch.21 availability note). TRON is planned to return in the next beta; turning
> activation back on (ch.21 D6) precedes the activation-time steps below and is
> part of any revival of this plan.

## Blocker: the derived custody address is knowingly wrong

**Nothing reachable may be built on this derivation until the proxy creation
bytecode is pinned and the derivation is verified against at least two
published custody-address vectors per network.**

`mm2src/coins/eth/tron/gasfree/config.rs:106` on the branch:

```rust
// TODO(gasfree): pin official GasFree SDK proxy creation bytecode per network.
// This is the only remaining placeholder of the §49.5 artifacts: until the
// exact SDK bytes are embedded, the derived custody address will not match the
// published per-network vectors.
const GASFREE_PROXY_CREATION_BYTECODE: &[u8] = &[];
```

A `CREATE2` derivation over empty creation bytecode yields a **different**
address from the real one, and that address is exactly what a user would be
told to fund. The only thing standing between this placeholder and lost funds
today is that the code is unreachable (see the second hazard below): the rail is
never called, and `gasfree_address` is hard-wired to `None`. Any step that makes
the rail reachable -- activation wiring, dispatcher wiring, exposing the address
over RPC -- removes that protection, so the bytecode is a precondition for all
three and not a parallel task.

Verification is not "the unit tests pass". The module's 73 tests were written by
the same hand as the code and exercise the derivation against its own
placeholder. What is required is two or more **published** custody-address
vectors per network (mainnet, Shasta, Nile) reproduced by
`derive_gasfree_address`.

## Second hazard: merging as-is would commit inert RPC surface

`gasless_withdraw` is exported from
`mm2src/coins/eth/tron/gasfree/mod.rs` but is **never called from the withdraw
dispatcher**; the only non-test change the branch makes to `eth_impl.rs` is a
single `..Default::default()`. `gasfree_address` is structurally present on
`CoinBalance` and always `None` -- `5e0af6f80`'s own message records that "live
`gasfree_address` population [is] deferred to coin-activation wiring (field
exposed structurally, stays None until wired)".

So merging the branch would add three RPC-visible fields -- `fee_method` and
`gasless` on the withdraw request, `gasfree_address` on the balance response --
with no behavior behind any of them. That is a wire-surface commitment in a
beta, paid for with nothing: integrators would see and start using fields that
do nothing, and the shapes would then be hard to change once the real behavior
arrives. The fields and the behavior must land together.

## Where the work lives

Branch `origin/feat/tron-gasfree`, four commits, 2026-06-19 to 2026-06-26,
`+4006 / -7` across 43 files. Merge base with `dev`:
`075f79a3d691a239b859cb42b79b1213323535de` (2026-06-26).

| SHA | Date | Subject |
|---|---|---|
| `e52fbbaca1ec2eb1d4565d3d244f9f3ba5c0d8ab` | 2026-06-19 | `crd(49): add Tron GasFree fee-delegation chapter (feat/tron-gasfree)` |
| `f26e865e8c730e848c95f2eec615cfa77b57c6f7` | 2026-06-26 | `feat(tron): implement GasFree gasless TRC20 withdraw rail (ch.49)` |
| `76a3bbd816f3c20979c629b26008cb79be322e93` | 2026-06-26 | `docs(crd): ch.49 parity corrections for Tron GasFree` |
| `5e0af6f80328b2271534ffe21c1aa73f1a499bb0` | 2026-06-26 | `feat(tron): wire GasFree onto shared surface, bind constants, port submit (ch.49 parity)` |

**Do not delete the branch.** Nothing on `dev` records that this capability was
ever specified or attempted, so the branch and this plan are the whole record.

## What is on the branch

A new module `mm2src/coins/eth/tron/gasfree/`, about 3000 lines, registered by a
one-line `pub mod gasfree;` in `mm2src/coins/eth/tron/mod.rs`:

| File | Lines | Tests | Role |
|---|---|---|---|
| `config.rs` | 440 | 15 | per-network constants, provider/token config and validation |
| `derive.rs` | 209 | 7 | Tron `CREATE2` custody-address derivation (§49.5) |
| `permit.rs` | 396 | 7 | TIP-712 `PermitTransfer` construction and signing (§49.6) |
| `client.rs` | 820 | 20 | provider REST types, HMAC auth, response envelope (§49.4) |
| `preflight.rs` | 366 | 11 | availability decision and fee math (§49.7) |
| `withdraw.rs` | 546 | 10 | rail selection, sign-only orchestration, public surface |
| `error.rs` | 191 | 3 | error taxonomy and HTTP status mapping (§49.9) |
| `mod.rs` | 36 | -- | layout documentation and re-exports |

73 `#[test]` functions at the branch tip. (`f26e865e8` claims 65 passing and
`5e0af6f80` claims 77; neither figure has been re-measured.)

Shared-surface additions, from `5e0af6f80`:

- `mm2src/coins/lp_coins_types.rs`: `WithdrawRequest` gains
  `#[serde(default)] fee_method: Option<FeeMethod>` and
  `#[serde(default)] gasless: Option<GaslessWithdrawOptions>` plus
  `#[derive(Default)]`; `CoinBalance` gains
  `#[serde(default, skip_serializing_if = "Option::is_none")] gasfree_address:
  Option<String>`. Four tests assert the non-Tron wire format is unchanged.
- `mm2src/coins/lp_coins_errors.rs`: `WithdrawError::Gasless(GasFreeWithdrawError)`,
  delegating `status_code()` to the inner error.
- `mm2src/mm2_net/src/{native_http.rs,wasm_http.rs,transport.rs}`: a new
  cross-platform `slurp_post_json_with_headers(url, headers, body)`.
- `indexmap = { version = "1.7", features = ["serde-1"] }` added to
  `mm2src/coins/Cargo.toml`, used by `permit.rs` for ordered TIP-712 field maps.
- About 46 mechanical `..Default::default()` additions at `CoinBalance` and
  `WithdrawRequest` construction sites across the UTXO, Solana, Tendermint,
  Lightning, QRC20, Sia, ZCoin and EVM modules and four files under
  `mm2src/coins_activation/src/`.

Network constants bound in `5e0af6f80`: per-network controller and beacon
base58 Tron addresses, the `initialize(address)` selector `0xc4d66de8`, and the
three TIP-712 chain IDs -- mainnet `728126428`, Shasta `2494104990`, Nile
`3448148188`.

### The chapter is the expensive half

`docs/reloaded-rewrite/49-tron-gasfree-fee-delegation.md`, 807 lines at the
branch tip (735 as first written in `e52fbbaca`, then `+99/-27` in the
`76a3bbd81` parity pass). It is the most thorough of the three June chapters and
**it is the part that needed forbidden-corpus access to recover**: the provider
REST surface and its HMAC request authentication, the response-envelope quirks
(the fee-field alias `estimatedTransferFee` / `estimateTransferFee`, integers
arriving as either numbers or decimal strings), the TIP-712 `PermitTransfer`
layout, the `CREATE2` rule, the error taxonomy with its status mapping, and the
per-network constants. Section map:

```
49.0  Executive Summary            49.6   PermitTransfer / TIP-712 (dictated)
49.1  Baseline Verification        49.7   Account preflight + availability
49.2  Subsystem Placement          49.8   Gasless withdraw behavior (sign-only)
49.3  Public RPC + activation      49.9   Error taxonomy + status codes
      .1 platform activation       49.10  Binding Requirements
      .2 token activation          49.11  Tests
      .3 balance response          49.12  Deferred Work
      .4 withdraw request          49.13  External References
      .5 withdraw response         49.14  Baseline Verifications
      .6 progress phases           49.15  Provenance Footer
49.4  Provider REST API (dictated) -- .1 endpoints .2 HMAC auth .3 envelope
      .4 account info .5 supported tokens .6 submit payload .7 submit/trace
49.5  CREATE2 custody derivation (dictated)
```

Plus a cross-reference paragraph added to ch.21 §21.10 pointing at ch.49.

If the chapter were lost, recovering it would mean another Spec Reader pass over
the corpus. The Rust is reproducible from the chapter; the chapter is not
reproducible from the Rust.

## Does it still apply?

`git merge-tree --write-tree --name-only origin/dev origin/feat/tron-gasfree`
reports **seven conflicts, every one mechanical**: they are `CoinBalance` and
`WithdrawRequest` construction sites, or dependency manifests, that `dev` has
edited since the merge base. None of them is Tron code.

```
Cargo.lock
mm2src/coins/Cargo.toml
mm2src/coins/eth/eth_impl.rs
mm2src/coins/lp_coins_types.rs
mm2src/coins/z_coin.rs
mm2src/coins_activation/src/erc20_token_activation.rs
mm2src/coins_activation/src/eth_with_tokens_activation.rs
```

Everything else auto-merges, including both `mm2_net` files and all the UTXO,
Solana and Tendermint call sites. Every dependency the module uses still exists
on `dev`: `mm2_eth::typed_data::{hash_typed_data, Eip712, TypedField}`
(`mm2src/mm2_eth/src/typed_data.rs:54`, `:166`),
`mm2_eth::keys::{sign, KeyPair, Secret}` (`mm2src/mm2_eth/src/keys.rs:255`) and
the `mm2_net` slurp helpers (`mm2src/mm2_net/src/native_http.rs:68`,
`wasm_http.rs:39`), whose `Request<Vec<u8>>` body shape the new helper matches.

**This branch does not use `ethabi`**, so the `ethabi`-to-`alloy` migration that
blocks a rebase of `feat/tron-swap-support` does not touch it. Note that `dev`
is 498 commits ahead of the merge base, so "seven mechanical conflicts" is a
statement about today and decays; re-run the `merge-tree` check before acting on
it.

## Remaining work

1. **Pin the official GasFree SDK proxy creation bytecode per network, and
   verify `derive_gasfree_address` against at least two published custody-address
   vectors per network.** The blocker above. `derive_gasfree_address` already
   takes the bytecode through `GasFreeArtifacts::creation_bytecode`, so swapping
   in the real bytes is the only code change; the verification is the work.
2. **Activation-time provider binding and live `gasfree_address` population.**
   Ch.49 §49.3.1-§49.3.3. This is what turns the structurally-present balance
   field into a real one, so it comes after step 1.
3. **Wire the rail into the withdraw dispatcher, then implement submit and
   on-chain settlement tracking** -- the chapter's `D-submit`, including the
   §49.3.6 long-running progress phases and their terminal states. This is the
   step that makes the derivation reachable, so it comes last.

Steps 2 and 3 are the point at which the inert RPC fields acquire behavior; they
must land in the same release as the fields, not before.

## Chapter renumbering: 49 → 57

`dev` has its own `docs/reloaded-rewrite/49-withdrawal-task-path.md` on a wholly
different topic, and `dev`'s chapter sequence currently runs to 55. **56 is being
taken by the EVM EIP-1559 fee-estimation revival in flight**, so this chapter
becomes **57**. The rename touches the filename, the title line, every `§49.x`
cross-reference inside the chapter, the ch.21 §21.10 cross-reference link, and
the three `00-overview.md` index edits (substantive-chapter class, R5, reading
order).

Because the filenames differ, git reports **no file conflict** for the chapter
itself -- only the `00-overview.md` index sentence conflicts, and that is easy
to resolve wrongly and end up with two chapter 49s in the tree. Do the rename
before resolving the index.

## Migrate the Dirty Gate verdict into the chapter

`76a3bbd81`'s message ends *"Dirty Gate PASS (0.08)."* **That commit message is
the only record of the verdict anywhere.** Recent chapters on `dev` carry gate
clearance inline in the chapter text instead -- see
[ch.47 §47 clearance notes](../reloaded-rewrite/47-metamask-integration.md)
(lines 359 and 877) and
[ch.55](../reloaded-rewrite/55-coin-activation-wire-contract.md) (§55.16 provenance footer) --
and a squash merge of this branch would destroy the only record of its pass.

So: **the first step of any revival is to copy the `0.08` verdict into the
chapter text**, with the commit it came from, before the branch history can be
collapsed.

## If both Tron branches are revived

The two Tron branches -- this one and
[`tron-swap-support`](./tron-swap-support.md) -- are **independent of each
other** and either can be revived alone. They overlap in exactly two places: one
`pub mod` line each in `mm2src/coins/eth/tron/mod.rs`, and ch.21, where this
branch adds an 8-line cross-reference paragraph to §21.10 while swap-support
rewrites §21.11 onward.

**Ordering, if both are revived: swap-support's ch.21 rescoping goes in first,
and this branch's §21.10 cross-reference paragraph is re-applied after it.** The
reverse order loses the paragraph in the rewrite. This note is repeated in the
swap-support plan, because two separate plans will not otherwise see it.

## Standing caveat

> **"the code of those three branches comes from a foreign PRs, that does not
> guarantee their correctness automatically. It should be checked and tested."**
> -- the maintainer, 2026-10-18

This governs every step above. A chapter that passed the Dirty Gate, a diff that
merges cleanly, and a module with 73 green unit tests are three different things,
and none of them is evidence that the code does the right thing on a funded
network. **A Dirty Gate score attests that a chapter is clean, never that the
behavior it describes is correct.**

Every "the reference does X" statement in ch.49 and in this plan is this
repository's June-2026 record of the reference, quoted from our own commit
messages and chapter text. None of it has been independently verified: the
GasFree provider API surface, the TIP-712 layout, the `CREATE2` rule, the
network constants and the lifecycle enumerations are all in that class. Treat
them as claims to confirm, not as facts.

## Open questions for a KDF Spec Reader

To be dispatched when the work is picked up, not before. Question 1 is the
blocking one.

1. **The GasFree proxy creation bytecode, per network** (`gasfree/config.rs:106`,
   currently `&[]`), **plus at least two published custody-address vectors per
   network to verify `derive_gasfree_address` against.** Without this the
   derivation is wrong and the feature cannot be made reachable.
   **Establish first whether these bytes are obtainable from the public GasFree
   SDK** -- if they are, this is a clean-channel fetch, not a Spec Reader task,
   and should not consume a dispatch.
2. **Is the reference's rail sign-only, or does it submit end to end?** Ch.49
   declares "sign-only today" with submission deferred as `D-submit`, while
   `5e0af6f80` says submit was ported "as real HTTP" and withdraw stayed
   sign-only. Establish what the reference actually does: does its withdraw
   submit the authorization to the provider and track on-chain settlement, what
   are the task progress phases (§49.3.6), and what are the terminal states?
   Without this, step 3 cannot be scoped.
3. **The exact wire contract of the fields this branch adds to shared,
   cross-coin structs** -- `fee_method` (its full enum; the branch has
   `gasless` / `auto` / native), `gasless` (`max_fee`, `deadline_seconds`,
   `fallback_to_native`), and `gasfree_address` on the balance response: field
   names, types, defaults, and whether each is present or omitted for a non-Tron
   coin. These are cross-coin RPC surfaces: if Reloaded's shapes differ from the
   reference, **every** EVM client sees it, not only Tron users.
4. **The `auto` rail's selection rule and the fallback semantics.** When
   `fee_method` is absent, and when `fallback_to_native` is set, what does the
   reference do -- in particular, does a failed gasless preflight fall back to a
   native-TRX withdraw **silently**, or fail? PR #25 chose to fail rather than
   substitute on the EVM fee path; the two should be consistent, and if the
   reference differs, that must be a recorded decision rather than an accident.
5. **The provider error taxonomy and its HTTP status mapping**, plus the
   provider's own lifecycle enumeration for a submitted authorization. Ch.49
   §49.9 binds a taxonomy; confirm it, because `WithdrawError::Gasless`
   delegates its `status_code()` to it and that becomes a **public** error
   contract.

## Governing CRD

- **Chapter 49 on the branch** (`docs/reloaded-rewrite/49-tron-gasfree-fee-delegation.md`
  at `76a3bbd81`), to be renumbered 57. Not on `dev`.
- [Chapter 21](../reloaded-rewrite/21-tron-integration.md) -- Tron integration;
  this branch adds a §21.10 cross-reference to ch.49.
- [Chapter 49 on `dev`](../reloaded-rewrite/49-withdrawal-task-path.md) -- the
  coin-generic withdraw request/response contract the gasless rail extends, and
  the reason for the renumbering.
