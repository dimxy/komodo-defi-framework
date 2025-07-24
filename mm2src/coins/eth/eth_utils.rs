use super::{EthCoinType, ExtractGasLimit, ETH_DECIMALS, ETH_GWEI_DECIMALS, ETH_MAX_TX_TYPE, FEE_PRIORITY_LEVEL_N};
use crate::{coin_conf, NumConversError, NumConversResult, SwapGasFeePolicy};
use ethabi::{Function, Token};
use ethereum_types::{Address, FromDecStrErr, U256};
use ethkey::{public_to_address, Public};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::MapToMmResult;
use mm2_number::{BigDecimal, MmNumber};
use secp256k1::PublicKey;
use serde_json::Value as Json;

/// Coin config parameter name for the max supported eth transaction type
const MAX_ETH_TX_TYPE_SUPPORTED: &str = "max_eth_tx_type";
/// Coin config parameter name for the eth legacy gas price multiplier
const LEGACY_GAS_PRICE_MULTIPLIER: &str = "gas_price_mult";
/// Coin config parameter name for eth gas base fee adjustment values used in the simple gas fee estimator
const GAS_BASE_FEE_MULTIPLIER: &str = "gas_base_fee_mult";
/// Coin config parameter name for eth gas max priority fee adjustment values used in the simple gas fee estimator
const GAS_PRIORITY_FEE_MULTIPLIER: &str = "gas_priority_fee_mult";
/// Coin config parameter name for the default eth swap gas fee policy
const SWAP_GAS_FEE_POLICY: &str = "swap_gas_fee_policy";

pub(crate) fn get_function_input_data(decoded: &[Token], func: &Function, index: usize) -> Result<Token, String> {
    decoded.get(index).cloned().ok_or(format!(
        "Missing input in function {}: No input found at index {}",
        func.name.clone(),
        index
    ))
}

pub(crate) fn get_function_name(name: &str, watcher_reward: bool) -> String {
    if watcher_reward {
        format!("{}{}", name, "Reward")
    } else {
        name.to_owned()
    }
}

pub fn addr_from_raw_pubkey(pubkey: &[u8]) -> Result<Address, String> {
    let pubkey = try_s!(PublicKey::from_slice(pubkey).map_err(|e| ERRL!("{:?}", e)));
    let eth_public = Public::from_slice(&pubkey.serialize_uncompressed()[1..65]);
    Ok(public_to_address(&eth_public))
}

pub fn addr_from_pubkey_str(pubkey: &str) -> Result<String, String> {
    let pubkey_bytes = try_s!(hex::decode(pubkey));
    let addr = try_s!(addr_from_raw_pubkey(&pubkey_bytes));
    Ok(format!("{:#02x}", addr))
}

pub(crate) fn display_u256_with_decimal_point(number: U256, decimals: u8) -> String {
    let mut string = number.to_string();
    let decimals = decimals as usize;
    if string.len() <= decimals {
        string.insert_str(0, &"0".repeat(decimals - string.len() + 1));
    }

    string.insert(string.len() - decimals, '.');
    string.trim_end_matches('0').into()
}

/// Converts 'number' to value with decimal point and shifts it left by 'decimals' places
pub fn u256_to_big_decimal(number: U256, decimals: u8) -> NumConversResult<BigDecimal> {
    let string = display_u256_with_decimal_point(number, decimals);
    Ok(string.parse::<BigDecimal>()?)
}

/// Shifts 'number' with decimal point right by 'decimals' places and converts it to U256 value
pub fn u256_from_big_decimal(amount: &BigDecimal, decimals: u8) -> NumConversResult<U256> {
    let mut amount = amount.to_string();
    let dot = amount.find('.');
    let decimals = decimals as usize;
    if let Some(index) = dot {
        let mut fractional = amount.split_off(index);
        // remove the dot from fractional part
        fractional.remove(0);
        if fractional.len() < decimals {
            fractional.insert_str(fractional.len(), &"0".repeat(decimals - fractional.len()));
        }
        fractional.truncate(decimals);
        amount.push_str(&fractional);
    } else {
        amount.insert_str(amount.len(), &"0".repeat(decimals));
    }
    U256::from_dec_str(&amount).map_to_mm(|e| NumConversError::new(format!("{:?}", e)))
}

