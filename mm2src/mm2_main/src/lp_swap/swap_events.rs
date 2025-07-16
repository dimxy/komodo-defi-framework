use super::maker_swap::MakerSavedEvent;
use super::maker_swap_v2::MakerSwapEvent;
use super::taker_swap::TakerSavedEvent;
use super::taker_swap_v2::TakerSwapEvent;
use mm2_event_stream::{Broadcaster, Event, EventStreamer, StreamHandlerInput, StreamerId};

use async_trait::async_trait;
use futures::channel::oneshot;
use futures::StreamExt;
use uuid::Uuid;

pub struct SwapStatusStreamer;

impl SwapStatusStreamer {
    #[inline(always)]
    pub fn new() -> Self { Self }

    #[inline(always)]
    pub const fn derive_streamer_id() -> StreamerId { StreamerId::SwapStatus }
}

#[derive(Serialize)]
#[serde(tag = "swap_type", content = "swap_data")]
pub enum SwapStatusEvent {
    MakerV1 { uuid: Uuid, event: MakerSavedEvent },
    TakerV1 { uuid: Uuid, event: TakerSavedEvent },
    MakerV2 { uuid: Uuid, event: MakerSwapEvent },
    TakerV2 { uuid: Uuid, event: TakerSwapEvent },
}

#[async_trait]
impl EventStreamer for SwapStatusStreamer {
    type DataInType = SwapStatusEvent;

    fn streamer_id(&self) -> StreamerId { Self::derive_streamer_id() }

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
