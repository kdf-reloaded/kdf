# Plan: Tron V1 atomic-swap participation

**Status:** parked (branch-only; its ch.21 rewrite must pass the KDF Dirty Gate first) — **Last reviewed:** 2026-10-18

> **Context:** written 2026-10-18, when a review of three stale June-2026
> branches found a fully wired Tron V1 swap implementation and a 568-line
> rewrite of CRD ch.21 living only on `origin/feat/tron-swap-support`. The
> maintainer decided to park the branch: it is neither to be merged nor
> deleted. This plan, not a dangling ref, is what keeps the work findable. Read
> the blocker below before anything else -- it is a precondition on the whole
> branch, not a step that can run in parallel with the rest.

## BLOCKER: the ch.21 swap chapter never passed the KDF Dirty Gate

> ### Hard precondition on any revival of this branch
>
> **Commit `ab87a0c2d` -- the commit that carries the entire 568-line Tron V1
> atomic-swap rewrite of `docs/reloaded-rewrite/21-tron-integration.md` --
> records NO KDF Dirty Gate verdict.**
>
> The only gate verdict anywhere on this branch is in `79ea5e8fb`, and it covers
> **only** the small R-AD2 order-display-address addendum: *"Gated PASS (score
> 0.06)."* That is a few paragraphs about rendering an address in base58check.
> It is **not** a verdict on the swap chapter.
>
> Meanwhile `ab87a0c2d`'s own provenance footer asserts *"Forbidden corpus: not
> consulted"* -- while the same chapter binds Tron-specific divergences for
> which **no clean public derivation could be found from inside this
> repository**: the `bytes32` / SHA-256 secret-hash variant of the deployed
> HTLC contract (R-SA1), its per-network deployment, and the
> indexed-contract-event discovery surface (R-L6).
>
> **Therefore: the chapter must pass the KDF Dirty Gate before one line of this
> branch is reused, and before any `kdf-coder` prompt is written from it.** Per
> `AGENTS.md` §2, a Spec Reader change "is not ready to cross the wall until
> this gate passes." An unverified "corpus not consulted" claim on a chapter
> binding corpus-shaped facts is the single item on these three June branches
> that should not be taken at face value.
>
> **If the gate fails, the chapter must be re-derived by `kdf-spec-reader` --
> not patched.** Patching a failed chapter by hand is exactly the move the
> clean-room wall exists to prevent, and a tainted orchestrator may not do it
> at all.

### What *is* legitimately baseline-derived, so the gate has a narrower target

The "not consulted" claim is partly plausible, and recording the split here
saves the gate from re-deciding it:

- **Clean, pre-existing project facts.** The method names `ethPayment`,
  `erc20Payment`, `receiverSpend` and `senderRefund`, and
  `etomic_swap_id(time_lock, secret_hash)`. `etomic_swap_id` is on `dev` today
  at `mm2src/coins/eth/eth_swap_ops.rs:36` (and `:61`, `:305`), and the method
  names also appear across `mm2src/coins/qrc20/*` and `mm2src/coins/eth/*`.
- **Not clean, and the actual target.** The Tron-specific divergences: the
  `bytes32` / SHA-256 secret-hash variant of the deployed contract, its
  per-network deployed addresses, and the indexed-contract-event discovery
  surface. These are the parts that look corpus-shaped and have no public
  derivation in our tree.

Dispatch the gate over the whole chapter regardless -- the split above is a
hypothesis about where the findings will land, not a license to gate only part
of it.

## Where the work lives

Branch `origin/feat/tron-swap-support`, three commits, 2026-06-19 to 2026-06-26,
`+3670 / -205` across 20 files. Merge base with `dev`:
`039dd3c290db0097d44e12b1b0a20c295e6011a1` (2026-06-25) -- note this is one
commit **earlier** than the base of the other two June branches, so this one
carries 499 commits of `dev` drift rather than 498.

