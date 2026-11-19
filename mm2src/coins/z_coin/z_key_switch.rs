//! Checks an activation of a shielded coin against the swaps that are not finished (CRD ch.39 R39.8.0bd
//! items 5 and 6).
//!
//! The swap storage belongs to the swap layer, which this crate cannot see. The swap layer registers an
//! [`UnfinishedSwapsView`] at start (see [`crate::register_unfinished_swaps_view`]); the decisions are made
//! here, next to the derivation of the per-swap HTLC key.

use async_trait::async_trait;
use primitives::hash::H264;

/// What an activation needs to know of one saved swap that is not finished and has a leg in the coin being
/// activated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnfinishedSwapLeg {
    /// The swap's uuid in its usual hyphenated text form.
    pub uuid: String,
    /// The HTLC public key the swap recorded for this coin; `None` when it recorded none.
    pub recorded_htlc_pubkey: Option<H264>,
    /// The input the per-swap HTLC key is derived from (R39.5.7): the maker's secret-hash bytes, or the taker's
    /// raw uuid bytes.
    pub swap_unique_input: Vec<u8>,
    /// The public half of the private key the swap saved for this leg (a swap an earlier build started with a
    /// random key); `None` when it saved none. Such a key counts only when it is the recorded key, as the
    /// kick-start treats it.
    pub saved_privkey_pubkey: Option<H264>,
    /// Whether every payment or fee this swap has to send in this coin is recorded as sent. `true` when the
    /// coin is the one the swap receives, which it only spends from.
    pub own_payments_sent: bool,
}

/// The saved V1 swaps that are not finished, as the swap layer reads them.
#[async_trait]
pub trait UnfinishedSwapsView: Send + Sync {
    /// The unfinished V1 swaps that have a leg in `ticker`, read from the saved swap storage (not only the
    /// swaps that are running).
    ///
    /// # Errors
    ///
    /// Returns the reason when a saved swap cannot be read or has no readable first event: an activation is
    /// then refused rather than allowed on a partial picture.
    async fn unfinished_v1_swaps_with_coin(&self, ticker: &str) -> Result<Vec<UnfinishedSwapLeg>, String>;
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::check_activation_against_unfinished_swaps;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{UnfinishedSwapLeg, UnfinishedSwapsView};
    use crate::z_coin::z_coin_wallet_db::unreadable_wallet_files;
    use crate::z_coin::{derive_swap_htlc_key_pair, ZCoinBuildError};
    use crate::CoinsContext;
    use common::log;
    use crypto::privkey::key_pair_from_secret;
    use mm2_core::mm_ctx::MmArc;
    use mm2_err_handle::prelude::*;
    use std::path::{Path, PathBuf};

    /// The UUIDs of the legs whose swap cannot use the HTLC key derived from `base_key` (R39.8.0bd item 5):
    /// a recorded public key that the re-derived key does not give and that no saved private key gives
    /// either (a saved key that is not the recorded one counts as none), or a key that cannot be
    /// re-derived at all.
    fn swaps_started_with_another_key(base_key: &keys::KeyPair, legs: &[UnfinishedSwapLeg]) -> Vec<String> {
        legs.iter()
            .filter(|leg| {
                let Some(recorded) = leg.recorded_htlc_pubkey else {
                    return false;
                };
                if leg.saved_privkey_pubkey == Some(recorded) {
                    return false;
                }
                match derive_swap_htlc_key_pair(base_key, &leg.swap_unique_input) {
                    Ok(derived) => derived.public_slice() != recorded.as_slice(),
                    Err(_) => true,
                }
            })
            .map(|leg| leg.uuid.clone())
            .collect()
    }

    fn join_uuids(uuids: &[String]) -> String { uuids.join(", ") }

    /// Refuses the activation of `ticker` with the transparent key `transparent_secret` when (item 5) swaps
    /// that are not finished were started with another key, or (item 6) a wallet database of the coin in
    /// `db_dir` cannot be read while a swap has not sent its own payment or fee. It runs before any wallet
    /// file is opened, so a refusal opens and creates none. A context that has no swap view registered has
    /// no swap storage to check.
    ///
    /// # Errors
    ///
    /// Returns [`ZCoinBuildError::KeyBlockedByUnfinishedSwaps`],
    /// [`ZCoinBuildError::WalletUnreadableWithUnsentSwaps`] or, when the saved swaps cannot be read,
    /// [`ZCoinBuildError::UnfinishedSwapsUnreadable`].
    pub(crate) async fn check_activation_against_unfinished_swaps(
        ctx: &MmArc,
        ticker: &str,
        transparent_secret: &[u8],
        db_dir: &Path,
    ) -> Result<(), MmError<ZCoinBuildError>> {
        let coins_ctx = CoinsContext::from_ctx(ctx).map_to_mm(|e| ZCoinBuildError::UnfinishedSwapsUnreadable {
            ticker: ticker.to_owned(),
            reason: e,
        })?;
        let Some(view) = coins_ctx.unfinished_swaps_view() else {
            log::warn!(
                "{}: no reader of the saved swaps is registered in this context; the key of the activation is \
                 not checked against unfinished swaps",
                ticker
            );
            return Ok(());
        };
        check_with_view(
            view.as_ref(),
            ticker,
            transparent_secret,
            db_dir,
            unreadable_wallet_files,
        )
        .await
    }

