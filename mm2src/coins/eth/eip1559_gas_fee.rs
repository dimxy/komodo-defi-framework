use super::web3_transport::{EthFeeHistoryNamespace, FeeHistoryResult};
use super::{u256_to_big_decimal, Web3RpcError, Web3RpcResult, ETH_GWEI_DECIMALS};
use common::log::info;
use ethereum_types::U256;
use http::StatusCode;
use mm2_err_handle::mm_error::MmError;
use mm2_err_handle::prelude::*;
use mm2_net::transport::slurp_url_with_headers;
use mm2_number::BigDecimal;
use num_traits::FromPrimitive;
use serde::de::{Deserializer, SeqAccess, Visitor};
use serde_json::{self as json};
use std::collections::HashMap;
use std::fmt;
use web3::{types::BlockNumber, Transport};

// Estimate base and priority fee per gas
// or fetch estimations from a gas api provider

const FEE_PER_GAS_LEVELS: usize = 3;

#[derive(Clone, Debug, Serialize)]
pub enum EstimationSource {
    /// filled by default values
    Empty,
    Simple,
    Infura,
    Blocknative,
}

impl Default for EstimationSource {
    fn default() -> Self { Self::Empty }
}

#[derive(Clone, Debug, Serialize)]
pub enum EstimationUnits {
    Gwei,
}

impl Default for EstimationUnits {
    fn default() -> Self { Self::Gwei }
}

/// One priority level estimated max fees
#[derive(Clone, Debug, Serialize)]
pub struct FeePerGasLevel {
    /// estimated max priority tip fee per gas in gwei
    pub max_priority_fee_per_gas: BigDecimal,
    /// estimated max fee per gas in gwei
    pub max_fee_per_gas: BigDecimal,
    /// estimated transaction min wait time in mempool in ms for this priority level
    pub min_wait_time: Option<u32>,
    /// estimated transaction max wait time in mempool in ms for this priority level
    pub max_wait_time: Option<u32>,
}

/// Estimated gas price for several priority levels
/// we support low/medium/high levels as we can use api providers which normally support such levels
#[derive(Default, Debug, Clone, Serialize)]
pub struct FeePerGasEstimated {
    /// base fee for the next block in gwei
    pub base_fee: BigDecimal,
    /// estimated low priority fee
    pub low: FeePerGasLevel,
    /// estimated medium priority fee
    pub medium: FeePerGasLevel,
    /// estimated high priority fee
    pub high: FeePerGasLevel,
    /// which estimator used
    pub source: EstimationSource,
    /// fee units
    pub units: EstimationUnits,
    /// base trend (up or down)
    pub base_fee_trend: String,
    /// priority trend (up or down)
    pub priority_fee_trend: String,
}

impl Default for FeePerGasLevel {
    fn default() -> Self {
        Self {
            max_priority_fee_per_gas: BigDecimal::from(0),
            max_fee_per_gas: BigDecimal::from(0),
            min_wait_time: None,
            max_wait_time: None,
        }
    }
}

impl From<InfuraFeePerGas> for FeePerGasEstimated {
    fn from(infura_fees: InfuraFeePerGas) -> Self {
        Self {
            base_fee: infura_fees.estimated_base_fee,
            low: FeePerGasLevel {
                max_fee_per_gas: infura_fees.low.suggested_max_fee_per_gas,
                max_priority_fee_per_gas: infura_fees.low.suggested_max_priority_fee_per_gas,
                min_wait_time: Some(infura_fees.low.min_wait_time_estimate),
                max_wait_time: Some(infura_fees.low.max_wait_time_estimate),
            },
            medium: FeePerGasLevel {
                max_fee_per_gas: infura_fees.medium.suggested_max_fee_per_gas,
                max_priority_fee_per_gas: infura_fees.medium.suggested_max_priority_fee_per_gas,
                min_wait_time: Some(infura_fees.medium.min_wait_time_estimate),
                max_wait_time: Some(infura_fees.medium.max_wait_time_estimate),
            },
            high: FeePerGasLevel {
                max_fee_per_gas: infura_fees.high.suggested_max_fee_per_gas,
                max_priority_fee_per_gas: infura_fees.high.suggested_max_priority_fee_per_gas,
                min_wait_time: Some(infura_fees.high.min_wait_time_estimate),
                max_wait_time: Some(infura_fees.high.max_wait_time_estimate),
            },
            source: EstimationSource::Infura,
            units: EstimationUnits::Gwei,
            base_fee_trend: infura_fees.base_fee_trend,
            priority_fee_trend: infura_fees.priority_fee_trend,
        }
    }
}

