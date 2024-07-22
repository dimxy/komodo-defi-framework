use async_trait::async_trait;
use common::executor::Timer;
use futures::channel::oneshot;
use mm2_event_stream::{Event, EventStreamer, NoDataIn, StreamHandlerInput, StreamingManager};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeartbeatEventConfig {
    /// The time in seconds to wait before sending another ping event.
    #[serde(default = "default_stream_interval")]
    pub stream_interval_seconds: f64,
}

const fn default_stream_interval() -> f64 { 5. }

pub struct HeartbeatEvent {
    config: HeartbeatEventConfig,
}

impl HeartbeatEvent {
    pub fn try_new(config: Json) -> Result<Self, String> {
        Ok(Self {
            config: serde_json::from_value(config).map_err(|e| e.to_string())?,
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
