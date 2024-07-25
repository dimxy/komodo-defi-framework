//! RPC activation and deactivation of the swap status streamer.
use super::{DisableStreamingRequest, DisableStreamingResponse, EnableStreamingResponse};
use crate::mm2::lp_swap::swap_events::SwapStatusStreamer;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_to_mm::MapToMmResult, mm_error::MmResult};

use common::HttpStatusCode;
use http::StatusCode;

#[derive(Deserialize)]
pub struct EnableSwapStatusStreamingRequest {
    pub client_id: u64,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum SwapStatusStreamingRequestError {
    EnableError(String),
    DisableError(String),
}

impl HttpStatusCode for SwapStatusStreamingRequestError {
    fn status_code(&self) -> StatusCode {
        match self {
            SwapStatusStreamingRequestError::EnableError(_) => StatusCode::BAD_REQUEST,
            SwapStatusStreamingRequestError::DisableError(_) => StatusCode::BAD_REQUEST,
        }
    }
}

pub async fn enable_swap_status(
    ctx: MmArc,
    req: EnableSwapStatusStreamingRequest,
) -> MmResult<EnableStreamingResponse, SwapStatusStreamingRequestError> {
    let swap_status_streamer = SwapStatusStreamer::new();
    ctx.event_stream_manager
        .add(req.client_id, swap_status_streamer, ctx.spawner())
        .await
        .map(EnableStreamingResponse::new)
        .map_to_mm(|e| SwapStatusStreamingRequestError::EnableError(format!("{e:?}")))
}

pub async fn disable_swap_status(
    ctx: MmArc,
    req: DisableStreamingRequest,
) -> MmResult<DisableStreamingResponse, SwapStatusStreamingRequestError> {
    ctx.event_stream_manager
        .stop(req.client_id, &req.streamer_id)
        .map_to_mm(|e| SwapStatusStreamingRequestError::DisableError(format!("{e:?}")))?;
    Ok(DisableStreamingResponse::new())
}
