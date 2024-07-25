//! RPC activation and deactivation for the network event streamer.
use super::{DisableStreamingRequest, DisableStreamingResponse, EnableStreamingResponse};

use common::HttpStatusCode;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_to_mm::MapToMmResult, mm_error::MmResult};
use mm2_net::network_event::NetworkEvent;

use serde_json::Value as Json;

#[derive(Deserialize)]
pub struct EnableNetworkStreamingRequest {
    pub client_id: u64,
    pub config: Option<Json>,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum NetworkStreamingRequestError {
    EnableError(String),
    DisableError(String),
}

impl HttpStatusCode for NetworkStreamingRequestError {
    fn status_code(&self) -> StatusCode {
        match self {
            NetworkStreamingRequestError::EnableError(_) => StatusCode::BAD_REQUEST,
            NetworkStreamingRequestError::DisableError(_) => StatusCode::BAD_REQUEST,
        }
    }
}

pub async fn enable_network(
    ctx: MmArc,
    req: EnableNetworkStreamingRequest,
) -> MmResult<EnableStreamingResponse, NetworkStreamingRequestError> {
    let network_steamer = NetworkEvent::try_new(req.config, ctx.clone())
        .map_to_mm(|e| NetworkStreamingRequestError::EnableError(format!("{e:?}")))?;
    ctx.event_stream_manager
        .add(req.client_id, network_steamer, ctx.spawner())
        .await
        .map(EnableStreamingResponse::new)
        .map_to_mm(|e| NetworkStreamingRequestError::EnableError(format!("{e:?}")))
}

pub async fn disable_network(
    ctx: MmArc,
    req: DisableStreamingRequest,
) -> MmResult<DisableStreamingResponse, NetworkStreamingRequestError> {
    ctx.event_stream_manager
        .stop(req.client_id, &req.streamer_id)
        .map_to_mm(|e| NetworkStreamingRequestError::DisableError(format!("{e:?}")))?;
    Ok(DisableStreamingResponse::new())
}
