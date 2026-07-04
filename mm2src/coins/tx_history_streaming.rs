use crate::TransactionDetails;
use mm2_core::mm_ctx::MmArc;
use mm2_event_stream::StreamerId;
use serde_json::{json, Value as Json};

/// Typed input accepted by the `TX_HISTORY:<ticker>` streamer.
pub enum TxHistoryStreamerInput {
    Records(Vec<Json>),
    Error(String),
}

pub fn publish_tx_history_records<I>(ctx: &MmArc, ticker: &str, records: I)
where
    I: IntoIterator<Item = TransactionDetails> + Send + 'static,
{
    let streamer_id = StreamerId::TxHistory(ticker.to_owned());
    let _ = ctx.event_stream_manager.send_fn(&streamer_id, || {
        let records = records
            .into_iter()
            .filter_map(|record| serde_json::to_value(record).ok())
            .collect();
        TxHistoryStreamerInput::Records(records)
    });
}

pub fn publish_tx_history_error(ctx: &MmArc, ticker: &str, error: impl Into<String>) {
    let streamer_id = StreamerId::TxHistory(ticker.to_owned());
    let error = error.into();
    let _ = ctx
        .event_stream_manager
        .send(&streamer_id, TxHistoryStreamerInput::Error(error));
}

pub fn tx_history_error_payload(error: String) -> Json { json!({ "error": error }) }

#[cfg(test)]
mod tests {
    use super::*;
    use mm2_core::mm_ctx::MmCtxBuilder;

    struct PanicOnIter;

    impl IntoIterator for PanicOnIter {
        type Item = TransactionDetails;
        type IntoIter = std::vec::IntoIter<TransactionDetails>;

        fn into_iter(self) -> Self::IntoIter { panic!("inactive tx-history publish must not consume records") }
    }

    #[test]
    fn publish_records_is_lazy_when_streamer_inactive() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        publish_tx_history_records(&ctx, "RICK", PanicOnIter);
    }

    #[test]
    fn tx_history_error_payload_uses_bound_error_field() {
        assert_eq!(tx_history_error_payload("boom".to_owned()), json!({ "error": "boom" }));
    }
}
