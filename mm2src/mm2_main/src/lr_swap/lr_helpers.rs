use super::lr_errors::LrSwapError;
use coins::{lp_coinfind_or_err, MmCoinEnum, Ticker};
use coins::eth::{EthCoin, EthCoinType};
use ethereum_types::Address as EthAddress;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_rpc::data::legacy::{SellBuyRequest, TakerAction};
use mm2_number::MmNumber;
use std::str::FromStr;
use trading_api::one_inch_api::client::ApiClient;

pub(crate) async fn get_coin_for_one_inch(ctx: &MmArc, ticker: &Ticker) -> MmResult<(EthCoin, EthAddress), LrSwapError> {
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
pub(crate) fn maker_coin_from_req(sell_buy_req: &SellBuyRequest) -> MmResult<Ticker, LrSwapError> {
    match sell_buy_method(sell_buy_req)? {
        TakerAction::Buy => Ok(sell_buy_req.base.clone()),
        TakerAction::Sell => Ok(sell_buy_req.rel.clone()),
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn taker_coin_from_req(sell_buy_req: &SellBuyRequest) -> MmResult<Ticker, LrSwapError> {
    match sell_buy_method(sell_buy_req)? {
        TakerAction::Buy => Ok(sell_buy_req.rel.clone()),
        TakerAction::Sell => Ok(sell_buy_req.base.clone()),
    }
}

#[allow(clippy::result_large_err)]
pub(crate) fn sell_buy_method(sell_buy_req: &SellBuyRequest) -> MmResult<TakerAction, LrSwapError> {
    match sell_buy_req.method.as_str() {
        "buy" => Ok(TakerAction::Buy),
        "sell" => Ok(TakerAction::Sell),
        _ => MmError::err(LrSwapError::InvalidParam(
            "invalid method in sell/buy request".to_owned(),
        )),
    }
}

/*
/// Maker volume as atomic swap maker volume 
#[allow(clippy::result_large_err)]
pub(crate) fn maker_volume_from_req(sell_buy_req: &SellBuyRequest) -> MmResult<MmNumber, LrSwapError> {
    match sell_buy_method(sell_buy_req)? {
        TakerAction::Buy => Ok(sell_buy_req.volume.clone()),
        TakerAction::Sell => Ok(&sell_buy_req.volume * &sell_buy_req.price),
    }
}*/

/// Taker volume as atomic swap taker volume 
#[allow(clippy::result_large_err)]
pub(crate) fn taker_volume_from_req(sell_buy_req: &SellBuyRequest) -> MmResult<MmNumber, LrSwapError> {
    match sell_buy_method(sell_buy_req)? {
        TakerAction::Buy => Ok(&sell_buy_req.volume * &sell_buy_req.price),
        TakerAction::Sell => Ok(sell_buy_req.volume.clone()),
    }
}
