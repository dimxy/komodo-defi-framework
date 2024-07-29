mod balance;
mod disable;
mod fee_estimation;
mod heartbeat;
mod network;
mod orders;
mod swaps;

// Re-exports
pub use balance::*;
pub use disable::*;
pub use fee_estimation::*;
pub use heartbeat::*;
pub use network::*;
pub use orders::*;
pub use swaps::*;

#[derive(Serialize)]
/// The success/ok response for any event streaming activation request.
/// Note that we don't have a unified request. It is rather defined per event streaming activation.
pub struct EnableStreamingResponse {
    pub streamer_id: String,
    // FIXME: If the the streamer was already running, it is probably running with different configuration.
    // We might want to inform the client that the configuration they asked for wasn't applied and return
    // the active configuration instead?
    // pub config: Json,
}

impl EnableStreamingResponse {
    fn new(streamer_id: String) -> Self { Self { streamer_id } }
}
