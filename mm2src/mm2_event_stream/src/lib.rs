use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value as Json;

#[cfg(target_arch = "wasm32")] use std::path::PathBuf;

#[cfg(target_arch = "wasm32")]
const DEFAULT_WORKER_PATH: &str = "event_streaming_worker.js";

/// Multi-purpose/generic event type that can easily be used over the event streaming
pub struct Event {
    /// The type of the event (balance, network, swap, etc...).
    event_type: String,
    /// The message to be sent to the client.
    message: Json,
    /// The filter object to be used to determine whether the event should be sent or not.
    /// It could also alter the event content
    ///
    /// The filter is wrapped in an `Arc` since the event producer should use it as a singleton
    /// by using the same filter over and over again with multiple events.
    filter: Option<Arc<dyn Filter>>,
}

impl Event {
    /// Creates a new `Event` instance with the specified event type and message.
    #[inline]
    pub fn new(event_type: String, message: Json, filter: Option<Arc<dyn Filter>>) -> Self {
        Self {
            event_type,
            message,
            filter,
        }
    }

    /// Create a new error `Event` instance with the specified error event type and message.
    #[inline]
    pub fn err(event_type: String, message: Json, filter: Option<Arc<dyn Filter>>) -> Self {
        Self {
            event_type: format!("ERROR_{event_type}"),
            message,
            filter,
        }
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
            || Some((self.event_type.clone(), self.message.clone())),
            |filter| {
                filter
                    .filter(&self.message, requested_events)
                    .map(|message| (self.event_type.clone(), message))
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

/// Event types streamed to clients through channels like Server-Sent Events (SSE).
#[derive(Deserialize, Eq, Hash, PartialEq)]
pub enum EventName {
    /// Indicates a change in the balance of a coin.
    BALANCE,
    /// Event triggered at regular intervals to indicate that the system is operational.
    HEARTBEAT,
    /// Returns p2p network information at a regular interval.
    NETWORK,
}

impl fmt::Display for EventName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BALANCE => write!(f, "COIN_BALANCE"),
            Self::HEARTBEAT => write!(f, "HEARTBEAT"),
            Self::NETWORK => write!(f, "NETWORK"),
        }
    }
}

/// Error event types used to indicate various kinds of errors to clients through channels like Server-Sent Events (SSE).
pub enum ErrorEventName {
    /// A generic error that doesn't fit any other specific categories.
    GenericError,
    /// Signifies an error related to fetching or calculating the balance of a coin.
    CoinBalanceError,
}

impl fmt::Display for ErrorEventName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenericError => write!(f, "ERROR"),
            Self::CoinBalanceError => write!(f, "COIN_BALANCE_ERROR"),
        }
    }
}

/// Configuration for event streaming
#[derive(Deserialize)]
pub struct EventStreamConfiguration {
    /// The value to set for the `Access-Control-Allow-Origin` header.
    #[serde(default)]
    pub access_control_allow_origin: String,
    #[serde(default)]
    active_events: HashMap<EventName, Json>,
    /// The path to the worker script for event streaming.
    #[cfg(target_arch = "wasm32")]
    #[serde(default = "default_worker_path")]
    pub worker_path: PathBuf,
}

#[cfg(target_arch = "wasm32")]
#[inline]
fn default_worker_path() -> PathBuf { PathBuf::from(DEFAULT_WORKER_PATH) }

impl Default for EventStreamConfiguration {
    fn default() -> Self {
        Self {
            access_control_allow_origin: String::from("*"),
            active_events: Default::default(),
            #[cfg(target_arch = "wasm32")]
            worker_path: default_worker_path(),
        }
    }
}

impl EventStreamConfiguration {
    /// Retrieves the configuration for a specific event by its name.
    #[inline]
    pub fn get_event(&self, event_name: &EventName) -> Option<Json> { self.active_events.get(event_name).cloned() }

    /// Gets the total number of active events in the configuration.
    #[inline]
    pub fn total_active_events(&self) -> usize { self.active_events.len() }
}

pub mod behaviour;
pub mod controller;
