use crate::EventName;
use async_trait::async_trait;
use futures::channel::oneshot;

#[async_trait]
pub trait EventStreamer {
    /// Returns the name of the event as an EventName enum variant.
    fn event_name() -> EventName;

    /// Returns a human readable unique identifier for the event streamer.
    /// No other event should have the same identifier.
    fn event_id(&self) -> String { unimplemented!() }

    /// Event handler that is responsible for broadcasting event data to the streaming channels.
    ///
    /// `tx` is a oneshot sender that is used to send the initialization status of the event.
    async fn handle(self, tx: oneshot::Sender<Result<(), String>>);

    /// Spawns the `Self::handle` in a separate thread.
    async fn spawn(self) -> Result<(), String>;
}
