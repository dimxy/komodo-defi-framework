//! Provides estimations of base and priority fee per gas or fetch estimations from a gas api provider
pub mod block_native;
pub mod infura;
pub mod simple;

use futures::TryFutureExt;

use crate::eth::{EthCoin, Web3RpcError};
use block_native::BlocknativeGasApiCaller;
use common::log::debug;
use ethereum_types::U256;
use infura::InfuraGasApiCaller;
use mm2_err_handle::mm_error::MmError;
use simple::FeePerGasSimpleEstimator;
use url::Url;

use crate::eth::Web3RpcResult;

type HeaderParams = Vec<(&'static str, &'static str)>;

const FEE_PER_GAS_LEVELS: usize = 3;

/// Indicates which provider was used to get fee per gas estimations
#[derive(Clone, Debug)]
pub enum EstimationSource {
    /// filled by default values
    Empty,
    /// internal simple estimator
    Simple,
    Infura,
    Blocknative,
}

impl ToString for EstimationSource {
    fn to_string(&self) -> String {
        match self {
            EstimationSource::Empty => "empty".into(),
            EstimationSource::Simple => "simple".into(),
            EstimationSource::Infura => "infura".into(),
            EstimationSource::Blocknative => "blocknative".into(),
        }
    }
}

impl Default for EstimationSource {
    fn default() -> Self { Self::Empty }
}

enum PriorityLevelId {
    Low = 0,
    Medium = 1,
    High = 2,
}

/// Gas fee estimator options
#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub enum GasFeeEstimator {
    /// Internal tx history gas fee estimator
    #[default]
    Simple,
    Infura {
        url: Url,
    },
    Blocknative {
        url: Url,
    },
}

impl GasFeeEstimator {
    pub(crate) async fn fetch_fee_estimation(&self, coin: &EthCoin) -> Web3RpcResult<FeePerGasEstimated> {
        match self {
            Self::Infura { url } => InfuraGasApiCaller::fetch_infura_fee_estimation(url, coin.chain_id).await,
            Self::Blocknative { url } => {
                BlocknativeGasApiCaller::fetch_blocknative_fee_estimation(url, coin.chain_id).await
            },
            Self::Simple => FeePerGasSimpleEstimator::estimate_fee_by_history(coin).await,
        }
    }

    pub(crate) fn is_chain_supported(&self, chain_id: u64) -> bool {
        match self {
            Self::Infura { .. } => InfuraGasApiCaller::is_chain_supported(chain_id),
            Self::Blocknative { .. } => BlocknativeGasApiCaller::is_chain_supported(chain_id),
            Self::Simple => FeePerGasSimpleEstimator::is_chain_supported(chain_id),
        }
    }
}

pub(crate) async fn call_gas_fee_estimator(coin: &EthCoin, use_simple: bool) -> Web3RpcResult<FeePerGasEstimated> {
    println!("call_gas_fee_estimator use_simple={use_simple}");
    if !use_simple && coin.gas_fee_estimator.is_chain_supported(coin.chain_id) {
        return coin
            .gas_fee_estimator
            .fetch_fee_estimation(coin)
            .or_else(|provider_err| {
                debug!(
                    "Call to evm gas api provider failed {}, using internal fee estimator",
                    provider_err
                );
                println!(
                    "call_gas_fee_estimator falling back to simple... err={:?}",
                    provider_err
                );
                // use simple if third party estimator has failed
                FeePerGasSimpleEstimator::estimate_fee_by_history(coin).map_err(move |simple_err| {
                    MmError::new(Web3RpcError::Internal(format!(
                        "All gas api requests failed, provider error: {}, history estimator error: {}",
                        provider_err, simple_err
                    )))
                })
            })
            .await;
    }
    println!("call_gas_fee_estimator default to simple");
    FeePerGasSimpleEstimator::estimate_fee_by_history(coin).await
}

/// Priority level estimated max fee per gas
#[derive(Clone, Debug, Default)]
pub struct FeePerGasLevel {
    /// estimated max priority tip fee per gas in wei
    pub max_priority_fee_per_gas: U256,
    /// estimated max fee per gas in wei
    pub max_fee_per_gas: U256,
    /// estimated transaction min wait time in mempool in ms for this priority level
    pub min_wait_time: Option<u32>,
    /// estimated transaction max wait time in mempool in ms for this priority level
    pub max_wait_time: Option<u32>,
}

/// Internal struct for estimated fee per gas for several priority levels, in wei
/// low/medium/high levels are supported
#[derive(Default, Debug, Clone)]
pub struct FeePerGasEstimated {
    /// base fee for the next block in wei
    pub base_fee: U256,
    /// estimated low priority fee
    pub low: FeePerGasLevel,
    /// estimated medium priority fee
    pub medium: FeePerGasLevel,
    /// estimated high priority fee
    pub high: FeePerGasLevel,
    /// which estimator used
    pub source: EstimationSource,
    /// base trend (up or down)
    pub base_fee_trend: String,
    /// priority trend (up or down)
    pub priority_fee_trend: String,
}
