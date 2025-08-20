//! Mock 1inch calls

#![allow(clippy::result_large_err)]

use super::client::{PortfolioApiMethods, PortfolioUrlBuilder, SwapApiMethods, SwapUrlBuilder,
                    ONE_INCH_ETH_SPECIAL_CONTRACT};
use super::errors::OneInchError;
use coins::hd_wallet::AddrToString;
use common::log;
use common::now_sec;
use ethabi::{Contract, Token};
use ethereum_types::{Address, U256};
use lazy_static::lazy_static;
use mm2_err_handle::mm_error::{MmError, MmResult};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::str::FromStr;
use url::Url;

macro_rules! bad_param_error {
    ($p: expr) => {
        MmError::new(OneInchError::InvalidParam(format!("mock API: bad param {}", $p)))
    };
}

macro_rules! missing_param_error {
    ($p: expr) => {
        MmError::new(OneInchError::InvalidParam(format!("mock API: missing param {}", $p)))
    };
}

macro_rules! unsupported_call_error {
    ($p: expr) => {
        MmError::new(OneInchError::InvalidParam(format!("mock API: unsupported call {}", $p)))
    };
}

const ETH_DECIMALS: u32 = 18;
pub const ETH_ERC20_MOCK_PRICE: u32 = 2500;
pub const TEST_LR_SWAP_CONTRACT_ABI: &str = include_str!("../../../mm2_test_helpers/dummy_files/test_lr_swap_abi.json");
lazy_static! {
    static ref ETH_CONTRACT: Address = Address::from_str(ONE_INCH_ETH_SPECIAL_CONTRACT).unwrap();
    static ref TEST_LR_SWAP_CONTRACT: Contract = Contract::load(TEST_LR_SWAP_CONTRACT_ABI.as_bytes()).unwrap();
    static ref GETH_ERC20_CONTRACT: Address =
        Address::from_str(&std::env::var("GETH_ERC20_CONTRACT").expect("need GETH_ERC20_CONTRACT env var"))
            .unwrap_or_default();
    static ref GETH_ERC20_DECIMALS: u32 =
        u32::from_str(&std::env::var("GETH_ERC20_DECIMALS").expect("need GETH_ERC20_DECIMALS env var"))
            .unwrap_or_default();
    static ref GETH_CHAIN_ID: u64 =
        u64::from_str(&std::env::var("GETH_CHAIN_ID").expect("need GETH_CHAIN_ID env var")).unwrap_or_default();
    static ref GETH_TEST_LR_SWAP_CONTRACT: Address = Address::from_str(
        &std::env::var("GETH_TEST_LR_SWAP_CONTRACT").expect("need GETH_TEST_LR_SWAP_CONTRACT env var")
    )
    .unwrap_or_default();
}

pub fn api_mock_entry(api_url: Url) -> MmResult<Value, OneInchError> {
    log::debug!("api_mock_entry entered for {}", api_url);
    let cross_prices_path = format!(
        "/{}{}",
        PortfolioUrlBuilder::PORTFOLIO_PRICES_ENDPOINT_V1_0,
        PortfolioApiMethods::CrossPrices.name()
    );
    let quote_path = format!(
        "/{}{}/{}",
        SwapUrlBuilder::CLASSIC_SWAP_ENDPOINT_V6_0,
        *GETH_CHAIN_ID,
        SwapApiMethods::ClassicSwapQuote.name()
    );
    let swap_path = format!(
        "/{}{}/{}",
        SwapUrlBuilder::CLASSIC_SWAP_ENDPOINT_V6_0,
        *GETH_CHAIN_ID,
        SwapApiMethods::ClassicSwapCreate.name()
    );
    let resp = if api_url.path() == cross_prices_path.as_str() {
        mock_cross_prices(api_url.query_pairs().into_owned().collect())
    } else if api_url.path() == quote_path.as_str() {
        mock_quote(api_url.query_pairs().into_owned().collect())
    } else if api_url.path() == swap_path.as_str() {
        mock_swap(api_url.query_pairs().into_owned().collect())
    } else {
        Err(unsupported_call_error!(api_url.path()))
    };
    log::debug!("api_mock_entry responds with {:?}", resp);
    resp
}

