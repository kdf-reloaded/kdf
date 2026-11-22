# Plan: legacy swaps read the chain correctly

**Status:** planning — **Last reviewed:** 2026-11-22

**CRD:** ch.51 (dated status notes; the reference record of the coin families is §51.6E). Reloaded-original,
clean side.

Hardening area: the correctness of the chain reads the legacy (v1) swap path makes. Four steps, one PR each.

## Steps

1. A found spend of a UTXO swap payment is read from the input that spends the payment — PR #481.
2. The secret read from the transaction that claimed a payment is accepted only when it matches the swap's secret
   hash, for every coin family. EVM coins also read it from the contract's claim event; Tendermint coins read
   every message of the transaction — PR #488.
3. The refund of an SLP, QRC20, ETH or ERC20 swap payment is judged by the chain's time: the platform coin's median
   time past for SLP and QRC20 tokens, the latest block's timestamp for ETH and ERC20 — PR #490.
4. A QRC20 spend or refund counts only when its contract call succeeded and its event belongs to that call; refunds
   are looked for in the history of the payment's sender — PR #496.

## Decisions

- 2026-11-22, maintainer: harden the legacy swap path's chain reads, one PR per step or family.
