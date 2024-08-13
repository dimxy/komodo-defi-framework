//! RPC activation and deactivation for different balance event streamers.
use super::EnableStreamingResponse;

use coins::eth::eth_balance_events::EthBalanceEventStreamer;
use coins::tendermint::tendermint_balance_events::TendermintBalanceEventStreamer;
use coins::utxo::utxo_balance_events::UtxoBalanceEventStreamer;
use coins::z_coin::z_balance_streaming::ZCoinBalanceEventStreamer;
use coins::{lp_coinfind, MmCoin, MmCoinEnum};
use common::HttpStatusCode;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::{map_to_mm::MapToMmResult, mm_error::MmResult};

use serde_json::Value as Json;

#[derive(Deserialize)]
pub struct EnableBalanceStreamingRequest {
    pub client_id: u64,
    pub coin: String,
    pub config: Option<Json>,
}

#[derive(Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum BalanceStreamingRequestError {
    EnableError(String),
    CoinNotFound,
    CoinNotSupported,
    Internal(String),
}

impl HttpStatusCode for BalanceStreamingRequestError {
    fn status_code(&self) -> StatusCode {
        match self {
            BalanceStreamingRequestError::EnableError(_) => StatusCode::BAD_REQUEST,
            BalanceStreamingRequestError::CoinNotFound => StatusCode::NOT_FOUND,
            BalanceStreamingRequestError::CoinNotSupported => StatusCode::NOT_IMPLEMENTED,
            BalanceStreamingRequestError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

pub async fn enable_balance(
    ctx: MmArc,
    req: EnableBalanceStreamingRequest,
) -> MmResult<EnableStreamingResponse, BalanceStreamingRequestError> {
    let coin = lp_coinfind(&ctx, &req.coin)
        .await
        .map_err(BalanceStreamingRequestError::Internal)?
        .ok_or(BalanceStreamingRequestError::CoinNotFound)?;

    let enable_result = match coin {
        MmCoinEnum::UtxoCoin(coin) => {
            check_empty_config(&req.config)?;
            let streamer = UtxoBalanceEventStreamer::new(coin.clone().into());
            ctx.event_stream_manager
                .add(req.client_id, streamer, coin.spawner())
                .await
        },
        MmCoinEnum::Bch(coin) => {
            check_empty_config(&req.config)?;
            let streamer = UtxoBalanceEventStreamer::new(coin.clone().into());
            ctx.event_stream_manager
                .add(req.client_id, streamer, coin.spawner())
                .await
        },
        MmCoinEnum::QtumCoin(coin) => {
            check_empty_config(&req.config)?;
            let streamer = UtxoBalanceEventStreamer::new(coin.clone().into());
            ctx.event_stream_manager
                .add(req.client_id, streamer, coin.spawner())
                .await
        },
        MmCoinEnum::EthCoin(coin) => {
            let streamer = EthBalanceEventStreamer::try_new(req.config, coin.clone())
                .map_to_mm(|e| BalanceStreamingRequestError::EnableError(format!("{e:?}")))?;
            ctx.event_stream_manager
                .add(req.client_id, streamer, coin.spawner())
                .await
        },
        MmCoinEnum::ZCoin(coin) => {
            check_empty_config(&req.config)?;
            let streamer = ZCoinBalanceEventStreamer::new(coin.clone());
            ctx.event_stream_manager
                .add(req.client_id, streamer, coin.spawner())
                .await
        },
        MmCoinEnum::Tendermint(coin) => {
            check_empty_config(&req.config)?;
            let streamer = TendermintBalanceEventStreamer::new(coin.clone());
            ctx.event_stream_manager
                .add(req.client_id, streamer, coin.spawner())
                .await
        },
        // FIXME: What about tokens?!
        _ => Err(BalanceStreamingRequestError::CoinNotSupported)?,
    };

    enable_result
        .map(EnableStreamingResponse::new)
        .map_to_mm(|e| BalanceStreamingRequestError::EnableError(format!("{e:?}")))
}

fn check_empty_config(config: &Option<Json>) -> MmResult<(), BalanceStreamingRequestError> {
    if config.is_some() {
        Err(BalanceStreamingRequestError::EnableError(
            "Invalid config provided. No config needed".to_string(),
        ))?
    }
    Ok(())
}
