//! Code to access 1inch API

#[cfg(not(feature = "run-docker-tests"))]
use crate::one_inch_api::errors::NativeError;

#[cfg(not(feature = "run-docker-tests"))]
use common::{log, StatusCode};

#[cfg(not(feature = "run-docker-tests"))]
use mm2_err_handle::{map_mm_error::MapMmError, map_to_mm::MapToMmResult};

#[cfg(not(feature = "run-docker-tests"))]
use mm2_err_handle::mm_error::MmError;

#[cfg(not(feature = "run-docker-tests"))]
use mm2_net::transport::slurp_url_with_headers;

use super::errors::OneInchError;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::mm_error::MmResult;

use serde::de::DeserializeOwned;
use url::Url;

#[cfg(feature = "test-ext-api")]
use lazy_static::lazy_static;

#[cfg(feature = "test-ext-api")]
use common::executor::Timer;

#[cfg(feature = "test-ext-api")]
use futures::lock::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

#[cfg(any(test, feature = "for-tests"))]
use mocktopus::macros::*;

const ONE_INCH_AGGREGATION_ROUTER_CONTRACT_V6_0: &str = "0x111111125421ca6dc452d289314280a0f8842a65";
pub(crate) const ONE_INCH_ETH_SPECIAL_CONTRACT: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

#[cfg(test)]
const ONE_INCH_API_TEST_URL: &str = "https://api.1inch.dev";

#[cfg(feature = "test-ext-api")]
lazy_static! {
    /// API key for testing
    static ref ONE_INCH_API_TEST_AUTH: String = std::env::var("ONE_INCH_API_TEST_AUTH").unwrap_or_default();
}

pub(crate) type QueryParams = Vec<(&'static str, String)>;

/// 1inch v6.0 supported eth-based chains
const ONE_INCH_V6_0_SUPPORTED_CHAINS: &[(&str, u64)] = &[
    ("Ethereum", 1),
    ("Optimism", 10),
    ("BSC", 56),
    ("Gnosis", 100),
    ("Polygon", 137),
    ("Fantom", 250),
    ("ZkSync", 324),
    ("Klaytn", 8217),
    ("Base", 8453),
    ("Arbitrum", 42161),
    ("Avalanche", 43114),
    ("Aurora", 1313161554),
    #[cfg(feature = "run-docker-tests")]
    ("Geth testnet", 1337),
];

/// 1inch API basic url builder
pub struct UrlBuilder {
    base_url: Url,
    endpoint: &'static str,
    chain_id: Option<u64>,
    method_name: String,
    query_params: QueryParams,
}

impl UrlBuilder {
    /// Create new basic url builder to call 1inch API's. Normally used by specific API url builders.
    /// Note: in the classic swap API chain_id is added into url path, in portfolio chain_id is a query param so it would be optional here.
    fn new(base_url: Url, chain_id: Option<u64>, endpoint: &'static str, method_name: String) -> Self {
        Self {
            base_url,
            endpoint,
            chain_id,
            method_name,
            query_params: vec![],
        }
    }

    pub fn with_query_params(mut self, mut more_params: QueryParams) -> Self {
        self.query_params.append(&mut more_params);
        self
    }

    #[allow(clippy::result_large_err)]
    pub fn build(&self) -> MmResult<Url, OneInchError> {
        let url = self.base_url.join(self.endpoint)?;
        let url = if let Some(chain_id) = self.chain_id {
            url.join(&format!("{}/", chain_id))?
        } else {
            url
        };
        let url = url.join(self.method_name.as_str())?;
        Ok(Url::parse_with_params(
            url.as_str(),
            self.query_params
                .iter()
                .map(|v| (v.0, v.1.as_str()))
                .collect::<Vec<_>>(),
        )?)
    }
}

/// 1inch swap api methods
pub enum SwapApiMethods {
    ClassicSwapQuote,
    ClassicSwapCreate,
    LiquiditySources,
    Tokens,
}
impl SwapApiMethods {
    const SWAP_METHOD: &str = "swap";
    const QUOTE_METHOD: &str = "quote";
    const LIQUIDITY_SOURCES_METHOD: &str = "liquidity-sources";
    const TOKENS_METHOD: &str = "tokens";

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::ClassicSwapQuote => Self::QUOTE_METHOD,
            Self::ClassicSwapCreate => Self::SWAP_METHOD,
            Self::LiquiditySources => Self::LIQUIDITY_SOURCES_METHOD,
            Self::Tokens => Self::TOKENS_METHOD,
        }
    }
}

/// 1inch swap api url builder
pub struct SwapUrlBuilder;

impl SwapUrlBuilder {
    pub(crate) const CLASSIC_SWAP_ENDPOINT_V6_0: &str = "swap/v6.0/";

    #[allow(clippy::result_large_err)]
    pub fn create_api_url_builder(
        ctx: &MmArc,
        chain_id: u64,
        method: SwapApiMethods,
    ) -> MmResult<UrlBuilder, OneInchError> {
        Ok(UrlBuilder::new(
            ApiClient::base_url(ctx)?,
            Some(chain_id),
            Self::CLASSIC_SWAP_ENDPOINT_V6_0,
            method.name().to_owned(),
        ))
    }
}