impl From<BlocknativeBlockPricesResponse> for FeePerGasEstimated {
    fn from(block_prices: BlocknativeBlockPricesResponse) -> Self {
        if block_prices.block_prices.is_empty() {
            return FeePerGasEstimated::default();
        }
        if block_prices.block_prices[0].estimated_prices.len() < 3 {
            return FeePerGasEstimated::default();
        }
        Self {
            base_fee: block_prices.block_prices[0].base_fee_per_gas.clone(),
            low: FeePerGasLevel {
                max_fee_per_gas: block_prices.block_prices[0].estimated_prices[2].max_fee_per_gas.clone(),
                max_priority_fee_per_gas: block_prices.block_prices[0].estimated_prices[2]
                    .max_priority_fee_per_gas
                    .clone(),
                min_wait_time: None,
                max_wait_time: None,
            },
            medium: FeePerGasLevel {
                max_fee_per_gas: block_prices.block_prices[0].estimated_prices[1].max_fee_per_gas.clone(),
                max_priority_fee_per_gas: block_prices.block_prices[0].estimated_prices[1]
                    .max_priority_fee_per_gas
                    .clone(),
                min_wait_time: None,
                max_wait_time: None,
            },
            high: FeePerGasLevel {
                max_fee_per_gas: block_prices.block_prices[0].estimated_prices[0].max_fee_per_gas.clone(),
                max_priority_fee_per_gas: block_prices.block_prices[0].estimated_prices[0]
                    .max_priority_fee_per_gas
                    .clone(),
                min_wait_time: None,
                max_wait_time: None,
            },
            source: EstimationSource::Blocknative,
            units: EstimationUnits::Gwei,
            base_fee_trend: String::default(),
            priority_fee_trend: String::default(),
        }
    }
}

/// Simple priority fee per gas estimator based on fee history
/// normally used if gas api provider is not available
pub(super) struct FeePerGasSimpleEstimator {}

impl FeePerGasSimpleEstimator {
    // TODO: add minimal max fee and priority fee
    /// depth to look for fee history to estimate priority fees
    const FEE_PRIORITY_DEPTH: u64 = 5u64;

    /// percentiles to pass to eth_feeHistory
    const HISTORY_PERCENTILES: [f64; FEE_PER_GAS_LEVELS] = [25.0, 50.0, 75.0];

    /// percentiles to calc max priority fee over historical rewards
    const CALC_PERCENTILES: [f64; FEE_PER_GAS_LEVELS] = [50.0, 50.0, 50.0];

    /// adjustment for max priority fee picked up by sampling
    const ADJUST_MAX_FEE: [f64; FEE_PER_GAS_LEVELS] = [1.0, 1.0, 1.0];

    /// adjustment for max priority fee picked up by sampling
    const ADJUST_MAX_PRIORITY_FEE: [f64; FEE_PER_GAS_LEVELS] = [1.0, 1.0, 1.0];

    /// block depth for eth_feeHistory
    pub fn history_depth() -> u64 { Self::FEE_PRIORITY_DEPTH }

    /// percentiles for priority rewards obtained with eth_feeHistory
    pub fn history_percentiles() -> &'static [f64] { &Self::HISTORY_PERCENTILES }

