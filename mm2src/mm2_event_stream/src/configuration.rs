use std::collections::HashMap;

use super::EventName;

use serde::Deserialize;
use serde_json::Value as Json;

#[cfg(target_arch = "wasm32")] use std::path::PathBuf;

#[cfg(target_arch = "wasm32")]
const DEFAULT_WORKER_PATH: &str = "event_streaming_worker.js";

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
