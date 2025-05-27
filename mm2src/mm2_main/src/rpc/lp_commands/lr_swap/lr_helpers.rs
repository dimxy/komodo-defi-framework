use super::lr_errors::LrSwapError;
use coins::eth::{EthCoin, EthCoinType};
use coins::{lp_coinfind_or_err, MmCoinEnum};
use ethereum_types::Address as EthAddress;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use std::str::FromStr;
use trading_api::one_inch_api::client::ApiClient;

pub(crate) async fn get_coin_for_one_inch(ctx: &MmArc, ticker: &str) -> MmResult<(EthCoin, EthAddress), LrSwapError> {
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
pub(crate) fn check_if_one_inch_supports_pair(base: &EthCoin, rel: &EthCoin) -> MmResult<(), LrSwapError> {
    if !ApiClient::is_chain_supported(base.chain_id()) {
        return MmError::err(LrSwapError::ChainNotSupported);
    }
    if base.chain_id() != rel.chain_id() {
        return MmError::err(LrSwapError::DifferentChains);
    }
    Ok(())
}
