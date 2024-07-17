use hyper::{body::Bytes, Body, Request, Response};
use mm2_core::mm_ctx::MmArc;
use serde_json::json;
use std::{collections::HashSet, convert::Infallible};

pub const SSE_ENDPOINT: &str = "/event-stream";

/// Handles broadcasted messages from `mm2_event_stream` continuously.
pub async fn handle_sse(request: Request<Body>, ctx_h: u32) -> Result<Response<Body>, Infallible> {
    // This is only called once for per client on the initialization,
    // meaning this is not a resource intensive computation.
    let ctx = match MmArc::from_ffi_handle(ctx_h) {
        Ok(ctx) => ctx,
        Err(err) => return handle_internal_error(err).await,
    };

    let config = &ctx.event_stream_configuration;
    let filtered_events = HashSet::new();

    let channel_controller = ctx.event_stream_manager.controller();
    let mut rx = channel_controller.create_channel(config.total_active_events());
    let body = Body::wrap_stream(async_stream::stream! {
        while let Some(event) = rx.recv().await {
            if let Some((event_type, message)) = event.get_data(&filtered_events) {
                let data = json!({
                    "_type": event_type,
                    "message": message,
                });

                yield Ok::<_, hyper::Error>(Bytes::from(format!("data: {data} \n\n")));
            }
        }
    });

    let response = Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Access-Control-Allow-Origin", &config.access_control_allow_origin)
        .body(body);

    match response {
        Ok(res) => Ok(res),
        Err(err) => handle_internal_error(err.to_string()).await,
    }
}

/// Fallback function for handling errors in SSE connections
async fn handle_internal_error(message: String) -> Result<Response<Body>, Infallible> {
    let response = Response::builder()
        .status(500)
        .body(Body::from(message))
        .expect("Returning 500 should never fail.");

    Ok(response)
}
