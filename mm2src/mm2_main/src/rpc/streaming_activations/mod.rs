mod balance;
mod network;
mod heartbeat;
mod eth_fee_estimator;

// Re-exports
pub use balance::*;
pub use network::*;
pub use heartbeat::*;
pub use eth_fee_estimator::*;

#[derive(Serialize)]
/// The success response for any event streaming activation endpoint.
pub struct EnableStreamingResponse {
    pub streamer_id: String,
}

impl EnableStreamingResponse {
    fn new(streamer_id: String) -> Self {
        Self {
            streamer_id
        }
    }
}

#[derive(Deserialize)]
/// The request used for any event streaming deactivation endpoint.
pub struct DisableStreamingRequest {
    pub client_id: u64,
    pub streamer_id: String,
}