/// Converts BigDecimal gwei value to wei value as U256
#[inline(always)]
pub fn wei_from_gwei_decimal(bigdec: &BigDecimal) -> NumConversResult<U256> {
    u256_from_big_decimal(bigdec, ETH_GWEI_DECIMALS)
}

/// Converts a U256 wei value to an gwei value as a BigDecimal
#[inline(always)]
pub fn wei_to_gwei_decimal(wei: U256) -> NumConversResult<BigDecimal> { u256_to_big_decimal(wei, ETH_GWEI_DECIMALS) }

/// Converts a U256 wei value to an ETH value as a BigDecimal
/// TODO: use wei_to_eth_decimal instead of u256_to_big_decimal(gas_cost_wei, ETH_DECIMALS)
#[inline(always)]
pub fn wei_to_eth_decimal(wei: U256) -> NumConversResult<BigDecimal> { u256_to_big_decimal(wei, ETH_DECIMALS) }

#[inline]
pub fn mm_number_to_u256(mm_number: &MmNumber) -> Result<U256, FromDecStrErr> {
    U256::from_dec_str(mm_number.to_ratio().to_integer().to_string().as_str())
}

#[inline]
pub fn mm_number_from_u256(u256: U256) -> MmNumber { MmNumber::from(u256.to_string().as_str()) }

#[inline]
pub fn wei_from_coins_mm_number(mm_number: &MmNumber, decimals: u8) -> NumConversResult<U256> {
    u256_from_big_decimal(&mm_number.to_decimal(), decimals)
}

#[inline]
#[allow(unused)]
pub fn wei_to_coins_mm_number(u256: U256, decimals: u8) -> NumConversResult<MmNumber> {
    Ok(MmNumber::from(u256_to_big_decimal(u256, decimals)?))
}

/// Get "max_eth_tx_type" param from a token conf, or from the platform coin conf
fn get_conf_param_or_from_plaform(ctx: &MmArc, conf: &Json, param: &str, coin_type: &EthCoinType) -> Option<Json> {
    match &coin_type {
        EthCoinType::Eth => conf.get(param).cloned(),
        EthCoinType::Erc20 { platform, .. } | EthCoinType::Nft { platform } => {
            conf.get(param)
                .cloned()
                .or(coin_conf(ctx, platform).get(param).cloned())
        },
    }
}

/// Get "max_eth_tx_type" param from a token conf, or from the platform coin conf
pub(super) fn get_max_eth_tx_type_conf(
    ctx: &MmArc,
    conf: &Json,
    coin_type: &EthCoinType,
) -> Result<Option<u64>, String> {
    match get_conf_param_or_from_plaform(ctx, conf, MAX_ETH_TX_TYPE_SUPPORTED, coin_type) {
        Some(val) => {
            let max_eth_tx_type = val
                .as_u64()
                .ok_or_else(|| format!("{MAX_ETH_TX_TYPE_SUPPORTED} in coins is invalid"))?;
            if max_eth_tx_type > ETH_MAX_TX_TYPE {
                return Err(format!("{MAX_ETH_TX_TYPE_SUPPORTED} in coins is too big"));
            }
            Ok(Some(max_eth_tx_type))
        },
        None => Ok(None),
    }
}

/// Get "gas_price_mult" param from a token conf, or from the platform coin conf
pub(super) fn get_gas_price_mult_conf(
    ctx: &MmArc,
    conf: &Json,
    coin_type: &EthCoinType,
) -> Result<Option<f64>, String> {
    match get_conf_param_or_from_plaform(ctx, conf, LEGACY_GAS_PRICE_MULTIPLIER, coin_type) {
        Some(val) => {
            let gas_price_mult = val
                .as_f64()
                .ok_or_else(|| format!("{LEGACY_GAS_PRICE_MULTIPLIER} in coins is invalid"))?;
            if gas_price_mult <= 0.0 {
                return Err(format!("{LEGACY_GAS_PRICE_MULTIPLIER} in coins is negative"));
            }
            Ok(Some(gas_price_mult))
        },
        None => Ok(None),
    }
}

