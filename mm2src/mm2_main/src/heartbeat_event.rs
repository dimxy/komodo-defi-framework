use async_trait::async_trait;
use common::{executor::{SpawnFuture, Timer},
             log::info};
use futures::channel::oneshot;
use mm2_core::mm_ctx::MmArc;
use mm2_event_stream::{Event, EventBehaviour, EventName};
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
    ctx: MmArc,
}

impl HeartbeatEvent {
    pub fn try_new(config: Json, ctx: MmArc) -> Result<Self, String> {
        Ok(Self {
            config: serde_json::from_value(config).map_err(|e| e.to_string())?,
            ctx,
        })
    }
}

#[async_trait]
impl EventBehaviour for HeartbeatEvent {
    fn event_name() -> EventName { EventName::HEARTBEAT }

    async fn handle(self, tx: oneshot::Sender<Result<(), String>>) {
        tx.send(Ok(())).unwrap();

        loop {
            self.ctx
                .stream_channel_controller
                .broadcast(Event::new(Self::event_name().to_string(), json!({}), None))
                .await;

            Timer::sleep(self.config.stream_interval_seconds).await;
        }
    }

    async fn spawn(self) -> Result<(), String> {
        info!(
            "{} event is activated with config: {:?}",
            Self::event_name(),
            self.config
        );

        let (tx, rx) = oneshot::channel();
        self.ctx.spawner().spawn(self.handle(tx));

        rx.await.unwrap_or_else(|e| {
            Err(format!(
                "The handler dropped before sending an initialization status: {e}",
            ))
        })
    }
}