| SHA | Date | Subject |
|---|---|---|
| `ab87a0c2d51a55a8fb65d38c6b7a4c8efe66fc9a` | 2026-06-19 | `crd(21): bind Tron V1 atomic-swap participation (feat/tron-swap-support)` |
| `dc7af3bf540ec47edffca6c2b2cf12520f0cdaab` | 2026-06-26 | `feat(tron): implement Tron V1 atomic-swap participation (ch.21)` |
| `79ea5e8fba7555f849d3a6721f2a065bd7871397` | 2026-06-26 | `crd(21): bind Tron order-display address derivation (R-AD2)` |

**Do not delete the branch.** `dev`'s
[ch.21](../reloaded-rewrite/21-tron-integration.md) still reads "wallet-only and
withdraw-capable today", with swap participation sitting in §21.11 Deferred Work
as D1/D2 under the `R-D1` gate invariant. The chapter rewrite and the pure
interop layer exist nowhere else.

## What is on the branch

- **`mm2src/coins/eth/tron/swap.rs`** -- new, 1214 lines, 22 tests. The pure,
  offline-testable dictated interop: the `bytes32` / SHA-256 HTLC contract ABI
  as an embedded JSON constant `TRON_SWAP_CONTRACT_ABI`, the swap-id derivation,
  payment / spend / refund call encoding and validation, contract-event
  normalization, DEX-fee transfer encode and validate, and the TRC20
  approve-to-zero-first allowance quirk. Its header comment states it does no
  node I/O, so it is fully unit-testable offline.
- **`mm2src/coins/eth/tron/swap_ops.rs`** -- new, 1055 lines, 6 tests. The async
  on-chain flows: `send_payment`, `spend_payment`, `refund_payment`,
  `validate_payment`, the trade-fee estimators over `getchainparameters` plus
  `getaccountresource` energy and bandwidth math, spend and refund discovery
  through a TronGrid-style contract-events indexer with fingerprint paging and
  `gettransactioninfobyid` confirmation, and the `wait_for_tx_spend` /
  search-for-swap-tx-spend polling.
- `tron/fee.rs` `+121` with 12 tests; `tron/activation.rs` `+111`, which now
  parses `swap_contract_address` -- something `dev`'s `activation.rs`
  **deliberately does not** (its header comment: "TRON activation deliberately
  does not parse `swap_contract_address` or V2 contracts because TRON-side
  atomic swaps are still pending"); `tron/api.rs` `+191` (the contract-events
  query); `tron/tx_builder.rs` `+40`; `tron/mod.rs` `+23`.
- **Real, reachable `SwapOps` dispatch** in `mm2src/coins/eth/eth_swap_ops.rs`
  (`+270`): `is_tron_family(coin_type)`, `tron_bytes32(label, bytes)`,
  `EthCoin::tron_swap_contract()`, `validate_tron_payment`, `send_tron_payment`,
  `spend_tron_payment`, `refund_tron_payment` -- plus `eth_market_ops.rs`
  (`+64`), `eth_mm_coin.rs` (`+66`), `eth_impl.rs` (`+57`), and typed refusals in
  `eth_swap_v2/eth_maker_swap_v2.rs` and `eth_taker_swap_v2.rs`.
- `TransactionEnum::TronTx(crate::eth::tron::SignedTronTx)` in
  `mm2src/coins/lp_coins_types.rs`, with its `From` impl and `Deref` arm.
- **Changes to the shared V1 swap drivers** -- see the next section, which is
  where they must go.

40 tests in all. Unlike the GasFree branch, this one **is fully wired and
reachable**: a Tron coin activated with a swap-contract address becomes
swap-capable, gated only by `tron_swap_contract()` erroring on a zero address.
That makes it a new live funds path, and it is the reason this branch must not
land near a beta: ch.21 T6 is feature-gated testnet work and **no commit message
on the branch claims any end-to-end run was ever performed** -- `dc7af3bf5`
records no test run at all.