    /// percentile for vector
    fn percentile_of(v: &mut Vec<U256>, percent: f64) -> U256 {
        v.sort();

        // validate bounds:
        let percent = if percent > 100.0 { 100.0 } else { percent };
        let percent = if percent < 0.0 { 0.0 } else { percent };

        let value_pos = ((v.len() - 1) as f64 * percent / 100.0).round() as usize;
        v[value_pos]
    }

    /// Estimate simplified gas priority fees based on fee history
    pub async fn estimate_fee_by_history<T: Transport>(
        fee_history_namespace: EthFeeHistoryNamespace<T>,
    ) -> Web3RpcResult<FeePerGasEstimated> {
        let res = fee_history_namespace
            .eth_fee_history(
                U256::from(Self::history_depth()),
                BlockNumber::Latest,
                Self::history_percentiles(),
            )
            .await;

        match res {
            Ok(fee_history) => Ok(Self::calculate_with_history(&fee_history)),
            Err(_) => MmError::err(Web3RpcError::Internal("Eth requests failed".into())),
        }
    }

    /// estimate priority fees by fee history
    fn calculate_with_history(fee_history: &FeeHistoryResult) -> FeePerGasEstimated {
        let base_fee = *fee_history.base_fee_per_gas.first().unwrap_or(&U256::from(0));
        let base_fee = u256_to_big_decimal(base_fee, ETH_GWEI_DECIMALS).unwrap_or_else(|_| BigDecimal::from(0));
        let mut priority_fees = vec![];
        for i in 0..Self::HISTORY_PERCENTILES.len() {
            let mut level_rewards = fee_history
                .priority_rewards
                .iter()
                .map(|rewards| if i < rewards.len() { rewards[i] } else { U256::from(0) })
                .collect::<Vec<_>>();

            let max_priority_fee_per_gas = Self::percentile_of(&mut level_rewards, Self::CALC_PERCENTILES[i]);
            let max_priority_fee_per_gas = u256_to_big_decimal(max_priority_fee_per_gas, ETH_GWEI_DECIMALS)
                .unwrap_or_else(|_| BigDecimal::from(0));
            let max_fee_per_gas = base_fee.clone()
                * BigDecimal::from_f64(Self::ADJUST_MAX_FEE[i]).unwrap_or_else(|| BigDecimal::from(0))
                + max_priority_fee_per_gas.clone()
                    * BigDecimal::from_f64(Self::ADJUST_MAX_PRIORITY_FEE[i]).unwrap_or_else(|| BigDecimal::from(0)); // TODO maybe use checked ops
            let priority_fee = FeePerGasLevel {
                max_priority_fee_per_gas,
                max_fee_per_gas,
                min_wait_time: None,
                max_wait_time: None, // TODO: maybe fill with some default values (and mark as uncertain)?
            };
            priority_fees.push(priority_fee);
        }
        drop_mutability!(priority_fees);
        FeePerGasEstimated {
            base_fee,
            low: priority_fees[0].clone(),
            medium: priority_fees[1].clone(),
            high: priority_fees[2].clone(),
            source: EstimationSource::Simple,
            units: EstimationUnits::Gwei,
            base_fee_trend: String::default(),
            priority_fee_trend: String::default(),
        }
    }
}

// Infura provider caller:

