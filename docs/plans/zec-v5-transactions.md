# Plan: ZEC transactions in the version-5 format (Zcash NU7)

**Status:** planning (post-beta; nothing is implemented) — **Last reviewed:** 2026-11-12

> **Context.** The Zcash network upgrade NU7 activated on mainnet at height 3,543,405 (ZIP 259,
> consensus branch id `0x77190AD9`). From NU7 on, a transaction's version must be 5 or 6, also for a
> transaction with only transparent inputs and outputs (ZIP 2003; ZIP 259). Version 5 is the layout of
> ZIP 225; version 6 is that of ZIP 229. NU7 also changes the block spacing to 25 seconds
> (ZIP 218).
>
> KDF Reloaded builds ZEC transactions with the shared UTXO transaction type
> (`mm2src/kdf_chain/src/transaction.rs`), which writes only the Overwinter and Sapling layouts of
> versions 3 and 4, and signs them with the ZIP-243 signature digest under the configured
> `consensus_branch_id` (`mm2src/kdf_script/src/sign.rs`, `sighash_overwintered`). The network
> therefore rejects every ZEC withdrawal, swap payment and refund with "transaction version 4 not
> supported by the network upgrade Nu7". The coin configuration
> (`mm2src/coins/utxo/utxo_builder/utxo_conf_builder.rs`) knows default version group ids and
> branch ids only for versions 3 and 4, and accepts `txversion` 5 without an error, which would give
> a body in the version-4 layout. CRD ch.39 R39.11.15 records that the shared parser has neither the
> version-5 nor the version-6 layout and computes transaction ids by double SHA-256, while the Zcash
> library reader in the tree (`zcash_primitives` 0.30.1, which the ARRR code uses) reads versions 3
> to 5 with the identifier rule of each version.
>
> The coins file (komodo-coins-rin) lists ZEC with `txversion` 4, `version_group_id` `0x892f2085`
> and `consensus_branch_id` `0x37a5165b`. The other Zcash-family coins of the coins file (KMD, ZER,
> BTCZ, FLUX) run their own networks; NU7 is an upgrade of the Zcash network only.
>
> 0.4.0-beta.1 records the limitation in the CHANGELOG (Known limitations); the work below follows
> after the beta.

## What a version-5 send needs

- The ZIP-225 layout: a header with the overwintered bit and version 5, the version group id
  `0x26A7270A`, the consensus branch id, the lock time and the expiry height, then the transparent
  bundle, and the Sapling and Orchard bundles, each with a count of 0.
- The ZIP-244 transaction id and signature digest. For transparent inputs the signature digest
  commits to the amounts and the scriptPubKeys of all inputs of the transaction, not only of the
  input being signed.
- Reading incoming version-5 transactions, for the history, the verification of unspent outputs and
  the validation of a counterparty's swap payment.
- The transaction id the node reports for its own transactions must be the ZIP-244 id.

## Options

- **(A) Extend the shared UTXO transaction type** with the version-5 layout and the ZIP-244 digests,
  in our own code.
- **(B) Build, sign and read version-5 transactions through `zcash_primitives`**, already in the
  tree (MIT or Apache-2.0, compatible with the project's license), for coins configured with
  `txversion` 5: a transparent-only bundle. The pinned `zcash_protocol` 0.10.6 knows NU7 only behind
  its unstable `nu7` build option, with a placeholder branch id and no activation height, so the
  NU7 branch id has to be supplied, or the crate updated.

CRD ch.39 R39.11.16a chose the Zcash library reader, not an extension of the shared parser, for the
swap path of the shielded coins; (B) follows the same route for ZEC.

**Recommendation:** (B); (A) only where (B) cannot serve the swap scripts.

## Work packages

| Id | What | State |
|---|---|---|
| Z1 | **Configuration.** Accept `txversion` 5, with the version group id `0x26A7270A` and the NU7 branch id as defaults; refuse an unsupported `txversion` at activation with a clear error. | Not started |
| Z2 | **Withdrawal:** build and sign a version-5 transaction. | Not started |
| Z3 | **Swap path:** the HTLC payment, the spend and the refund, with P2SH inputs under the ZIP-244 digest. | Not started |
| Z4 | **Reading version-5 transactions** through the Zcash reader: history, verification of unspent outputs, validation of swap payments. | Not started |
| Z5 | **The ZIP-244 transaction id** for the node's own transactions. | Not started |
| Z6 | **Tests:** the public ZIP-244 test vectors (the zcash-test-vectors repository, MIT) and round trips. | Not started |
| Z7 | **Reference position:** a Spec Reader pass that records in the CRD how the reference implementation handles ZEC after NU7, before the implementation starts. | Not started |
| Z8 | **Lab:** a small mainnet withdrawal, then a ZEC/RIN swap between two KDF Reloaded nodes. | Not started |
| Z9 | **Coins file:** `txversion` 5, the version group id and the branch id for ZEC in komodo-coins-rin, once the code ships. | Not started |

## Open points

- Whether ZEC is marked `wallet_only` in the coins file until the fix (the maintainer's decision).
- Fees under ZIP 317.
- What the 25-second block spacing means for ZEC's confirmation settings and swap lock times.