### The chapter

The 568-line ch.21 rewrite is thorough and canonical: rescoped one-sentence
claim, rescoped executive summary, updated §21.1 epoch table, §21.12 and §21.14
binding-scope notes distinguishing dictated ABI from discretionary Rust shape,
T1-T6, §21.18 deferrals, V4/V5, §21.20 external references and a rewritten
§21.21 provenance footer. It also updates
[ch.30](../reloaded-rewrite/30-provenance-attribution.md) -- the ch.21 row of
the chapter index and the external-specification list.

| Branch §21.x | Binds |
|---|---|
| R-P2 | swap dispatch through the EVM-family facade by chain family |
| R-T5 | the `TransactionEnum` Tron case |
| R-A3 | swap-contract binding from `swap_contract_address` config |
| R-AD2 | base58check order-display address derivation |
| §21.11 R-S1, R-S2 | chain-family dispatch; **SHA-256 32-byte secret hash** |
| §21.12 R-SA1, R-SA2 | the contract ABI table; swap id = `SHA-256(LE u32 lockTime ‖ secretHash)` |
| §21.13 R-L1-R-L7 | payment send / validate / spend / refund / secret extraction / discovery / confirmation |
| §21.14 R-AP1 | TRC20 approve-to-zero-first and non-boolean-return tolerance |
| §21.15 R-DF1, R-DF2 | DEX taker-fee send and validation |
| §21.16 R-TF1 | trade-fee estimators over the bandwidth and energy model |
| §21.17 T1-T6 | the verification surface (T6 = feature-gated testnet end-to-end) |
| §21.18 D1-D3, R-D1 | rescoped deferrals: V2 swaps, watcher-reward paths, NFT withdraw |
| §21.19 V4, V5 | baseline-absence verifications for the swap feature and the ABI |

The chapter edits ch.21 **in place**. Unlike the GasFree chapter, it needs no
renumbering.

## Discard the shared-driver change: `dev` already solved it, and solved it better

The branch adds a default trait method
`SwapOps::swap_secret_hash(&self, secret: &[u8]) -> Vec<u8>`
(`mm2src/coins/lp_coins_traits.rs:103`) and calls it from
`mm2src/mm2_main/src/lp_swap/{maker_swap.rs,taker_swap.rs,recreate_swap_data.rs}`,
together with an `H160Json` to `BytesJson` widening of
`MakerSwapData::secret_hash` and `MakerNegotiationData::secret_hash`.

`dev` landed both halves independently in commit **`bfa0e41b3`** (2026-08-19,
*"fix(swap): select the secret-hash algorithm the coin pair actually needs"*),
bound as **CRD ch.51 R71 / R72 / R72A / R72B**
([`51-legacy-v1-swap-state-machine.md`](../reloaded-rewrite/51-legacy-v1-swap-state-machine.md),
from line 903) with
`select_secret_hash_algo(maker_coin, taker_coin)` at
`mm2src/mm2_main/src/lp_swap.rs:938`.

**The branch's approach is not merely redundant -- it is unsound for a
cross-family pairing.** `dev` selects on the **coin pair**, once, when the
secret is first chosen, then persists the result and reads it everywhere (with a
`dhash160` fallback only when resuming a swap persisted before the field
existed). The branch selects **per coin**, at each call site, using whichever
coin happens to be local to that site: `MakerSwap::get_my_negotiation_data`
sends `self.maker_coin.swap_secret_hash(...)` while the taker-payment spec at
`mm2src/mm2_main/src/lp_swap/maker_swap.rs:822` uses
`self.taker_coin.swap_secret_hash(...)`. With Tron (32-byte SHA-256) on one side
and a UTXO coin (20-byte `dhash160`) on the other, the two legs disagree about
the hash width -- **a stuck cross-family swap with funds locked**. That is
precisely the failure `bfa0e41b3` describes diagnosing from a live SC/RIN taker
swap:

