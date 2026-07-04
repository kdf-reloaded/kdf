/// Balance event streamer.
///
/// Polls a single coin's balance at a configurable interval and emits
/// SSE events when the balance changes. Each coin gets its own streamer
/// instance identified by `StreamerId::Balance(ticker)`.
use async_trait::async_trait;
use coins::utxo::utxo_common::{address_balance as utxo_address_balance, address_from_str_unchecked};
use common::executor::Timer;
use common::log;
use futures::compat::Future01CompatExt;
use futures::future::{select, Either};
use mm2_event_stream::{mpsc, oneshot, Broadcaster, Event, EventStreamer, StreamerId};
use serde::Deserialize;
use serde_json::json;

use super::{EnableStreamingRequest, EnableStreamingResponse, StreamingError};
use coins::{lp_coinfind, MarketCoinOps, MmCoinEnum};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;

fn electrum_utxo_watch_address(coin: &MmCoinEnum) -> Option<String> {
    match coin {
        MmCoinEnum::UtxoCoin(c) => {
            if c.as_ref().rpc_client.is_native() {
                None
            } else {
                c.my_address().ok()
            }
        },
        MmCoinEnum::QtumCoin(c) => {
            if c.as_ref().rpc_client.is_native() {
                None
            } else {
                c.my_address().ok()
            }
        },
        MmCoinEnum::Bch(c) => {
            if c.as_ref().rpc_client.is_native() {
                None
            } else {
                c.my_address().ok()
            }
        },
        _ => None,
    }
}

async fn watched_balance(coin: &MmCoinEnum, watched_address: &Option<String>) -> Result<coins::CoinBalance, String> {
    match (coin, watched_address.as_deref()) {
        (MmCoinEnum::UtxoCoin(c), Some(address)) if !c.as_ref().rpc_client.is_native() => {
            let address = address_from_str_unchecked(c.as_ref(), address)?;
            utxo_address_balance(c, &address).await.map_err(|e| e.to_string())
        },
        (MmCoinEnum::QtumCoin(c), Some(address)) if !c.as_ref().rpc_client.is_native() => {
            let address = address_from_str_unchecked(c.as_ref(), address)?;
            utxo_address_balance(c, &address).await.map_err(|e| e.to_string())
        },
        (MmCoinEnum::Bch(c), Some(address)) if !c.as_ref().rpc_client.is_native() => {
            let address = address_from_str_unchecked(c.as_ref(), address)?;
            utxo_address_balance(c, &address).await.map_err(|e| e.to_string())
        },
        _ => coin.my_balance().compat().await.map_err(|e| e.to_string()),
    }
}

fn should_emit_balance_event(
    prev_spendable: Option<&String>,
    prev_unspendable: Option<&String>,
    prev_watched_address: Option<&String>,
    spendable: &String,
    unspendable: &String,
    watched_address: Option<&String>,
) -> bool {
    prev_spendable != Some(spendable)
        || prev_unspendable != Some(unspendable)
        || prev_watched_address != watched_address
}

/// Per-coin configuration for the balance streamer.
#[derive(Deserialize)]
pub struct EnableBalanceRequest {
    /// Coin ticker to monitor (must already be activated).
    pub coin: String,
    /// Poll interval in seconds. Default: 30, minimum: 10.
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
}

fn default_interval() -> u64 { 30 }

/// The balance streamer for a single coin.
pub struct BalanceEventStreamer {
    ticker: String,
    interval_secs: u64,
    ctx: MmArc,
}

impl BalanceEventStreamer {
    pub fn new(ticker: String, interval_secs: u64, ctx: MmArc) -> Self {
        Self {
            ticker,
            interval_secs: interval_secs.max(10), // floor at 10s
            ctx,
        }
    }
}

#[async_trait]
impl EventStreamer for BalanceEventStreamer {
    type DataInType = mm2_event_stream::NoDataIn;

