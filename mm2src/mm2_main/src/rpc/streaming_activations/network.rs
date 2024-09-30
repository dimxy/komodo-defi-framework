//! RPC activation and deactivation for the network event streamer.
use super::EnableStreamingResponse;

use common::HttpStatusCode;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_to_mm::MapToMmResult, mm_error::MmResult};
use mm2_net::network_event::{NetworkEvent, NetworkEventConfig};

#[derive(Deserialize)]
pub struct EnableNetworkStreamingRequest {
    pub client_id: u64,
    pub config: NetworkEventConfig,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum NetworkStreamingRequestError {
    EnableError(String),
}

impl HttpStatusCode for NetworkStreamingRequestError {
    fn status_code(&self) -> StatusCode { StatusCode::BAD_REQUEST }
}

pub async fn enable_network(
    ctx: MmArc,
    req: EnableNetworkStreamingRequest,
) -> MmResult<EnableStreamingResponse, NetworkStreamingRequestError> {
    let network_steamer = NetworkEvent::new(req.config, ctx.clone());
    ctx.event_stream_manager
        .add(req.client_id, network_steamer, ctx.spawner())
        .await
        .map(EnableStreamingResponse::new)
        .map_to_mm(|e| NetworkStreamingRequestError::EnableError(format!("{e:?}")))
}