```
taker_swap:1477] [chk-payment] arg parse failed: secret_hash parse failed:
Hash256::TryFrom<&[u8]> invalid slice length: expected 32 byte slice, found [20 bytes]
```

**The right Tron change is a one-arm extension of `dev`'s design**, not the
branch's. The inner helper `commits_to_a_native_32_byte_hash` at
`lp_swap.rs:939-946` currently matches
`MmCoinEnum::SiaCoin | MmCoinEnum::TendermintCoin | MmCoinEnum::TendermintToken`
plus `MmCoinEnum::LightningCoin` on native targets. **The wrinkle: Tron lives
inside `MmCoinEnum::EthCoin`**, so the new arm cannot match a top-level variant
-- it must inspect `EthCoinType::Tron | EthCoinType::Trc20 { .. }` on the coin
inside. And `dev`'s ch.51 R71/R72 does **not** name Tron, so the CRD needs the
Tron arm bound as well as the code.

Wire note, verified: the `H160Json` to `BytesJson` widening is JSON-compatible
for 20-byte values. `mm2src/kdf_rpc_types/src/v1/types/bytes.rs:50` serializes as
lowercase hex with no `0x` prefix, and the `json_hash!` macro in
`mm2src/kdf_rpc_types/src/v1/types/hash.rs` does the same; `H160`'s
deserializer requires exactly 40 hex characters while `Bytes`' accepts any even
length. Reading existing persisted swaps therefore works and writing a 20-byte
value is byte-identical. This holds for `dev`'s version as much as the
branch's -- it is not a reason to prefer the branch's.

## Revive as a rewrite from the gated chapter, not as a rebase

**A rebase produces something that neither compiles nor matches the architecture
`dev` settled on.** Two independent reasons, neither of which conflict
resolution surfaces:

1. **The event-decoding path has no landing place.**
   `mm2src/coins/eth/tron/swap.rs:15` imports `ethabi::{Contract, Event, RawLog,
   Token}` and the file further uses `ethabi::Function` (line 208),
   `ethabi::Log` (line 707) and `ethabi::encode` (lines 867, 1172, 1186, 1208).
   **`ethabi` is no longer a dependency of `mm2src/coins`**: `mm2src/coins/Cargo.toml`
   carries only the comment "alloy ABI codec (migration target, replacing
   ethabi)" above `alloy-dyn-abi` / `alloy-json-abi` / `alloy-primitives`
   `= "1.5"`. `dev` replaced it with `mm2src/coins/eth/abi.rs`, an alloy-backed
   facade whose entire public surface is `Token` (line 27), `AbiError` (43),
   `Function` (174), `Contract` (225), `Event` (277) and `encode` (289) --
   **there is no `RawLog`, no `Log` and no `parse_log`.**
2. **The shared-driver change must be discarded**, per the section above, in
   favor of `dev`'s ch.51 R71/R72 mechanism.

`git merge-tree --write-tree --name-only origin/dev origin/feat/tron-swap-support`
reports five conflicts, which understate the problem by a wide margin:

```
docs/reloaded-rewrite/30-provenance-attribution.md    (trivial: one table row, one list line)
mm2src/coins/eth/eth_market_ops.rs
mm2src/coins/eth/eth_mm_coin.rs
mm2src/mm2_main/src/lp_swap/maker_swap.rs
mm2src/mm2_main/src/lp_swap/taker_swap.rs
```

**Keep the branch's test vectors as fixtures.** `swap.rs`'s 22 offline tests are
reusable even when the code around them is rewritten, and so is the design
recorded in `swap_ops.rs`: Tron exposes no Ethereum-style log filtering, so
spend and refund discovery is **indexer**-based with fingerprint paging, with
`gettransactioninfobyid` used only to confirm an already-known transaction. That
is structurally different from every other EVM coin in the tree and is recorded
nowhere else. Its R-L6 requirement -- that a missing event endpoint yield a
**typed failure** rather than a silent "not found" -- is the safety-relevant
half of it: the alternative is a swap that believes it was never spent and
refunds over a completed leg.

