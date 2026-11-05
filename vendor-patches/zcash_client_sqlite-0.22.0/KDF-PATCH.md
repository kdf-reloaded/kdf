# KDF Reloaded patch

This directory is based on the published `zcash_client_sqlite` 0.22.0 crate
and retains its upstream package metadata (`Cargo.toml` declares
`license = "MIT OR Apache-2.0"`).

License files: the published crate ships no license files, although its
README refers to `LICENSE-APACHE` and `LICENSE-MIT`. The two texts placed
here are copies of the files of the sibling vendored `zcash_transparent`
0.10.0, which comes from the same upstream repository (librustzcash, Electric
Coin Company); they differ from the `zcash_primitives` 0.30.1 copies only in
the copyright year of the MIT text (2017-2024 here, 2017-2019 there). KDF
Reloaded relies on the MIT option when combining this crate with the
GPL-2.0-only work.

## The one change

`wallet::store_transaction_to_be_sent` marks the notes a sent transaction
spends. For an Orchard bundle and for an Ironwood bundle it did so under
`#[cfg(feature = "orchard")]` and, without the feature, called `panic!`. KDF
Reloaded builds the crate without that feature (the wallet is Sapling-only:
it holds no Orchard or Ironwood keys and notes), and a withdrawal to a Pirate1
address is a version-6 transaction with an Ironwood bundle (CRD chapter 39,
R39.6.5d and R39.6.5g). Recording it, so that its Sapling notes stop being
selected before it is mined, must not panic.

In the patched copy the two `panic!` calls are replaced by an empty block
under the same `#[cfg(not(feature = "orchard"))]`, with a comment: without the
feature the wallet holds no notes of those pools, so no nullifier can match.
The Sapling nullifiers are marked as before. Nothing else in the crate is
changed; with the feature enabled the code is identical to upstream.

Drop this patch when the published crate stops panicking without the feature.
Focused tests: `z_coin::z_note_lock_tests::pirate1_withdraw::t39_6_5e_*` and
`z_coin::z_ironwood_read_tests::t39_6_5a_the_broadcast_record_reads_the_mainnet_transactions`
in the `coins` crate.