/// Note: response is in token or eth units, for example: for ETH/USDT pair the price is 2500.0 USDT
fn mock_cross_prices(query_pairs: HashMap<String, String>) -> MmResult<Value, OneInchError> {
    let token0_address = query_pairs
        .get("token0_address")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("token0_address"))?;
    let token1_address = query_pairs
        .get("token1_address")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("token1_address"))?;
    if token0_address == *ETH_CONTRACT && token1_address == *GETH_ERC20_CONTRACT {
        Ok(json!(
            [{
                "timestamp": now_sec(),
                "open": ETH_ERC20_MOCK_PRICE as f64 * 0.995,
                "low": ETH_ERC20_MOCK_PRICE as f64 * 0.997,
                "avg": ETH_ERC20_MOCK_PRICE as f64 * 1.0,
                "high": ETH_ERC20_MOCK_PRICE as f64 * 1.001,
                "close": ETH_ERC20_MOCK_PRICE as f64 * 0.996,
            }]
        ))
    } else if token0_address == *GETH_ERC20_CONTRACT && token1_address == *ETH_CONTRACT {
        Ok(json!(
            [{
                "timestamp": now_sec(),
                "open": 1.0 / (ETH_ERC20_MOCK_PRICE as f64 * 0.997),
                "low": 1.0 / (ETH_ERC20_MOCK_PRICE as f64 * 1.003),
                "avg": 1.0 / (ETH_ERC20_MOCK_PRICE as f64 * 1.0),
                "high": 1.0 / (ETH_ERC20_MOCK_PRICE as f64 * 0.997),
                "close": 1.0 / (ETH_ERC20_MOCK_PRICE as f64 * 1.002),
            }]
        ))
    } else {
        Err(bad_param_error!("token0_address or token1_address"))
    }
}

fn mock_quote(query_pairs: HashMap<String, String>) -> MmResult<Value, OneInchError> {
    let src = query_pairs
        .get("src")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("src"))?;
    let dst = query_pairs
        .get("dst")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("dst"))?;
    let src_amount = query_pairs
        .get("amount")
        .map(|s| U256::from_dec_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("amount"))?;
    if src == *ETH_CONTRACT && dst == *GETH_ERC20_CONTRACT {
        Ok(json!({
            "dstAmount": calc_dst_tokens(src_amount).to_string(),
            "srcToken":{
                "address": src.addr_to_string(),
                "symbol": "ETH",
                "name": "eth dev",
                "decimals": 18,
                "eip2612": false,
                "isFoT": false,
                "tags":[]
            },
            "dstToken":{
                "address": dst.addr_to_string(),
                "symbol": "ERC20",
                "name": "erc20 dev token",
                "decimals": *GETH_ERC20_DECIMALS,
                "eip2612": true,
                "isFoT": false,
                "tags": []
            },
            "gas":230000
        }))
    } else if src == *GETH_ERC20_CONTRACT && dst == *ETH_CONTRACT {
        Ok(json!({
            "dstAmount": calc_dst_wei(src_amount).to_string(),
            "srcToken":{
                "address": src.addr_to_string(),
                "symbol": "ERC20",
                "name": "erc20 dev token",
                "decimals": *GETH_ERC20_DECIMALS,
                "eip2612": true,
                "isFoT": false,
                "tags": []
            },
            "dstToken":{
                "address": dst.addr_to_string(),
                "symbol": "ETH",
                "name": "eth dev",
                "decimals": 18,
                "eip2612": false,
                "isFoT": false,
                "tags":[]
            },
            "gas":230000
        }))
    } else {
        Err(bad_param_error!("src or dst"))
    }
}

