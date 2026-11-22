# Plan: legacy swaps read the chain correctly (spend input, secret, refund time, execution)

**Status:** planning — **Last reviewed:** 2026-11-22

**CRD:** ch.51 (dated status notes; the swap machines' searches and refund gate), ch.38 (UTXO and QRC20 search),
ch.18 (Tendermint extraction). Reloaded-original, clean side. The cleared CRD does not cover legacy-swap extraction,
spend search or refund gates for EVM, QRC20 or SLP. Questions about the reference's behavior there have gone to
the Spec Reader, and steps 2 to 4 wait for their gated answers.

> **Context.** A review of the legacy (v1) swap path on `dev` found four places where a swap reads the chain in a
> way that can miss or misjudge what happened:
>
> 1. a spend of a swap payment is classified by the transaction's first input, not by the input that spends the
>    payment;
> 2. on EVM, the secret is taken from a direct `receiverSpend` call only and is not checked against the swap's
>    secret hash (Tendermint does not check it either);
> 3. the refund of an SLP, QRC20 or EVM payment is judged against the node's clock, not the chain's;
> 4. a found QRC20 spend or refund is not checked for a failed contract execution.
>
> None of these changes a wire message.

## Steps

### 1. A spend is classified by the input that spends the payment (UTXO family)

Status: in progress (PR #NNN).

`search_for_swap_output_spend_of` and the every-server search read the spending transaction's first input to
decide between claim and refund. The spend lookup already returns the index of the input that spends the swap
output (`SpentOutputInfo::input_index`). Both paths will classify by that input. This covers every coin on the shared
UTXO search: standard UTXO coins, BCH, Qtum, SLP (which searches its own output index) and the non-lightwalletd
shielded path. The lightwalletd path already classifies by the spending input. The secret extraction scans every
input and checks the hash, so it is unchanged. The version-2 funding-spend search had the same flaw (it took the
swap output's index as the input index) and is fixed in the same change.

Tests: a spend whose first input is not the swap input, for a claim and a refund, through both searches; an index
that is out of range is an error, not a guess.

### 2. Secret extraction checks the hash on every family (EVM, Tendermint)

Status: waiting for the Spec Reader's gated answers (Q1, Q6).

EVM: the secret is checked against the swap's secret hash, using the contract's hash rule. A claim that is not a
direct `receiverSpend` call (for example one made through another contract) is read from the contract's
`ReceiverSpent` event, which carries the id and the secret. Tendermint: the claim message that matches the swap is
found among the transaction's messages, and the secret is checked against the hash. Tendermint swaps are off, so
this part has low priority.

### 3. The refund gate reads the chain's time (SLP, QRC20, EVM)

Status: waiting for the Spec Reader's gated answer (Q3).

- SLP: the refund is a BCH script spend, so its gate uses BCH's median time past, like the other UTXO coins.
- EVM and QRC20: the contract compares the lock time with the time of the block that includes the refund. The gate
  reads the latest block's timestamp from the chain.

The clock check that refuses a refund when the node's clock is far off stays as it is.

### 4. A found QRC20 spend or refund counts only when its contract call succeeded

Status: waiting for the Spec Reader's gated answer (Q4).

The search ties the swap event to the contract-call output it matches and refuses a call whose receipt says it
failed (`excepted`). EVM spends and refunds are found through the contract's events, and a failed execution emits
none, so the EVM search already meets this; a test pins it.

### 5. Lab rows

A1: a UTXO claim whose spend has an extra input first, if the lab can build one. A3: an EVM or QRC20 refund at the
lock time. A4: a QRC20 search over a failed call, if one can be produced.

## Deliberately not in this plan

- Durable swap records, refusing a newer swap database, and swap-lock fencing: a separate lane.
- Swap version two (its searches and extraction), apart from the funding-spend index fix in step 1.

## Decisions

- 2026-11-22, maintainer: fix the four chain reads in the legacy path, one PR per step or family.