#[derive(Clone, Debug, Deserialize)]
struct InfuraFeePerGasLevel {
    #[serde(rename = "suggestedMaxPriorityFeePerGas")]
    suggested_max_priority_fee_per_gas: BigDecimal,
    #[serde(rename = "suggestedMaxFeePerGas")]
    suggested_max_fee_per_gas: BigDecimal,
    #[serde(rename = "minWaitTimeEstimate")]
    min_wait_time_estimate: u32,
    #[serde(rename = "maxWaitTimeEstimate")]
    max_wait_time_estimate: u32,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct InfuraFeePerGas {
    low: InfuraFeePerGasLevel,
    medium: InfuraFeePerGasLevel,
    high: InfuraFeePerGasLevel,
    #[serde(rename = "estimatedBaseFee")]
    estimated_base_fee: BigDecimal,
    #[serde(rename = "networkCongestion")]
    network_congestion: BigDecimal,
    #[serde(rename = "latestPriorityFeeRange")]
    latest_priority_fee_range: Vec<BigDecimal>,
    #[serde(rename = "historicalPriorityFeeRange")]
    historical_priority_fee_range: Vec<BigDecimal>,
    #[serde(rename = "historicalBaseFeeRange")]
    historical_base_fee_range: Vec<BigDecimal>,
    #[serde(rename = "priorityFeeTrend")]
    priority_fee_trend: String, // we are not using enum here bcz values not mentioned in docs could be received
    #[serde(rename = "baseFeeTrend")]
    base_fee_trend: String,
}

lazy_static! {
    static ref INFURA_GAS_API_AUTH_TEST: String = std::env::var("INFURA_GAS_API_AUTH_TEST").unwrap_or_default();
}

#[allow(dead_code)]
pub(super) struct InfuraGasApiCaller {}

#[allow(dead_code)]
impl InfuraGasApiCaller {
    const INFURA_GAS_API_URL: &'static str = "https://gas.api.infura.io/networks/1";
    const INFURA_GAS_FEES_CALL: &'static str = "suggestedGasFees";

    fn get_infura_gas_api_url() -> (String, Vec<(&'static str, &'static str)>) {
        let url = format!("{}/{}", Self::INFURA_GAS_API_URL, Self::INFURA_GAS_FEES_CALL);
        let headers = vec![("Authorization", INFURA_GAS_API_AUTH_TEST.as_str())];
        (url, headers)
    }

    async fn make_infura_gas_api_request() -> Result<InfuraFeePerGas, MmError<String>> {
        let (url, headers) = Self::get_infura_gas_api_url();
        let resp = slurp_url_with_headers(&url, headers).await.mm_err(|e| e.to_string())?;
        if resp.0 != StatusCode::OK {
            let error = format!("{} failed with status code {}", Self::INFURA_GAS_FEES_CALL, resp.0);
            info!("gas api error: {}", error);
            return MmError::err(error);
        }
        let estimated_fees = json::from_slice(&resp.2).map_err(|e| e.to_string())?;
        Ok(estimated_fees)
    }