pub enum PortfolioApiMethods {
    CrossPrices,
}
impl PortfolioApiMethods {
    const CROSS_PRICES_METHOD: &str = "time_range/cross_prices";

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::CrossPrices => Self::CROSS_PRICES_METHOD,
        }
    }
}

pub struct PortfolioUrlBuilder;

impl PortfolioUrlBuilder {
    pub(crate) const PORTFOLIO_PRICES_ENDPOINT_V1_0: &str = "portfolio/integrations/prices/v1/";

    #[allow(clippy::result_large_err)]
    pub fn create_api_url_builder(ctx: &MmArc, method: PortfolioApiMethods) -> MmResult<UrlBuilder, OneInchError> {
        Ok(UrlBuilder::new(
            ApiClient::base_url(ctx)?,
            None,
            Self::PORTFOLIO_PRICES_ENDPOINT_V1_0,
            method.name().to_owned(),
        ))
    }
}

/// 1-inch API caller
pub struct ApiClient;

#[allow(clippy::swap_ptr_to_ref)] // need for moctopus
#[cfg_attr(any(test, feature = "for-tests"), mockable)]
impl ApiClient {
    #[allow(unused_variables)]
    #[allow(clippy::result_large_err)]
    fn base_url(ctx: &MmArc) -> MmResult<Url, OneInchError> {
        #[cfg(not(test))]
        let url_cfg = ctx.conf["1inch_api"]
            .as_str()
            .ok_or(OneInchError::InvalidParam("No API config param".to_owned()))?;

        #[cfg(test)]
        let url_cfg = ONE_INCH_API_TEST_URL;

        Ok(Url::parse(url_cfg)?)
    }

    pub const fn eth_special_contract() -> &'static str {
        ONE_INCH_ETH_SPECIAL_CONTRACT
    } // TODO: must use the 1inch call, not a const (on zk chain it's not const)

    pub const fn classic_swap_contract() -> &'static str {
        ONE_INCH_AGGREGATION_ROUTER_CONTRACT_V6_0
    }

    pub fn is_chain_supported(chain_id: u64) -> bool {
        ONE_INCH_V6_0_SUPPORTED_CHAINS.iter().any(|(_name, id)| *id == chain_id)
    }

    #[cfg(not(feature = "run-docker-tests"))]
    fn get_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            #[cfg(feature = "test-ext-api")]
            ("Authorization", ONE_INCH_API_TEST_AUTH.as_str()),
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ]
    }

    pub async fn call_api<T>(api_url: Url) -> MmResult<T, OneInchError>
    where
        T: DeserializeOwned,
    {
        #[cfg(feature = "test-ext-api")]
        let _guard = ApiClient::one_req_per_sec().await;

        #[cfg(feature = "run-docker-tests")]
        return serde_json::from_value(super::api_mock::api_mock_entry(api_url)?).map_err(|err| {
            OneInchError::ParseBodyError {
                error_msg: err.to_string(),
            }
            .into()
        });

        #[cfg(not(feature = "run-docker-tests"))]
        ApiClient::call_api_impl(api_url).await
    }

    #[cfg(not(feature = "run-docker-tests"))]
    async fn call_api_impl<T>(api_url: Url) -> MmResult<T, OneInchError>
    where
        T: DeserializeOwned,
    {
        log::debug!("1inch call url={}", api_url.to_string());
        let (status_code, _, body) = slurp_url_with_headers(api_url.as_str(), ApiClient::get_headers())
            .await
            .mm_err(OneInchError::TransportError)?;
        log::debug!(
            "1inch response body={}",
            String::from_utf8_lossy(&body[..std::cmp::min(3072, body.len())])
        );
        if status_code != StatusCode::OK {
            // TODO: also handle text body errors like 'The limit of requests per second has been exceeded'
            let body_as_json = serde_json::from_slice(&body).map_to_mm(|err| OneInchError::ParseBodyError {
                error_msg: err.to_string(),
            })?;
            let error = NativeError::new(status_code, body_as_json);
            return Err(MmError::new(OneInchError::from_native_error(error)));
        }
        serde_json::from_slice(&body).map_err(|err| {
            OneInchError::ParseBodyError {
                error_msg: err.to_string(),
            }
            .into()
        })
    }

    /// Prevent concurrent calls
    #[cfg(feature = "test-ext-api")]
    async fn one_req_per_sec<'a>() -> AsyncMutexGuard<'a, ()> {
        lazy_static! {
            /// Lock to ensure requests to the API are not running concurrently in tests
            static ref ONE_INCH_REQ_SYNC: AsyncMutex<()> = AsyncMutex::new(());
        }
        let guard = ONE_INCH_REQ_SYNC.lock().await;
        Timer::sleep(1.).await; // ensure 1 req per sec to prevent 1inch rate limiter error for dev account
        guard
    }
}
