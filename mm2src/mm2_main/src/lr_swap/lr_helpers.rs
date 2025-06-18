use super::lr_errors::LrSwapError;
use coins::eth::{u256_to_big_decimal, wei_from_big_decimal, EthCoin, EthCoinType};
use coins::{lp_coinfind_or_err, MmCoinEnum, NumConversResult, Ticker};
use ethereum_types::{Address as EthAddress, FromDecStrErr, U256};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::TakerAction;
use std::str::FromStr;
use trading_api::one_inch_api::client::ApiClient;

pub(crate) async fn get_coin_for_one_inch(
    ctx: &MmArc,
    ticker: &Ticker,
) -> MmResult<(EthCoin, EthAddress), LrSwapError> {
    let coin = match lp_coinfind_or_err(ctx, ticker).await? {
        MmCoinEnum::EthCoin(coin) => coin,
        _ => return Err(MmError::new(LrSwapError::CoinTypeError)),
    };
    let contract = match coin.coin_type {
        EthCoinType::Eth => EthAddress::from_str(ApiClient::eth_special_contract())
            .map_to_mm(|_| LrSwapError::InternalError("invalid address".to_owned()))?,
        EthCoinType::Erc20 { token_addr, .. } => token_addr,
        EthCoinType::Nft { .. } => return Err(MmError::new(LrSwapError::NftProtocolNotSupported)),
    };
    Ok((coin, contract))
}

#[allow(clippy::result_large_err)]
pub(crate) fn check_if_one_inch_supports_pair(base_chain_id: u64, rel_chain_id: u64) -> MmResult<(), LrSwapError> {
    if !ApiClient::is_chain_supported(base_chain_id) {
        return MmError::err(LrSwapError::ChainNotSupported);
    }
    if base_chain_id != rel_chain_id {
        return MmError::err(LrSwapError::DifferentChains);
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
pub(crate) fn sell_buy_method(method: &str) -> MmResult<TakerAction, LrSwapError> {
    match method {
        "buy" => Ok(TakerAction::Buy),
        "sell" => Ok(TakerAction::Sell),
        _ => MmError::err(LrSwapError::InvalidParam(
            "invalid method in sell/buy request".to_owned(),
        )),
    }
}

#[inline]
pub(crate) fn mm_number_to_u256(mm_number: &MmNumber) -> Result<U256, FromDecStrErr> {
    U256::from_dec_str(mm_number.to_ratio().to_integer().to_string().as_str())
}

#[inline]
pub(crate) fn mm_number_from_u256(u256: U256) -> MmNumber { MmNumber::from(u256.to_string().as_str()) }

#[inline]
pub(crate) fn u256_from_coins_mm_number(mm_number: &MmNumber, decimals: u8) -> NumConversResult<U256> {
    wei_from_big_decimal(&mm_number.to_decimal(), decimals)
}

#[inline]
#[allow(unused)]
pub(crate) fn u256_to_coins_mm_number(u256: U256, decimals: u8) -> NumConversResult<MmNumber> {
    Ok(MmNumber::from(u256_to_big_decimal(u256, decimals)?))
}