fn mock_swap(query_pairs: HashMap<String, String>) -> MmResult<Value, OneInchError> {
    fn make_eth_to_token_tx(from: Address, value: U256, slippage: U256) -> Value {
        let function = TEST_LR_SWAP_CONTRACT.function("swapEthForTokens").unwrap();
        let data = function.encode_input(&[Token::Uint(slippage)]).unwrap();
        json!({
            "from": from.addr_to_string(),
            "to": GETH_TEST_LR_SWAP_CONTRACT.addr_to_string(),
            "data": hex::encode(data),
            "value": value.to_string(),
            "gas": 150000,
            "gasPrice": "1_000_000_000"
        })
    }

    fn make_token_to_eth_tx(from: Address, value: U256, slippage: U256) -> Value {
        let function = TEST_LR_SWAP_CONTRACT.function("swapTokensForEth").unwrap();
        let data = function
            .encode_input(&[Token::Uint(value), Token::Uint(slippage)])
            .unwrap();
        json!({
            "from": from.addr_to_string(),
            "to": GETH_TEST_LR_SWAP_CONTRACT.addr_to_string(),
            "data": hex::encode(data),
            "value": value.to_string(),
            "gas": 150000,
            "gasPrice": "1_000_000_000"
        })
    }

    let from = query_pairs
        .get("from")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("src"))?;
    let src = query_pairs
        .get("src")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("src"))?;
    let dst = query_pairs
        .get("dst")
        .map(|s| Address::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("dst"))?;
    let src_amount = query_pairs
        .get("amount")
        .map(|s| U256::from_dec_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("amount"))?; // amount in wei
    let slippage = query_pairs
        .get("slippage")
        .map(|s| f64::from_str(s).unwrap())
        .ok_or_else(|| missing_param_error!("slippage"))?; // slippage in percent
    let slippage_for_call = (slippage * 10.0) as u64; // slippage for call in 1/1000
    if src == *ETH_CONTRACT && dst == *GETH_ERC20_CONTRACT {
        Ok(json!({
            "dstAmount": calc_dst_tokens(src_amount).to_string(),
            "tx": make_eth_to_token_tx(from, src_amount, slippage_for_call.into()),
            "srcToken":{
                "address": src.addr_to_string(),
                "symbol": "ETH",
                "name": "eth dev",
                "decimals": 18,
                "eip2612": false,
                "isFoT": false,
                "tags":[]
            },
            "dstToken":{
                "address": dst.addr_to_string(),
                "symbol": "ERC20DEV",
                "name": "erc20 dev token",
                "decimals": *GETH_ERC20_DECIMALS,
                "eip2612": true,
                "isFoT": false,
                "tags":[]
            },
            "protocols":[[[]]]
        }))
    } else if src == *GETH_ERC20_CONTRACT && dst == *ETH_CONTRACT {
        Ok(json!({
            "dstAmount": calc_dst_wei(src_amount).to_string(),
            "tx": make_token_to_eth_tx(from, src_amount, slippage_for_call.into()),
            "srcToken":{
                "address": src.addr_to_string(),
                "symbol": "ERC20",
                "name": "erc20 dev token",
                "decimals": *GETH_ERC20_DECIMALS,
                "eip2612": true,
                "isFoT": false,
                "tags": []
            },
            "dstToken":{
                "address": dst.addr_to_string(),
                "symbol": "ETH",
                "name": "eth dev",
                "decimals": 18,
                "eip2612": false,
                "isFoT": false,
                "tags":[]
            }
        }))
    } else {
        Err(bad_param_error!("src or dst"))
    }
}

/// src_amount is in wei, response is in token smallest units
fn calc_dst_tokens(src_amount: U256) -> U256 {
    src_amount * U256::from(ETH_ERC20_MOCK_PRICE) * U256::from(10_u64.pow(*GETH_ERC20_DECIMALS))
        / U256::from(10_u64.pow(ETH_DECIMALS))
}

/// src_amount is in token smallest units, response is in wei
fn calc_dst_wei(src_amount: U256) -> U256 {
    src_amount * U256::from(10_u64.pow(ETH_DECIMALS))
        / (U256::from(ETH_ERC20_MOCK_PRICE) * U256::from(10_u64.pow(*GETH_ERC20_DECIMALS)))
}
