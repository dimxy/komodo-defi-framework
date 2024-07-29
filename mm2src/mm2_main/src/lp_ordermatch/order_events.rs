use super::{MakerMatch, TakerMatch};
use mm2_event_stream::{Broadcaster, Event, EventStreamer, StreamHandlerInput};

use async_trait::async_trait;
use futures::channel::oneshot;
use futures::StreamExt;

pub struct OrderStatusStreamer;

impl OrderStatusStreamer {
    pub fn new() -> Self { Self }
}

#[derive(Clone, Serialize)]
#[serde(tag = "order_type", content = "order_data")]
pub(super) enum OrderStatusEvent {
    MakerMatch(MakerMatch),
    TakerMatch(TakerMatch),
    MakerConnected(MakerMatch),
    TakerConnected(TakerMatch),
}

#[async_trait]
impl EventStreamer for OrderStatusStreamer {
    type DataInType = ();

    fn streamer_id(&self) -> String { "ORDER_STATUS".to_string() }

    async fn handle(
        self,
        broadcaster: Broadcaster,
        ready_tx: oneshot::Sender<Result<(), String>>,
        mut data_rx: impl StreamHandlerInput<Self::DataInType>,
    ) {
        ready_tx
            .send(Ok(()))
            .expect("Receiver is dropped, which should never happen.");

        while let Some(swap_data) = data_rx.next().await {
            let event_data = serde_json::to_value(swap_data).expect("Serialization shouldn't fail.");
            let event = Event::new(self.streamer_id(), event_data);
            broadcaster.broadcast(event);
        }
    }
}
