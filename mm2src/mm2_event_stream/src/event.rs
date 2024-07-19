use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value as Json;

/// Multi-purpose/generic event type that can easily be used over the event streaming
pub struct Event {
    /// The type of the event (balance, network, swap, etc...).
    streamer_id: String,
    /// The message to be sent to the client.
    message: Json,
    /// The filter object to be used to determine whether the event should be sent or not.
    /// It could also alter the event content
    ///
    /// The filter is wrapped in an `Arc` since the event producer should use it as a singleton
    /// by using the same filter over and over again with multiple events.
    filter: Option<Arc<dyn Filter>>,
    /// Indicating whether this event is an error event or a normal one.
    error: bool,
}

impl Event {
    /// Creates a new `Event` instance with the specified event type and message.
    #[inline]
    pub fn new(streamer_id: String, message: Json, filter: Option<Arc<dyn Filter>>) -> Self {
        Self {
            streamer_id,
            message,
            filter,
            error: false,
        }
    }

    /// Create a new error `Event` instance with the specified error event type and message.
    #[inline]
    pub fn err(streamer_id: String, message: Json, filter: Option<Arc<dyn Filter>>) -> Self {
        Self {
            streamer_id,
            message,
            filter,
            error: true,
        }
    }

    pub fn origin(&self) -> &str {
        &self.streamer_id
    }

    pub fn get(&self) -> (String, Json) {
        let prefix = if self.error { "ERROR:" } else { "" };
        (format!("{prefix}{streamer_id}"), self.message.clone())
    }

    /// Returns the event type and message to be sent or `None` if the event should not be sent.
    ///
    /// Uses the `requested_events` to determine whether the event should be sent or not.
    /// If `requested_events` is empty, this doesn't mean the event won't be sent, this is
    /// decided by the event's filtering mechanism.
    ///
    /// `requested_events` could also be used to alter the event content (e.g. remove certain fields)
    pub fn get_data(&self, requested_events: &HashSet<String>) -> Option<(String, Json)> {
        self.filter.as_ref().map_or_else(
            // If no filter is set, send the event as is.
            || Some((self.streamer_id.clone(), self.message.clone())),
            |filter| {
                filter
                    .filter(&self.message, requested_events)
                    .map(|message| (self.streamer_id.clone(), message))
            },
        )
    }
}

/// A trait that defines the filtering mechanism for events.
///
/// Each event has a filter that determines whether the event should be send out
/// to the client or not based on the client's requested events.
pub trait Filter: Send + Sync {
    /// Filters the event based on the requested events.
    ///
    /// Returns the (maybe altered) message to be sent or `None` if the event should not be sent.
    /// `requested_events` is a set of the events that the client asked to subscribe to (e.g. `BALANCE:BTC`)
    fn filter(&self, message: &Json, requested_events: &HashSet<String>) -> Option<Json>;
}
