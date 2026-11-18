# Plan: ZEC transactions in the version-5 format (Zcash NU7)

**Status:** in progress (0.5.0; route (A) decided 2026-11-17; P1 format and digests PR #397 and P2 ZEC wiring PR #404 merged; P2b history report PR #415; P3 coins file open) — **Last reviewed:** 2026-11-18

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

*Amended 2026-11-17 — decided: route (A).* Two facts checked after this plan was written changed the recommendation:
the pinned `zcash_protocol` 0.10.6 has no NU7 branch id (`Nu7` exists only behind its unstable build option, with the
placeholder `0xFFFFFFFF`), so the pinned `zcash_primitives` can neither build nor read an NU7 transaction; and ZEC is a
transparent UTXO coin, so every ZEC build, signature and parse goes through the shared UTXO code, where (B) would need an
adapter at each site. `zcash_protocol` 0.11 and `zcash_primitives` 0.31 have NU7, but moving the Zcash family is a migration
of the ARRR path and of the three vendored Zcash crates. Route (A) extends the shared transaction type and signature hash
with the ZIP-225 layout and the ZIP-244 digests, pinned by the public ZIP-244 test vectors; the branch id stays a configured
value. A later upgrade of the Zcash library family is a separate item; once it is in, the library can serve as an independent
cross-check of the v5 serializer in tests.

## Work packages

| Id | What | State |
|---|---|---|
| Z1 | **Configuration.** Accept `txversion` 5 with the keys of the shared coins file (`version_group_id` and `consensus_branch_id`, each with or without a `0x` prefix); default the version group id to `0x26A7270A` for version 5 when it is absent; refuse `txversion` 6 or higher, and 5 without `overwintered`, at activation with a clear error. *(Amended 2026-11-17.)* | Done (P2, PR #404) |
| Z2 | **Withdrawal:** build and sign a version-5 transaction. | Done (P2, PR #404) |
| Z3 | **Swap path:** the HTLC payment, the spend and the refund, with P2SH inputs under the ZIP-244 digest. | Done (P2, PR #404) |
| Z4 | **Reading version-5 transactions** in the shared UTXO parser (route A; all three bundles of ZIP 225): history, verification of unspent outputs, validation of swap payments. | Done (P1 parser, P2 wiring); the history reports a transaction it cannot read, such as version 6 until the Zcash library upgrade (P2b, PR #415) |
| Z5 | **The ZIP-244 transaction id**, for the node's own transactions and for incoming ones with Sapling or Orchard parts. | Done (P1, PR #397) |
| Z6 | **Tests:** the public ZIP-244 test vectors (the zcash-test-vectors repository, MIT) and round trips. | Done (P1, PR #397) |
| Z7 | **Reference position:** a Spec Reader pass that records in the CRD how the reference implementation handles ZEC after NU7, before the implementation starts. | Done (CRD ch.39 §39.11.4V) |
| Z8 | **Lab:** a small mainnet withdrawal, then a ZEC/RIN swap between two KDF Reloaded nodes. | Done (lab rows of PR #404: withdrawal, swap, refund, all version 5 on mainnet) |
| Z9 | **Coins file:** `txversion` 5, the version group id and the branch id for ZEC in komodo-coins-rin, once the code ships. | Not started |

## Decisions (2026-11-17)

- Route (A); version 5 only (not 6); the parser reads all three ZIP-225 bundles and computes the full ZIP-244 transaction id.
- Expiry height stays 0 unless the reference answer differs; the fee rule is unchanged (the fee is computed per
  transaction).
- `required_confirmations` for ZEC stays 3; the 25-second spacing gets test cases only.
- No guard against a coins file that switches to version 5 before or after the release: a build without version-5 support
  activates ZEC and shows balances, and every send is refused by the network, no funds move (lab row of 2026-11-16 and
  2026-11-17 on the 0.4.0-beta.1 release and on dev).
- Pull requests: P1 format and digests (Z4 parser, Z5, Z6); P2 ZEC wiring (Z1–Z3, Z4 wiring) with a mainnet withdrawal and a
  ZEC/RIN swap between two KDF Reloaded nodes as READY conditions; P2b the history report of unreadable transactions
  (decision D39.11.4V.1 (b) of 2026-11-18); P3 the coins file (Z9).

## Open points

- Fees under ZIP 317, if the lab shows a refusal.
