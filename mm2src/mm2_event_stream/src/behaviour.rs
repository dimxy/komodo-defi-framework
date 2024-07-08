use crate::{ErrorEventName, EventName, EventStreamConfiguration};
use async_trait::async_trait;
use futures::channel::oneshot;

#[async_trait]
pub trait EventBehaviour {
    /// Returns the unique name of the event as an EventName enum variant.
    fn event_name() -> EventName;

    /// Returns the name of the error event as an ErrorEventName enum variant.
    /// By default, it returns `ErrorEventName::GenericError,` which shows as "ERROR" in the event stream.
    fn error_event_name() -> ErrorEventName { ErrorEventName::GenericError }

    /// Event handler that is responsible for broadcasting event data to the streaming channels.
    ///
    /// `tx` is a oneshot sender that is used to send the initialization status of the event.
    async fn handle(self, tx: oneshot::Sender<Result<(), String>>);

    /// Spawns the `Self::handle` in a separate thread.
    async fn spawn(self) -> Result<(), String>;
}