## Revival sequence

In this order. Step 1 is a precondition, not a parallel task.

1. **Gate `ab87a0c2d`'s chapter through `kdf-dirty-gate`.** If it fails,
   re-derive via `kdf-spec-reader` before anything else.
2. **Migrate the gate verdict into the chapter text.** Once the chapter passes,
   record the clearance inline, the way recent `dev` chapters do -- see
   [ch.47](../reloaded-rewrite/47-metamask-integration.md) (lines 359 and 877)
   and [ch.55](../reloaded-rewrite/55-coin-activation-wire-contract.md) (line
   1387). `79ea5e8fb`'s `0.06` for the R-AD2 addendum currently exists **only in
   a commit message**, and a squash merge would destroy it; fold it in at the
   same time.
3. **Bind the Tron arm of the secret-hash selection** in ch.51 (R71/R72 name
   only Sia, Lightning and the Tendermint family today) and in ch.21 R-S2,
   consistent with `dev`'s pair-based design rather than the branch's per-coin
   trait method.
4. **Decide the `abi.rs` question**: extend the facade with log parsing, or
   rewrite the event decoding against alloy directly -- and re-prove the
   encodings against the frozen calldata vectors in
   `mm2src/coins/eth/abi_golden_tests.rs` either way.
5. **Hand the gated chapter to `kdf-coder`** to reimplement against today's
   `dev`, reusing the branch's test vectors as fixtures. **Do not paste branch
   code into a Coder prompt if the gate verdict is anything other than a clean
   pass.**
6. **Do not land it near a beta.** Tron swaps are a new live funds path for a
   coin family with no end-to-end verification recorded anywhere on the branch.
   Require the ch.21 T6 testnet end-to-end run before release, not after.

## If both Tron branches are revived

The two Tron branches -- this one and [`tron-gasfree`](./tron-gasfree.md) -- are
**independent of each other** and either can be revived alone. They overlap in
exactly two places: one `pub mod` line each in `mm2src/coins/eth/tron/mod.rs`,
and ch.21, where gasfree adds an 8-line cross-reference paragraph to §21.10
while this branch rewrites §21.11 onward.

**Ordering, if both are revived: this branch's ch.21 rescoping goes in first,
and gasfree's §21.10 cross-reference paragraph is re-applied after it.** The
reverse order loses the paragraph in the rewrite. This note is repeated in the
gasfree plan, because two separate plans will not otherwise see it.

## Standing caveat

> **"the code of those three branches comes from a foreign PRs, that does not
> guarantee their correctness automatically. It should be checked and tested."**
> -- the maintainer, 2026-10-18

This governs every step above, and it bites hardest here, because this branch is
the one that is already reachable. **A Dirty Gate score attests that a chapter is
clean, never that the behavior it describes is correct** -- so even a clean pass
on step 1 leaves the whole of the maintainer's caveat outstanding.

Every "the reference does X" statement in the branch's ch.21 and in this plan is
this repository's June-2026 record of the reference, quoted from our own commit
messages and chapter text, **independently verified nowhere**. Specifically in
that class: the Tron HTLC contract ABI, the SHA-256 secret-hash divergence, the
swap-id derivation, the event-indexer discovery contract and the USDT approval
quirk. Those are also exactly the items behind the gate blocker, which is not a
coincidence.

## Open questions for a KDF Spec Reader

To be dispatched when the work is picked up. **Question 1 is the gate-blocking
question and must be answered before anything else on this branch proceeds.**

1. **The Tron-deployed V1 HTLC contract: is the `bytes32` / SHA-256 variant
   published, and where?** The central provenance question. Establish whether
   the Tron build's ABI, its deployed per-network addresses and the SHA-256
   choice are **publicly published** (contract source, verified deployment, SDK)
   or recoverable only from the corpus. **The answer decides whether the chapter
   can stand as written or must be re-derived -- and the chapter must go through
   the Dirty Gate either way, which it never did.**
