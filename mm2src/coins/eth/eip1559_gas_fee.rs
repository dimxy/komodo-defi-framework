//! Provides estimations of base and priority fee per gas or fetch estimations from a gas api provider

use super::web3_transport::FeeHistoryResult;
use super::{u256_to_big_decimal, Web3RpcError, Web3RpcResult, ETH_GWEI_DECIMALS};
use crate::EthCoin;
use ethereum_types::U256;
use mm2_err_handle::mm_error::MmError;
use mm2_err_handle::or_mm_error::OrMmError;
use mm2_number::BigDecimal;
use num_traits::FromPrimitive;
use url::Url;
use web3::types::BlockNumber;

pub(crate) use gas_api::BlocknativeGasApiCaller;
#[allow(unused_imports)]
pub(crate) use gas_api::InfuraGasApiCaller;

use gas_api::{BlocknativeBlockPricesResponse, InfuraFeePerGas};

const FEE_PER_GAS_LEVELS: usize = 3;

/// Indicates which provider was used to get fee per gas estimations
#[derive(Clone, Debug, Serialize)]
pub enum EstimationSource {
    /// filled by default values
    Empty,
    /// internal simple estimator
    Simple,
    Infura,
    Blocknative,
}

impl Default for EstimationSource {
    fn default() -> Self { Self::Empty }
}

/// Estimated fee per gas units
#[derive(Clone, Debug, Serialize)]
pub enum EstimationUnits {
    Gwei,
}

impl Default for EstimationUnits {
    fn default() -> Self { Self::Gwei }
}

enum PriorityLevelId {
    Low = 0,
    Medium = 1,
    High = 2,
}

/// Supported gas api providers
#[derive(Deserialize)]
pub enum GasApiProvider {
    Infura,
    Blocknative,
}

#[derive(Deserialize)]
pub struct GasApiConfig {
    /// gas api provider name to use
    pub provider: GasApiProvider,
    /// gas api provider or proxy base url (scheme, host and port without the relative part)
    pub url: Url,
}

