use crate::p2p::P2PContext;
use async_trait::async_trait;
use common::{executor::{SpawnFuture, Timer},
             log::info};
use futures::channel::oneshot;
use mm2_core::mm_ctx::MmArc;
pub use mm2_event_stream::behaviour::EventBehaviour;
use mm2_event_stream::{Event, EventName};
use mm2_libp2p::behaviours::atomicdex;
use serde::Deserialize;
use serde_json::{json, Value as Json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkEventConfig {
    /// The time in seconds to wait after sending network info before sending another one.
    #[serde(default = "default_stream_interval")]
    pub stream_interval_seconds: f64,
    /// Always (force) send network info data, even if it's the same as the previous one sent.
    #[serde(default)]
    pub always_send: bool,
}

const fn default_stream_interval() -> f64 { 5. }

pub struct NetworkEvent {
    config: NetworkEventConfig,
    ctx: MmArc,
}

impl NetworkEvent {
    pub fn try_new(config: Json, ctx: MmArc) -> Result<Self, String> {
        Ok(Self {
            config: serde_json::from_value(config).map_err(|e| e.to_string())?,
            ctx,
        })
    }
}

#[async_trait]
impl EventBehaviour for NetworkEvent {
    fn event_name() -> EventName { EventName::NETWORK }

    async fn handle(self, tx: oneshot::Sender<Result<(), String>>) {
        let p2p_ctx = P2PContext::fetch_from_mm_arc(&self.ctx);
        let mut previously_sent = json!({});

        tx.send(Ok(())).unwrap();

        loop {
            let p2p_cmd_tx = p2p_ctx.cmd_tx.lock().clone();

            let peers_info = atomicdex::get_peers_info(p2p_cmd_tx.clone()).await;
            let gossip_mesh = atomicdex::get_gossip_mesh(p2p_cmd_tx.clone()).await;
            let gossip_peer_topics = atomicdex::get_gossip_peer_topics(p2p_cmd_tx.clone()).await;
            let gossip_topic_peers = atomicdex::get_gossip_topic_peers(p2p_cmd_tx.clone()).await;
            let relay_mesh = atomicdex::get_relay_mesh(p2p_cmd_tx).await;

            let event_data = json!({
                "peers_info": peers_info,
                "gossip_mesh": gossip_mesh,
                "gossip_peer_topics": gossip_peer_topics,
                "gossip_topic_peers": gossip_topic_peers,
                "relay_mesh": relay_mesh,
            });

            if previously_sent != event_data || self.config.always_send {
                self.ctx
                    .stream_channel_controller
                    .broadcast(Event::new(Self::event_name().to_string(), event_data.to_string()))
                    .await;

                previously_sent = event_data;
            }

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
                "The handler dropped before sending an initialization status: {}",
                e
            ))
        })
    }
}
