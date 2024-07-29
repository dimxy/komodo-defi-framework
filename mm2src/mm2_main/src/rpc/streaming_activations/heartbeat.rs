//! RPC activation and deactivation for the heartbeats.
use super::{EnableStreamingResponse};

use crate::mm2::heartbeat_event::HeartbeatEvent;
use common::HttpStatusCode;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_to_mm::MapToMmResult, mm_error::MmResult};

use serde_json::Value as Json;

#[derive(Deserialize)]
pub struct EnableHeartbeatRequest {
    pub client_id: u64,
    pub config: Option<Json>,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum HeartbeatRequestError {
    EnableError(String),
}

impl HttpStatusCode for HeartbeatRequestError {
    fn status_code(&self) -> StatusCode {
        StatusCode::BAD_REQUEST
    }
}

pub async fn enable_heartbeat(
    ctx: MmArc,
    req: EnableHeartbeatRequest,
) -> MmResult<EnableStreamingResponse, HeartbeatRequestError> {
    let heartbeat_streamer =
        HeartbeatEvent::try_new(req.config).map_to_mm(|e| HeartbeatRequestError::EnableError(format!("{e:?}")))?;
    ctx.event_stream_manager
        .add(req.client_id, heartbeat_streamer, ctx.spawner())
        .await
        .map(EnableStreamingResponse::new)
        .map_to_mm(|e| HeartbeatRequestError::EnableError(format!("{e:?}")))
}