    /// Fetch api provider gas fee estimations
    pub async fn fetch_infura_fee_estimation() -> Web3RpcResult<FeePerGasEstimated> {
        let infura_estimated_fees = Self::make_infura_gas_api_request()
            .await
            .mm_err(Web3RpcError::Transport)?;
        Ok(infura_estimated_fees.into())
    }
}

// Blocknative provider caller

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
struct BlocknativeBlockPrices {
    #[serde(rename = "blockNumber")]
    block_number: u32,
    #[serde(rename = "estimatedTransactionCount")]
    estimated_transaction_count: u32,
    #[serde(rename = "baseFeePerGas")]
    base_fee_per_gas: BigDecimal,
    #[serde(rename = "estimatedPrices")]
    estimated_prices: Vec<BlocknativeEstimatedPrices>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
struct BlocknativeEstimatedPrices {
    confidence: u32,
    price: u64,
    #[serde(rename = "maxPriorityFeePerGas")]
    max_priority_fee_per_gas: BigDecimal,
    #[serde(rename = "maxFeePerGas")]
    max_fee_per_gas: BigDecimal,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
struct BlocknativeBaseFee {
    confidence: u32,
    #[serde(rename = "baseFee")]
    base_fee: BigDecimal,
}

struct BlocknativeEstimatedBaseFees {}

impl BlocknativeEstimatedBaseFees {
    /// Parse blocknative's base_fees in pending blocks : '[ "pending+1" : {}, "pending+2" : {}, ..]' removing 'pending+n'
    fn parse_pending<'de, D>(deserializer: D) -> Result<Vec<Vec<BlocknativeBaseFee>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PendingBlockFeeParser;
        impl<'de> Visitor<'de> for PendingBlockFeeParser {
            type Value = Vec<Vec<BlocknativeBaseFee>>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("[u32, BigDecimal]")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut pending_block_fees = Vec::<Vec<BlocknativeBaseFee>>::new();
                while let Some(fees) = seq.next_element::<HashMap<String, Vec<BlocknativeBaseFee>>>()? {
                    if let Some(fees) = fees.iter().next() {
                        pending_block_fees.push(fees.1.clone());
                    }
                }
                Ok(pending_block_fees)
            }
        }
        deserializer.deserialize_any(PendingBlockFeeParser {})
    }
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BlocknativeBlockPricesResponse {
    system: String,
    network: String,
    unit: String,
    #[serde(rename = "maxPrice")]
    max_price: u32,
    #[serde(rename = "currentBlockNumber")]
    current_block_number: u32,
    #[serde(rename = "msSinceLastBlock")]
    ms_since_last_block: u32,
    #[serde(rename = "blockPrices")]
    block_prices: Vec<BlocknativeBlockPrices>,
    #[serde(
        rename = "estimatedBaseFees",
        deserialize_with = "BlocknativeEstimatedBaseFees::parse_pending"
    )]
    estimated_base_fees: Vec<Vec<BlocknativeBaseFee>>,
}

lazy_static! {
    static ref BLOCKNATIVE_GAS_API_AUTH_TEST: String =
        std::env::var("BLOCKNATIVE_GAS_API_AUTH_TEST").unwrap_or_default();
}

#[allow(dead_code)]
pub(super) struct BlocknativeGasApiCaller {}

#[allow(dead_code)]
impl BlocknativeGasApiCaller {
    const BLOCKNATIVE_GAS_API_URL: &'static str = "https://api.blocknative.com/gasprices";
    const BLOCKNATIVE_GAS_PRICES_CALL: &'static str = "blockprices";
    const BLOCKNATIVE_GAS_PRICES_PARAMS: &'static str =
        "confidenceLevels=10&confidenceLevels=50&confidenceLevels=90&withBaseFees=true";

    fn get_blocknative_gas_api_url() -> (String, Vec<(&'static str, &'static str)>) {
        let url = format!(
            "{}/{}?{}",
            Self::BLOCKNATIVE_GAS_API_URL,
            Self::BLOCKNATIVE_GAS_PRICES_CALL,
            Self::BLOCKNATIVE_GAS_PRICES_PARAMS
        );
        let headers = vec![("Authorization", BLOCKNATIVE_GAS_API_AUTH_TEST.as_str())];
        (url, headers)
    }

    async fn make_blocknative_gas_api_request() -> Result<BlocknativeBlockPricesResponse, MmError<String>> {
        let (url, headers) = Self::get_blocknative_gas_api_url();
        let resp = slurp_url_with_headers(&url, headers).await.mm_err(|e| e.to_string())?;
        if resp.0 != StatusCode::OK {
            let error = format!(
                "{} failed with status code {}",
                Self::BLOCKNATIVE_GAS_PRICES_CALL,
                resp.0
            );
            info!("gas api error: {}", error);
            return MmError::err(error);
        }
        let block_prices = json::from_slice(&resp.2).map_err(|e| e.to_string())?;
        Ok(block_prices)
    }

    /// Fetch api provider gas fee estimations
    pub async fn fetch_blocknative_fee_estimation() -> Web3RpcResult<FeePerGasEstimated> {
        let block_prices = Self::make_blocknative_gas_api_request()
            .await
            .mm_err(Web3RpcError::Transport)?;
        Ok(block_prices.into())
    }
}