/// Priority level estimated max fee per gas
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
        if block_prices.block_prices[0].estimated_prices.len() < FEE_PER_GAS_LEVELS {
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
pub(crate) struct FeePerGasSimpleEstimator {}

impl FeePerGasSimpleEstimator {
    // TODO: add minimal max fee and priority fee
    /// depth to look for fee history to estimate priority fees
    const FEE_PRIORITY_DEPTH: u64 = 5u64;

    /// percentiles to pass to eth_feeHistory
    const HISTORY_PERCENTILES: [f64; FEE_PER_GAS_LEVELS] = [25.0, 50.0, 75.0];

    /// percentile to predict next base fee over historical rewards
    const BASE_FEE_PERCENTILE: f64 = 75.0;

    /// percentiles to calc max priority fee over historical rewards
    const PRIORITY_FEE_PERCENTILES: [f64; FEE_PER_GAS_LEVELS] = [50.0, 50.0, 50.0];

    /// adjustment for max fee per gas picked up by sampling
    const ADJUST_MAX_FEE: [f64; FEE_PER_GAS_LEVELS] = [1.1, 1.175, 1.25]; // 1.25 assures max_fee_per_gas will be over next block base_fee

    /// adjustment for max priority fee picked up by sampling
    const ADJUST_MAX_PRIORITY_FEE: [f64; FEE_PER_GAS_LEVELS] = [1.0, 1.0, 1.0];

    /// block depth for eth_feeHistory
    pub fn history_depth() -> u64 { Self::FEE_PRIORITY_DEPTH }

    /// percentiles for priority rewards obtained with eth_feeHistory
    pub fn history_percentiles() -> &'static [f64] { &Self::HISTORY_PERCENTILES }

    /// percentile for vector
    fn percentile_of(v: &[U256], percent: f64) -> U256 {
        let mut v_mut = v.to_owned();
        v_mut.sort();

        // validate bounds:
        let percent = if percent > 100.0 { 100.0 } else { percent };
        let percent = if percent < 0.0 { 0.0 } else { percent };

        let value_pos = ((v_mut.len() - 1) as f64 * percent / 100.0).round() as usize;
        v_mut[value_pos]
    }

    /// Estimate simplified gas priority fees based on fee history
    pub async fn estimate_fee_by_history(coin: &EthCoin) -> Web3RpcResult<FeePerGasEstimated> {
        let res: Result<FeeHistoryResult, web3::Error> = coin
            .eth_fee_history(
                U256::from(Self::history_depth()),
                BlockNumber::Latest,
                Self::history_percentiles(),
            )
            .await;

        match res {
            Ok(fee_history) => Ok(Self::calculate_with_history(&fee_history)?),
            Err(_) => MmError::err(Web3RpcError::Internal("Eth requests failed".into())),
        }
    }

    fn predict_base_fee(base_fees: &[U256]) -> U256 { Self::percentile_of(base_fees, Self::BASE_FEE_PERCENTILE) }

    fn priority_fee_for_level(
        level: PriorityLevelId,
        base_fee: BigDecimal,
        fee_history: &FeeHistoryResult,
    ) -> Web3RpcResult<FeePerGasLevel> {
        let level_i = level as usize;
        let level_rewards = fee_history
            .priority_rewards
            .as_ref()
            .or_mm_err(|| Web3RpcError::Internal("expected reward in eth_feeHistory".into()))?
            .iter()
            .map(|rewards| {
                if level_i < rewards.len() {
                    rewards[level_i]
                } else {
                    U256::from(0)
                }
            })
            .collect::<Vec<_>>();

        let max_priority_fee_per_gas = Self::percentile_of(&level_rewards, Self::PRIORITY_FEE_PERCENTILES[level_i]);
        let max_priority_fee_per_gas =
            u256_to_big_decimal(max_priority_fee_per_gas, ETH_GWEI_DECIMALS).unwrap_or_else(|_| BigDecimal::from(0));
        let max_fee_per_gas = base_fee
            * BigDecimal::from_f64(Self::ADJUST_MAX_FEE[level_i]).unwrap_or_else(|| BigDecimal::from(0))
            + max_priority_fee_per_gas.clone()
                * BigDecimal::from_f64(Self::ADJUST_MAX_PRIORITY_FEE[level_i]).unwrap_or_else(|| BigDecimal::from(0)); // TODO maybe use checked ops
        Ok(FeePerGasLevel {
            max_priority_fee_per_gas,
            max_fee_per_gas,
            min_wait_time: None,
            max_wait_time: None, // TODO: maybe fill with some default values (and mark them as uncertain)?
        })
    }

    /// estimate priority fees by fee history
    fn calculate_with_history(fee_history: &FeeHistoryResult) -> Web3RpcResult<FeePerGasEstimated> {
        let latest_base_fee = fee_history
            .base_fee_per_gas
            .first()
            .cloned()
            .unwrap_or_else(|| U256::from(0));
        let latest_base_fee =
            u256_to_big_decimal(latest_base_fee, ETH_GWEI_DECIMALS).unwrap_or_else(|_| BigDecimal::from(0));
        let predicted_base_fee = Self::predict_base_fee(&fee_history.base_fee_per_gas);
        Ok(FeePerGasEstimated {
            base_fee: u256_to_big_decimal(predicted_base_fee, ETH_GWEI_DECIMALS)
                .unwrap_or_else(|_| BigDecimal::from(0)),
            low: Self::priority_fee_for_level(PriorityLevelId::Low, latest_base_fee.clone(), fee_history)?,
            medium: Self::priority_fee_for_level(PriorityLevelId::Medium, latest_base_fee.clone(), fee_history)?,
            high: Self::priority_fee_for_level(PriorityLevelId::High, latest_base_fee, fee_history)?,
            source: EstimationSource::Simple,
            units: EstimationUnits::Gwei,
            base_fee_trend: String::default(),
            priority_fee_trend: String::default(),
        })
    }
}

mod gas_api {
    use super::FeePerGasEstimated;
    use crate::eth::{Web3RpcError, Web3RpcResult};
    use common::log::debug;
    use http::StatusCode;
    use mm2_err_handle::mm_error::MmError;
    use mm2_err_handle::prelude::*;
    use mm2_net::transport::slurp_url_with_headers;
    use mm2_number::BigDecimal;
    use serde::de::{Deserializer, SeqAccess, Visitor};
    use serde_json::{self as json};
    use std::collections::HashMap;
    use std::fmt;
    use url::Url;

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

    /// Infura gas api provider caller
    #[allow(dead_code)]
    pub(crate) struct InfuraGasApiCaller {}

    #[allow(dead_code)]
    impl InfuraGasApiCaller {
        const INFURA_GAS_FEES_ENDPOINT: &'static str = "networks/1/suggestedGasFees"; // Support only main chain

        fn get_infura_gas_api_url(base_url: &Url) -> (Url, Vec<(&'static str, &'static str)>) {
            let mut url = base_url.clone();
            url.set_path(Self::INFURA_GAS_FEES_ENDPOINT);
            let headers = vec![("Authorization", INFURA_GAS_API_AUTH_TEST.as_str())];
            (url, headers)
        }

        async fn make_infura_gas_api_request(
            url: &Url,
            headers: Vec<(&'static str, &'static str)>,
        ) -> Result<InfuraFeePerGas, MmError<String>> {
            let resp = slurp_url_with_headers(url.as_str(), headers)
                .await
                .mm_err(|e| e.to_string())?;
            if resp.0 != StatusCode::OK {
                let error = format!("{} failed with status code {}", url, resp.0);
                debug!("infura gas api error: {}", error);
                return MmError::err(error);
            }
            let estimated_fees = json::from_slice(&resp.2).map_to_mm(|e| e.to_string())?;
            Ok(estimated_fees)
        }

        /// Fetch fee per gas estimations from infura provider
        pub async fn fetch_infura_fee_estimation(base_url: &Url) -> Web3RpcResult<FeePerGasEstimated> {
            let (url, headers) = Self::get_infura_gas_api_url(base_url);
            let infura_estimated_fees = Self::make_infura_gas_api_request(&url, headers)
                .await
                .mm_err(Web3RpcError::Transport)?;
            Ok(infura_estimated_fees.into())
        }
    }

    lazy_static! {
        /// API key for testing
        static ref BLOCKNATIVE_GAS_API_AUTH_TEST: String = std::env::var("BLOCKNATIVE_GAS_API_AUTH_TEST").unwrap_or_default();
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug, Deserialize)]
    pub(crate) struct BlocknativeBlockPrices {
        #[serde(rename = "blockNumber")]
        pub block_number: u32,
        #[serde(rename = "estimatedTransactionCount")]
        pub estimated_transaction_count: u32,
        #[serde(rename = "baseFeePerGas")]
        pub base_fee_per_gas: BigDecimal,
        #[serde(rename = "estimatedPrices")]
        pub estimated_prices: Vec<BlocknativeEstimatedPrices>,
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug, Deserialize)]
    pub(crate) struct BlocknativeEstimatedPrices {
        pub confidence: u32,
        pub price: u64,
        #[serde(rename = "maxPriorityFeePerGas")]
        pub max_priority_fee_per_gas: BigDecimal,
        #[serde(rename = "maxFeePerGas")]
        pub max_fee_per_gas: BigDecimal,
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug, Deserialize)]
    pub(crate) struct BlocknativeBaseFee {
        pub confidence: u32,
        #[serde(rename = "baseFee")]
        pub base_fee: BigDecimal,
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

    /// Blocknative gas prices response
    /// see https://docs.blocknative.com/gas-prediction/gas-platform
    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    pub(crate) struct BlocknativeBlockPricesResponse {
        pub system: String,
        pub network: String,
        pub unit: String,
        #[serde(rename = "maxPrice")]
        pub max_price: u32,
        #[serde(rename = "currentBlockNumber")]
        pub current_block_number: u32,
        #[serde(rename = "msSinceLastBlock")]
        pub ms_since_last_block: u32,
        #[serde(rename = "blockPrices")]
        pub block_prices: Vec<BlocknativeBlockPrices>,
        #[serde(
            rename = "estimatedBaseFees",
            deserialize_with = "BlocknativeEstimatedBaseFees::parse_pending"
        )]
        pub estimated_base_fees: Vec<Vec<BlocknativeBaseFee>>,
    }

    /// Blocknative gas api provider caller
    #[allow(dead_code)]
    pub(crate) struct BlocknativeGasApiCaller {}

    #[allow(dead_code)]
    impl BlocknativeGasApiCaller {
        const BLOCKNATIVE_GAS_PRICES_ENDPOINT: &'static str = "gasprices/blockprices";
        const BLOCKNATIVE_GAS_PRICES_LOW: &'static str = "10";
        const BLOCKNATIVE_GAS_PRICES_MEDIUM: &'static str = "50";
        const BLOCKNATIVE_GAS_PRICES_HIGH: &'static str = "90";

        fn get_blocknative_gas_api_url(base_url: &Url) -> (Url, Vec<(&'static str, &'static str)>) {
            let mut url = base_url.clone();
            url.set_path(Self::BLOCKNATIVE_GAS_PRICES_ENDPOINT);
            url.query_pairs_mut()
                .append_pair("confidenceLevels", Self::BLOCKNATIVE_GAS_PRICES_LOW)
                .append_pair("confidenceLevels", Self::BLOCKNATIVE_GAS_PRICES_MEDIUM)
                .append_pair("confidenceLevels", Self::BLOCKNATIVE_GAS_PRICES_HIGH)
                .append_pair("withBaseFees", "true");

            let headers = vec![("Authorization", BLOCKNATIVE_GAS_API_AUTH_TEST.as_str())];
            (url, headers)
        }

        async fn make_blocknative_gas_api_request(
            url: &Url,
            headers: Vec<(&'static str, &'static str)>,
        ) -> Result<BlocknativeBlockPricesResponse, MmError<String>> {
            let resp = slurp_url_with_headers(url.as_str(), headers)
                .await
                .mm_err(|e| e.to_string())?;
            if resp.0 != StatusCode::OK {
                let error = format!("{} failed with status code {}", url, resp.0);
                debug!("blocknative gas api error: {}", error);
                return MmError::err(error);
            }
            let block_prices = json::from_slice(&resp.2).map_err(|e| e.to_string())?;
            Ok(block_prices)
        }

        /// Fetch fee per gas estimations from blocknative provider
        pub async fn fetch_blocknative_fee_estimation(base_url: &Url) -> Web3RpcResult<FeePerGasEstimated> {
            let (url, headers) = Self::get_blocknative_gas_api_url(base_url);
            let block_prices = Self::make_blocknative_gas_api_request(&url, headers)
                .await
                .mm_err(Web3RpcError::Transport)?;
            Ok(block_prices.into())
        }
    }
}
