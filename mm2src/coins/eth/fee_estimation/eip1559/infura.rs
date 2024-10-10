use super::{EstimationSource, FeePerGasEstimated, FeePerGasLevel, HeaderParams};
use crate::eth::{Web3RpcError, Web3RpcResult};
use crate::{wei_from_gwei_decimal, NumConversError};
use mm2_err_handle::mm_error::MmError;
use mm2_err_handle::prelude::*;
use mm2_net::transport::slurp_url_with_headers;
use mm2_number::BigDecimal;

use http::StatusCode;
use serde_json::{self as json};
use std::convert::TryFrom;
use std::convert::TryInto;
use url::{ParseError, Url};

lazy_static! {
    /// API key for testing
    static ref INFURA_GAS_API_AUTH_TEST: String = std::env::var("INFURA_GAS_API_AUTH_TEST").unwrap_or_default();
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct InfuraFeePerGasLevel {
    #[serde(rename = "suggestedMaxPriorityFeePerGas")]
    pub suggested_max_priority_fee_per_gas: BigDecimal,
    #[serde(rename = "suggestedMaxFeePerGas")]
    pub suggested_max_fee_per_gas: BigDecimal,
    #[serde(rename = "minWaitTimeEstimate")]
    pub min_wait_time_estimate: u32,
    #[serde(rename = "maxWaitTimeEstimate")]
    pub max_wait_time_estimate: u32,
}

/// Infura gas api response
/// see https://docs.infura.io/api/infura-expansion-apis/gas-api/api-reference/gasprices-type2
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct InfuraFeePerGas {
    pub low: InfuraFeePerGasLevel,
    pub medium: InfuraFeePerGasLevel,
    pub high: InfuraFeePerGasLevel,
    #[serde(rename = "estimatedBaseFee")]
    pub estimated_base_fee: BigDecimal,
    #[serde(rename = "networkCongestion")]
    pub network_congestion: BigDecimal,
    #[serde(rename = "latestPriorityFeeRange")]
    pub latest_priority_fee_range: Vec<BigDecimal>,
    #[serde(rename = "historicalPriorityFeeRange")]
    pub historical_priority_fee_range: Vec<BigDecimal>,
    #[serde(rename = "historicalBaseFeeRange")]
    pub historical_base_fee_range: Vec<BigDecimal>,
    #[serde(rename = "priorityFeeTrend")]
    pub priority_fee_trend: String, // we are not using enum here bcz values not mentioned in docs could be received
    #[serde(rename = "baseFeeTrend")]
    pub base_fee_trend: String,
}

impl TryFrom<InfuraFeePerGas> for FeePerGasEstimated {
    type Error = MmError<NumConversError>;

    fn try_from(infura_fees: InfuraFeePerGas) -> Result<Self, Self::Error> {
        Ok(Self {
            base_fee: wei_from_gwei_decimal!(&infura_fees.estimated_base_fee)?,
            low: FeePerGasLevel {
                max_fee_per_gas: wei_from_gwei_decimal!(&infura_fees.low.suggested_max_fee_per_gas)?,
                max_priority_fee_per_gas: wei_from_gwei_decimal!(&infura_fees.low.suggested_max_priority_fee_per_gas)?,
                min_wait_time: Some(infura_fees.low.min_wait_time_estimate),
                max_wait_time: Some(infura_fees.low.max_wait_time_estimate),
            },
            medium: FeePerGasLevel {
                max_fee_per_gas: wei_from_gwei_decimal!(&infura_fees.medium.suggested_max_fee_per_gas)?,
                max_priority_fee_per_gas: wei_from_gwei_decimal!(
                    &infura_fees.medium.suggested_max_priority_fee_per_gas
                )?,
                min_wait_time: Some(infura_fees.medium.min_wait_time_estimate),
                max_wait_time: Some(infura_fees.medium.max_wait_time_estimate),
            },
            high: FeePerGasLevel {
                max_fee_per_gas: wei_from_gwei_decimal!(&infura_fees.high.suggested_max_fee_per_gas)?,
                max_priority_fee_per_gas: wei_from_gwei_decimal!(&infura_fees.high.suggested_max_priority_fee_per_gas)?,
                min_wait_time: Some(infura_fees.high.min_wait_time_estimate),
                max_wait_time: Some(infura_fees.high.max_wait_time_estimate),
            },
            source: EstimationSource::Infura,
            base_fee_trend: infura_fees.base_fee_trend,
            priority_fee_trend: infura_fees.priority_fee_trend,
        })
    }
}

/// Infura gas api provider caller
#[allow(dead_code)]
pub(crate) struct InfuraGasApiCaller {}

#[allow(dead_code)]
impl InfuraGasApiCaller {
    const INFURA_GAS_API_ENDPOINT: &'static str = "networks/";
    const INFURA_GAS_FEES_METHOD: &'static str = "suggestedGasFees";

    const INFURA_GAS_API_CHAINS: &[(&'static str, u64)] = &[
        ("ArbitrumMainnet", 42161),
        ("ArbitrumNova", 42170),
        ("AvalancheMainnet", 43114),
        ("BaseMainnet", 8453),
        ("BNBMainnet", 56),
        ("opBNBLayer2", 204),
        ("CronosMainnet", 25),
        ("EthereumMainnet", 1),
        ("EthereumHolesky", 17000),
        ("EthereumSepolia", 11155111),
        ("FantomMainnet", 250),
        ("FilecoinMainnet", 314),
        ("LineaMainnet", 59144),
        ("LineaSepolia", 59141),
        ("OptimismMainnet", 10),
        ("PolygonMainnet", 137),
        ("PolygonAmoy", 80002),
        ("ZKsyncEraMainnet", 324),
    ];

    pub(crate) fn is_chain_supported(chain_id: u64) -> bool {
        Self::INFURA_GAS_API_CHAINS.iter().any(|(_name, id)| *id == chain_id)
    }

    fn get_infura_gas_api_url(base_url: &Url, chain_id: u64) -> MmResult<(Url, HeaderParams), ParseError> {
        let url = base_url
            .join(Self::INFURA_GAS_API_ENDPOINT)?
            .join(&format!("{}/", chain_id.to_string()))?
            .join(Self::INFURA_GAS_FEES_METHOD)?;
        let headers = vec![("Authorization", INFURA_GAS_API_AUTH_TEST.as_str())];
        Ok((url, headers))
    }

    async fn make_infura_gas_api_request(url: &Url, headers: HeaderParams) -> MmResult<InfuraFeePerGas, String> {
        let resp = slurp_url_with_headers(url.as_str(), headers)
            .await
            .mm_err(|e| e.to_string())?;
        if resp.0 != StatusCode::OK {
            let error = format!("{} failed with status code {}", url, resp.0);
            return MmError::err(error);
        }
        let estimated_fees = json::from_slice(&resp.2).map_to_mm(|e| e.to_string())?;
        Ok(estimated_fees)
    }

    /// Fetch fee per gas estimations from infura provider
    pub async fn fetch_infura_fee_estimation(base_url: &Url, chain_id: u64) -> Web3RpcResult<FeePerGasEstimated> {
        let (url, headers) =
            Self::get_infura_gas_api_url(base_url, chain_id).mm_err(|err| Web3RpcError::Internal(err.to_string()))?;
        let infura_estimated_fees = Self::make_infura_gas_api_request(&url, headers)
            .await
            .mm_err(Web3RpcError::Transport)?;
        infura_estimated_fees.try_into().mm_err(Into::into)
    }
}
