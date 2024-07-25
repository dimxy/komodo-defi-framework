mod balance;
mod fee_estimation;
mod heartbeat;
mod network;

// Re-exports
pub use balance::*;
pub use fee_estimation::*;
pub use heartbeat::*;
pub use network::*;

#[derive(Serialize)]
/// The success/ok response for any event streaming activation request.
pub struct EnableStreamingResponse {
    pub streamer_id: String,
    // FIXME: Consider returning the applied config here (might be different from the one the client requested).
    // pub config: Json,
}

impl EnableStreamingResponse {
    fn new(streamer_id: String) -> Self { Self { streamer_id } }
}

#[derive(Deserialize)]
/// The request used for any event streaming deactivation.
pub struct DisableStreamingRequest {
    pub client_id: u64,
    pub streamer_id: String,
}
