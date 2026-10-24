/// SSE (Server-Sent Events) endpoint handler.
///
/// Clients connect to `GET /event-stream?id=<client_id>` and receive a
/// persistent HTTP response with `Content-Type: text/event-stream`.
/// Events are formatted as `data: {json}\n\n` per the SSE specification.
#[cfg(not(target_arch = "wasm32"))]
pub const SSE_ENDPOINT: &str = "/event-stream";

#[cfg(not(target_arch = "wasm32"))]
pub async fn handle_sse(req: http::request::Parts, ctx_h: u32) -> hyper::Response<super::RpcBody> {
    use futures::stream::unfold;
    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::Frame;
    use mm2_core::mm_ctx::MmArc;
    use serde_json::json;

    let ctx = match MmArc::from_ffi_handle(ctx_h) {
        Ok(ctx) => ctx,
        Err(e) => {
            return hyper::Response::builder()
                .status(500)
                .body(super::full_body(format!("No context: {}", e)))
                .unwrap();
        },
    };

    // Extract client_id from query string: ?id=<u64>
    let client_id = req
        .uri
        .query()
        .and_then(|q| {
            q.split('&')
                .find_map(|pair| pair.strip_prefix("id="))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0);

    let cors = ctx.event_stream_access_control();
    let manager = ctx.event_stream_manager.clone();
    let handle = manager.new_client(client_id);

    // Build a streaming body: unfold yields SSE-formatted chunks from the event rx. hyper 1 has no
    // `Body::wrap_stream`; a `StreamBody` of `Frame`s is the equivalent and is polled lazily by the
    // connection task, so a slow client applies backpressure to the stream exactly as before.
    let stream = unfold(handle.rx, |mut rx| async move {
        let event = rx.recv().await?;
        let (origin, payload) = event.get();
        let msg = json!({
            "origin": origin,
            "payload": payload,
            "error": event.is_error(),
        });
        let formatted = format!("data: {}\n\n", msg);
        let bytes = hyper::body::Bytes::from(formatted);
        Some((Ok::<_, std::convert::Infallible>(Frame::data(bytes)), rx))
    });

    let body = StreamBody::new(stream).boxed_unsync();

    hyper::Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("Access-Control-Allow-Origin", cors)
        .body(body)
        .unwrap()
}