    /// [`check_activation_against_unfinished_swaps`] with the swap view and the probe of the wallet files
    /// given.
    pub(super) async fn check_with_view(
        view: &dyn UnfinishedSwapsView,
        ticker: &str,
        transparent_secret: &[u8],
        db_dir: &Path,
        unreadable_files: impl Fn(&Path, &str) -> Vec<(PathBuf, String)>,
    ) -> Result<(), MmError<ZCoinBuildError>> {
        let legs = view.unfinished_v1_swaps_with_coin(ticker).await.map_to_mm(|reason| {
            ZCoinBuildError::UnfinishedSwapsUnreadable {
                ticker: ticker.to_owned(),
                reason,
            }
        })?;
        if legs.is_empty() {
            return Ok(());
        }

        let base_key =
            key_pair_from_secret(transparent_secret).mm_err(|e| ZCoinBuildError::HdDerivationError(e.to_string()))?;
        let blocked = swaps_started_with_another_key(&base_key, &legs);
        if !blocked.is_empty() {
            return MmError::err(ZCoinBuildError::KeyBlockedByUnfinishedSwaps {
                ticker: ticker.to_owned(),
                swaps: join_uuids(&blocked),
            });
        }

        let unsent: Vec<String> = legs
            .iter()
            .filter(|leg| !leg.own_payments_sent)
            .map(|leg| leg.uuid.clone())
            .collect();
        if unsent.is_empty() {
            return Ok(());
        }
        let unreadable = unreadable_files(db_dir, ticker);
        if unreadable.is_empty() {
            return Ok(());
        }
        MmError::err(ZCoinBuildError::WalletUnreadableWithUnsentSwaps {
            ticker: ticker.to_owned(),
            files: unreadable
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            swaps: join_uuids(&unsent),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use common::block_on;
        use primitives::hash::H264;
        use std::sync::Mutex;

        struct ScriptedView {
            answer: Mutex<Result<Vec<UnfinishedSwapLeg>, String>>,
        }

        #[async_trait::async_trait]
        impl UnfinishedSwapsView for ScriptedView {
            async fn unfinished_v1_swaps_with_coin(&self, _ticker: &str) -> Result<Vec<UnfinishedSwapLeg>, String> {
                self.answer.lock().unwrap().clone()
            }
        }

        fn view(answer: Result<Vec<UnfinishedSwapLeg>, String>) -> ScriptedView {
            ScriptedView {
                answer: Mutex::new(answer),
            }
        }

        const KEY_A: [u8; 32] = [0x11; 32];
        const KEY_B: [u8; 32] = [0x22; 32];

        /// A leg started with the key `secret`, for the swap input `input`.
        fn leg_started_with(secret: &[u8; 32], input: &[u8], uuid_byte: u8) -> UnfinishedSwapLeg {
            let base = key_pair_from_secret(secret).unwrap();
            let derived = derive_swap_htlc_key_pair(&base, input).unwrap();
            UnfinishedSwapLeg {
                uuid: uuid::Uuid::from_bytes([uuid_byte; 16]).to_string(),
                recorded_htlc_pubkey: Some(H264::from(derived.public_slice())),
                swap_unique_input: input.to_vec(),
                saved_privkey_pubkey: None,
                own_payments_sent: true,
            }
        }

        fn no_unreadable(_dir: &Path, _ticker: &str) -> Vec<(PathBuf, String)> { Vec::new() }

        fn run(
            legs: Result<Vec<UnfinishedSwapLeg>, String>,
            secret: &[u8; 32],
            unreadable: impl Fn(&Path, &str) -> Vec<(PathBuf, String)>,
        ) -> Result<(), MmError<ZCoinBuildError>> {
            block_on(check_with_view(
                &view(legs),
                "ARRR",
                secret,
                Path::new("/nonexistent"),
                unreadable,
            ))
        }

        /// R39.8.0bd item 5, refusal: a swap started with another key blocks the activation, and the error
        /// lists its UUID and says what to do.
        #[test]
        fn a_swap_started_with_another_key_blocks_the_activation() {
            let blocked = leg_started_with(&KEY_A, b"input-one", 1);
            let fine = leg_started_with(&KEY_B, b"input-two", 2);

            let err = run(Ok(vec![blocked.clone(), fine.clone()]), &KEY_B, no_unreadable).unwrap_err();

            match err.get_inner() {
                ZCoinBuildError::KeyBlockedByUnfinishedSwaps { ticker, swaps } => {
                    assert_eq!(ticker, "ARRR");
                    assert!(swaps.contains(&blocked.uuid), "{swaps}");
                    assert!(!swaps.contains(&fine.uuid), "{swaps}");
                },
                other => panic!("unexpected error {other}"),
            }
            let text = err.to_string();
            assert!(text.contains("ARRR cannot be activated with this key"), "{text}");
            assert!(text.contains("could not be completed or refunded"), "{text}");
            assert!(text.contains("let them finish or refund, then switch"), "{text}");
        }

        /// R39.8.0bd item 5, allow: swaps started with this key, a swap whose saved private key is the recorded
        /// key, a swap that recorded no key, and no swap at all.
        #[test]
        fn swaps_that_can_use_this_key_allow_the_activation() {
            let matching = leg_started_with(&KEY_A, b"input-one", 1);
            let mut saved_privkey = leg_started_with(&KEY_B, b"input-two", 2);
            saved_privkey.saved_privkey_pubkey = saved_privkey.recorded_htlc_pubkey;
            let mut no_key = leg_started_with(&KEY_B, b"input-three", 3);
            no_key.recorded_htlc_pubkey = None;

            assert!(run(Ok(vec![matching, saved_privkey, no_key]), &KEY_A, no_unreadable).is_ok());
            assert!(run(Ok(Vec::new()), &KEY_A, no_unreadable).is_ok());
        }

        /// A saved private key that is not the recorded key counts as none: the key is re-derived and
        /// compared, and the swap blocks the activation when it differs.
        #[test]
        fn a_saved_private_key_that_is_not_the_recorded_key_does_not_exempt_the_swap() {
            let mut leg = leg_started_with(&KEY_B, b"input-two", 2);
            leg.saved_privkey_pubkey = Some(H264::from([0x42u8; 33]));

            let err = run(Ok(vec![leg.clone()]), &KEY_A, no_unreadable).unwrap_err();
            assert!(matches!(
                err.get_inner(),
                ZCoinBuildError::KeyBlockedByUnfinishedSwaps { .. }
            ));
            // With the key the swap was started with, the re-derived key matches and the swap is fine.
            assert!(run(Ok(vec![leg]), &KEY_B, no_unreadable).is_ok());
        }

        /// A swap started with the key being activated stays allowed when the coin is activated again with it
        /// (that an HD `account` does not enter the transparent key is shown in `z_htlc_key`).
        #[test]
        fn a_swap_started_with_this_key_stays_allowed_on_every_activation() {
            let leg = leg_started_with(&KEY_A, b"input-one", 1);
            for _ in 0..2 {
                assert!(run(Ok(vec![leg.clone()]), &KEY_A, no_unreadable).is_ok());
            }
        }

        /// Swaps that cannot be read refuse the activation.
        #[test]
        fn unreadable_saved_swaps_refuse_the_activation() {
            let err = run(Err("swap storage is locked".to_owned()), &KEY_A, no_unreadable).unwrap_err();
            assert!(matches!(
                err.get_inner(),
                ZCoinBuildError::UnfinishedSwapsUnreadable { reason, .. } if reason == "swap storage is locked"
            ));
        }

        /// R39.8.0bd item 6: a wallet database that cannot be read with a swap that has not sent its payment
        /// refuses the activation; with the payment sent, or every database readable, it allows.
        #[test]
        fn an_unreadable_wallet_database_with_an_unsent_swap_refuses_the_activation() {
            let mut unsent = leg_started_with(&KEY_A, b"input-one", 1);
            unsent.own_payments_sent = false;
            let sent = leg_started_with(&KEY_A, b"input-two", 2);
            let unreadable = |_dir: &Path, _ticker: &str| {
                vec![(
                    PathBuf::from("/data/ARRR_RELOADED_WALLET.0123456789abcdef.db"),
                    "locked".to_owned(),
                )]
            };

            let err = run(Ok(vec![unsent.clone(), sent.clone()]), &KEY_A, unreadable).unwrap_err();
            match err.get_inner() {
                ZCoinBuildError::WalletUnreadableWithUnsentSwaps { files, swaps, .. } => {
                    assert!(files.contains("ARRR_RELOADED_WALLET.0123456789abcdef.db"), "{files}");
                    assert!(swaps.contains(&unsent.uuid), "{swaps}");
                    assert!(!swaps.contains(&sent.uuid), "{swaps}");
                },
                other => panic!("unexpected error {other}"),
            }
            // Every swap has sent its payment: nothing to look up, so the unreadable file does not matter.
            assert!(run(Ok(vec![sent]), &KEY_A, unreadable).is_ok());
            // Every database readable.
            assert!(run(Ok(vec![unsent]), &KEY_A, no_unreadable).is_ok());
        }

        /// The probe finds a real file that is not a database, and an empty file is readable.
        #[test]
        fn the_probe_names_a_wallet_file_that_cannot_be_read() {
            let dir = crate::z_coin::z_coin_wallet_db::tests::test_db_dir("key-switch-probe");
            std::fs::write(dir.join("ARRR_RELOADED_WALLET.db"), b"").unwrap();
            let broken = dir.join("ARRR_RELOADED_WALLET.0123456789abcdef.db");
            std::fs::write(&broken, vec![0x5a; 8192]).unwrap();

            let unreadable = unreadable_wallet_files(&dir, "ARRR");

            assert_eq!(unreadable.len(), 1);
            assert_eq!(unreadable[0].0, broken);
        }
    }
}