/// Get "gas_fee_base_adjust" param from a token conf, or from the platform coin conf
pub(super) fn get_gas_base_fee_mult_conf(
    ctx: &MmArc,
    conf: &Json,
    coin_type: &EthCoinType,
) -> Result<Option<Vec<f64>>, String> {
    match get_conf_param_or_from_plaform(ctx, conf, GAS_BASE_FEE_MULTIPLIER, coin_type) {
        Some(val) => {
            let gas_base_fee_mult = val
                .as_array()
                .ok_or_else(|| format!("{GAS_BASE_FEE_MULTIPLIER} in coins not an array"))?;
            if gas_base_fee_mult.len() != FEE_PRIORITY_LEVEL_N {
                return Err(format!("{GAS_BASE_FEE_MULTIPLIER} in coins has invalid size"));
            }
            let gas_base_fee_mult: Result<Vec<f64>, _> = gas_base_fee_mult
                .iter()
                .map(|v| {
                    v.as_f64()
                        .ok_or_else(|| format!("{GAS_BASE_FEE_MULTIPLIER} in coins has invalid value"))
                })
                .collect();
            let gas_base_fee_mult = gas_base_fee_mult?;
            Ok(Some(gas_base_fee_mult))
        },
        None => Ok(None),
    }
}

/// Get "gas_fee_priority_adjust" param from a token conf, or from the platform coin conf
pub(super) fn get_gas_priority_fee_mult_conf(
    ctx: &MmArc,
    conf: &Json,
    coin_type: &EthCoinType,
) -> Result<Option<Vec<f64>>, String> {
    match get_conf_param_or_from_plaform(ctx, conf, GAS_PRIORITY_FEE_MULTIPLIER, coin_type) {
        Some(val) => {
            let gas_priority_fee_mult = val
                .as_array()
                .ok_or_else(|| format!("{GAS_PRIORITY_FEE_MULTIPLIER} in coins not an array"))?;
            if gas_priority_fee_mult.len() != FEE_PRIORITY_LEVEL_N {
                return Err(format!("{GAS_PRIORITY_FEE_MULTIPLIER} in coins has invalid size"));
            }
            let gas_priority_fee_mult: Result<Vec<f64>, _> = gas_priority_fee_mult
                .iter()
                .map(|v| {
                    v.as_f64()
                        .ok_or_else(|| format!("{GAS_PRIORITY_FEE_MULTIPLIER} in coins has invalid value"))
                })
                .collect();
            let gas_priority_fee_mult = gas_priority_fee_mult?;
            Ok(Some(gas_priority_fee_mult))
        },
        None => Ok(None),
    }
}

/// Get "swap_gas_fee_policy" param from the platform coin conf
pub(super) fn get_swap_gas_fee_policy_conf(
    ctx: &MmArc,
    conf: &Json,
    coin_type: &EthCoinType,
) -> Result<Option<SwapGasFeePolicy>, String> {
    match get_conf_param_or_from_plaform(ctx, conf, SWAP_GAS_FEE_POLICY, coin_type) {
        Some(val) => {
            let swap_gas_fee_policy: SwapGasFeePolicy =
                serde_json::from_value(val).map_err(|_| format!("{SWAP_GAS_FEE_POLICY} in coins is invalid"))?;
            Ok(Some(swap_gas_fee_policy))
        },
        None => Ok(None),
    }
}

pub(super) fn extract_gas_limit_from_conf<T: ExtractGasLimit>(coin_conf: &Json) -> Result<T, String> {
    Ok(coin_conf
        .get(T::key())
        .cloned()
        .map(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_default())
}