2. **The deployed Tron swap-contract addresses per network**, and whether they
   are operator-configured -- as the branch's R-A3 assumes, reusing the EVM
   family's existing `swap_contract_address` parameter -- or **pinned per
   network** at the reference. If the reference pins them, R-A3 is a divergence
   and must be recorded as one.
3. **How does the reference select the secret-hash algorithm for a Tron
   pairing?** Given that `dev` has independently settled on pair-based selection
   (ch.51 R71/R72, `lp_swap.rs:938`): does the reference key the choice on the
   **coin pair** or on the **local coin**; is the negotiated `secret_hash` ever
   renegotiated per leg; and for a Tron-to-UTXO pairing, which width goes on the
   wire and what does the narrower leg derive from it (ch.51 R72A's question,
   applied to Tron)? **This is the one place where getting it wrong produces a
   stuck cross-family swap with funds locked**, so this dispatch should
   explicitly request correctness analysis of the reference code in this area,
   not merely a chapter-matches-code confirmation.
4. **The event-discovery contract.** Tron exposes no Ethereum-style log
   filtering, so the branch uses a TronGrid-style indexed-event query with
   fingerprint paging. Establish the reference's endpoint, query parameters,
   paging and ordering semantics, and -- importantly -- its behavior when **no**
   event-capable endpoint is configured. R-L6 requires a typed failure rather
   than a silent "not found"; confirm the reference agrees, because the
   alternative is a swap that believes it was never spent and refunds over a
   completed leg.
5. **Watcher-path status for Tron.** The branch's §21.18 D2 asserts that Tron V1
   swaps do **not** support watcher-reward variants and refuse them with a typed
   error. Confirm: a watcher that accepts a Tron swap it cannot resolve is a
   funds hazard, and this interacts with the watcher-reward chapter (ch.09).
6. **The TRC20 approve-to-zero-first sequence and the safe-transfer wrapper**
   (R-AP1): confirm the exact allowance sequence, which token behaviors trigger
   it, and whether the deployed Tron swap contract's transfer wrapper tolerates
   non-boolean-returning TRC20s (mainnet USDT). The branch states this as
   dictated by the token contracts; a wrong allowance sequence means a payment
   that cannot be locked.
7. **The swap-id derivation for Tron.** R-SA2 binds
   `SHA-256( little-endian uint32(lockTime) ‖ secretHash )`. Confirm whether this
   is identical to the project's existing `etomic_swap_id`
   (`mm2src/coins/eth/eth_swap_ops.rs:36`) applied over a 32-byte hash, or a
   Tron-specific variant. If identical, the fact is baseline-derived and clean;
   if not, it falls inside question 1's scope.

## Governing CRD

- [Chapter 21](../reloaded-rewrite/21-tron-integration.md) -- Tron integration.
  `dev`'s version is wallet-and-withdraw-only, with swap participation deferred
  in §21.11 as D1/D2 under `R-D1`. The branch's 568-line rewrite at
  `ab87a0c2d` / `79ea5e8fb` replaces it in place, and is the artifact the Dirty
  Gate blocker is about.
- [Chapter 51](../reloaded-rewrite/51-legacy-v1-swap-state-machine.md) -- R71,
  R72, R72A, R72B: `dev`'s pair-based secret-hash selection, which supersedes the
  branch's shared-driver change and needs a Tron arm bound.
- [Chapter 30](../reloaded-rewrite/30-provenance-attribution.md) -- the chapter
  index row and external-specification list the branch also updates.
- [Chapter 09](../reloaded-rewrite/09-watcher-reward-infrastructure.md) -- watcher rewards,
  which §21.18 D2 refuses for Tron (Spec Reader question 5).