    fn streamer_id(&self) -> StreamerId { StreamerId::Balance(self.ticker.clone()) }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        shutdown_rx: oneshot::Receiver<()>,
        _data_rx: mpsc::UnboundedReceiver<mm2_event_stream::NoDataIn>,
    ) {
        // Verify the coin exists before signalling readiness.
        let coin = match lp_coinfind(&self.ctx, &self.ticker).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                let _ = ready_tx.send(Err(format!("Coin {} is not activated", self.ticker)));
                return;
            },
            Err(e) => {
                let _ = ready_tx.send(Err(format!("Error finding coin {}: {}", self.ticker, e)));
                return;
            },
        };

        let _ = ready_tx.send(Ok(()));

        let interval_secs = self.interval_secs as f64;
        let mut shutdown = core::pin::pin!(shutdown_rx);
        let sid = StreamerId::Balance(self.ticker.clone());

        // Track previous balance to only emit on change.
        let mut prev_spendable: Option<String> = None;
        let mut prev_unspendable: Option<String> = None;
        let mut watched_address: Option<String> = electrum_utxo_watch_address(&coin);
        let mut prev_watched_address: Option<String> = None;

        // Emit an initial snapshot right away so clients don't wait for the first interval tick.
        match watched_balance(&coin, &watched_address).await {
            Ok(balance) => {
                let spendable = balance.spendable.to_string();
                let unspendable = balance.unspendable.to_string();

                prev_spendable = Some(spendable.clone());
                prev_unspendable = Some(unspendable.clone());
                prev_watched_address = watched_address.clone();

                let event = Event::new(
                    sid.clone(),
                    json!({
                        "coin": self.ticker,
                        "watched_address": watched_address,
                        "spendable": spendable,
                        "unspendable": unspendable,
                        "timestamp": common::now_ms(),
                    }),
                );
                broadcaster.broadcast(event);
            },
            Err(e) => {
                log::error!("Initial balance poll error for {}: {}", self.ticker, e);
                let event = Event::err(
                    sid.clone(),
                    json!({
                        "coin": self.ticker,
                        "error": e.to_string(),
                        "timestamp": common::now_ms(),
                    }),
                );
                broadcaster.broadcast(event);
            },
        }

        loop {
            // Re-register watch target for Electrum-backed UTXO coins if the active address changes.
            // This is relevant when address state is rotated externally (e.g. account/address updates).
            let current_watch_address = electrum_utxo_watch_address(&coin);
            if current_watch_address != watched_address {
                watched_address = current_watch_address;
            }

            let sleep = Timer::sleep(interval_secs);
            let sleep = core::pin::pin!(sleep);
            match select(sleep, &mut shutdown).await {
                Either::Left(_) => {
                    match watched_balance(&coin, &watched_address).await {
                        Ok(balance) => {
                            let spendable = balance.spendable.to_string();
                            let unspendable = balance.unspendable.to_string();

                            // Emit when the balance or the watched Electrum UTXO address changes.
                            let changed = should_emit_balance_event(
                                prev_spendable.as_ref(),
                                prev_unspendable.as_ref(),
                                prev_watched_address.as_ref(),
                                &spendable,
                                &unspendable,
                                watched_address.as_ref(),
                            );

                            if changed {
                                prev_spendable = Some(spendable.clone());
                                prev_unspendable = Some(unspendable.clone());
                                prev_watched_address = watched_address.clone();

                                let event = Event::new(
                                    sid.clone(),
                                    json!({
                                        "coin": self.ticker,
                                        "watched_address": watched_address,
                                        "spendable": spendable,
                                        "unspendable": unspendable,
                                        "timestamp": common::now_ms(),
                                    }),
                                );
                                broadcaster.broadcast(event);
                            }
                        },
                        Err(e) => {
                            log::error!("Balance poll error for {}: {}", self.ticker, e);
                            let event = Event::err(
                                sid.clone(),
                                json!({
                                    "coin": self.ticker,
                                    "error": e.to_string(),
                                    "timestamp": common::now_ms(),
                                }),
                            );
                            broadcaster.broadcast(event);
                        },
                    }
                },
                Either::Right(_) => {
                    break;
                },
            }
        }
    }
}

/// RPC handler for `stream::balance::enable`.
pub async fn enable_balance(
    ctx: MmArc,
    req: EnableStreamingRequest<EnableBalanceRequest>,
) -> MmResult<EnableStreamingResponse, StreamingError> {
    let client_id = req.client_id;
    let ticker = req.inner.coin.clone();
    let interval = req.inner.interval_secs;

    let streamer = BalanceEventStreamer::new(ticker, interval, ctx.clone());
    let streamer_id = streamer.streamer_id().to_string();
    ctx.event_stream_manager
        .add(client_id, streamer)
        .await
        .map_err(|e| MmError::new(StreamingError::InitFailed(e)))?;

    Ok(EnableStreamingResponse::new(streamer_id))
}

#[cfg(test)]
mod tests {
    use super::should_emit_balance_event;

    #[test]
    fn emits_when_watched_address_changes() {
        let prev_spendable = Some("1".to_string());
        let prev_unspendable = Some("0".to_string());
        let prev_watched_address = Some("RoldAddress".to_string());
        let spendable = "1".to_string();
        let unspendable = "0".to_string();
        let watched_address = Some("RnewAddress".to_string());

        assert!(should_emit_balance_event(
            prev_spendable.as_ref(),
            prev_unspendable.as_ref(),
            prev_watched_address.as_ref(),
            &spendable,
            &unspendable,
            watched_address.as_ref(),
        ));
    }

    #[test]
    fn skips_when_balance_and_watched_address_are_unchanged() {
        let prev_spendable = Some("1".to_string());
        let prev_unspendable = Some("0".to_string());
        let prev_watched_address = Some("RsameAddress".to_string());
        let spendable = "1".to_string();
        let unspendable = "0".to_string();
        let watched_address = Some("RsameAddress".to_string());

        assert!(!should_emit_balance_event(
            prev_spendable.as_ref(),
            prev_unspendable.as_ref(),
            prev_watched_address.as_ref(),
            &spendable,
            &unspendable,
            watched_address.as_ref(),
        ));
    }
}
