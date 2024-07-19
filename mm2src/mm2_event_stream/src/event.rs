use serde_json::Value as Json;

#[derive(Default)]
/// Multi-purpose/generic event type that can easily be used over the event streaming
pub struct Event {
    /// The type of the event (balance, network, swap, etc...).
    streamer_id: String,
    /// The message to be sent to the client.
    message: Json,
    /// Indicating whether this event is an error event or a normal one.
    error: bool,
}

impl Event {
    /// Creates a new `Event` instance with the specified event type and message.
    #[inline]
    pub fn new(streamer_id: String, message: Json) -> Self {
        Self {
            streamer_id,
            message,
            error: false,
        }
    }

    /// Create a new error `Event` instance with the specified error event type and message.
    #[inline]
    pub fn err(streamer_id: String, message: Json) -> Self {
        Self {
            streamer_id,
            message,
            error: true,
        }
    }

    pub fn origin(&self) -> &str { &self.streamer_id }

    pub fn get(&self) -> (String, Json) {
        let prefix = if self.error { "ERROR:" } else { "" };
        (format!("{prefix}{}", self.streamer_id), self.message.clone())
    }
}
