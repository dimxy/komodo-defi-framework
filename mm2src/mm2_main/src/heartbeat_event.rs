use async_trait::async_trait;
use common::executor::Timer;
use futures::channel::oneshot;
use mm2_event_stream::{Event, EventStreamer, NoDataIn, StreamHandlerInput, StreamingManager};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct HeartbeatEventConfig {
    /// The time in seconds to wait before sending another ping event.
    pub stream_interval_seconds: f64,
}

impl Default for HeartbeatEventConfig {
    fn default() -> Self {
        Self {
            stream_interval_seconds: 5.0,
        }
    }
}

pub struct HeartbeatEvent {
    config: HeartbeatEventConfig,
}

impl HeartbeatEvent {
    pub fn try_new(config: Option<Json>) -> serde_json::Result<Self> {
        Ok(Self {
            config: config.map(serde_json::from_value).unwrap_or(Ok(Default::default()))?,
        })
    }
}

#[async_trait]
impl EventStreamer for HeartbeatEvent {
    type DataInType = NoDataIn;

    fn streamer_id(&self) -> String { "HEARTBEAT".to_string() }

    async fn handle(
        self,
        broadcaster: StreamingManager,
        ready_tx: oneshot::Sender<Result<(), String>>,
        _: impl StreamHandlerInput<NoDataIn>,
    ) {
        ready_tx.send(Ok(())).unwrap();

        loop {
            broadcaster.broadcast(Event::new(self.streamer_id(), json!({})));

            Timer::sleep(self.config.stream_interval_seconds).await;
        }
    }
}
