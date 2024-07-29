mod controller;
pub mod event;
pub mod manager;
pub mod streamer;

// Re-export important types.
pub use event::Event;
pub use manager::{StreamingManager, StreamingManagerError};
pub use streamer::{EventStreamer, NoDataIn, StreamHandlerInput};
