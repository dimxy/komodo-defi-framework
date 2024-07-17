use mm2_core::mm_ctx::MmArc;
use serde_json::json;
use std::collections::HashSet;
use web_sys::SharedWorker;

struct SendableSharedWorker(SharedWorker);

unsafe impl Send for SendableSharedWorker {}

struct SendableMessagePort(web_sys::MessagePort);

unsafe impl Send for SendableMessagePort {}

/// Handles broadcasted messages from `mm2_event_stream` continuously for WASM.
pub async fn handle_worker_stream(ctx: MmArc) {
    let config = &ctx.event_stream_configuration;

    let channel_controller = ctx.event_stream_manager.controller();
    let mut rx = channel_controller.create_channel(config.total_active_events());

    let worker_path = config
        .worker_path
        .to_str()
        .expect("worker_path contains invalid UTF-8 characters");
    let worker = SendableSharedWorker(
        SharedWorker::new(worker_path).unwrap_or_else(|_| {
            panic!(
                "Failed to create a new SharedWorker with path '{}'.\n\
                This could be due to the file missing or the browser being incompatible.\n\
                For more details, please refer to https://developer.mozilla.org/en-US/docs/Web/API/SharedWorker#browser_compatibility",
                worker_path
            )
        }),
    );

    let port = SendableMessagePort(worker.0.port());
    port.0.start();
    let filtered_events = HashSet::new();

    while let Some(event) = rx.recv().await {
        if let Some((event_type, message)) = event.get_data(&filtered_events) {
            let data = json!({
                "_type": event_type,
                "message": message,
            });

            let message_js = wasm_bindgen::JsValue::from_str(&data.to_string());

            port.0.post_message(&message_js)
            .expect("Failed to post a message to the SharedWorker.\n\
            This could be due to the browser being incompatible.\n\
            For more details, please refer to https://developer.mozilla.org/en-US/docs/Web/API/MessagePort/postMessage#browser_compatibility");
        }
    }
}
