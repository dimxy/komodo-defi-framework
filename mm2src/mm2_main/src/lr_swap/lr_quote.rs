//! Implementation for finding the best-priced quote for a Taker swap with liquidity routing (LR).
//! A swap with LR may have interim conversion (LR swap) of a source token into a token needed to send in the atomic swap,
//! or, conversion of a received atomic swap token into a destination token.
//! LR currently is supported for EVM chains.

use super::lr_errors::LrSwapError;
use super::lr_helpers::get_coin_for_one_inch;
use crate::lp_swap::taker_swap::TakerSwapPreparedParams;
use crate::lr_swap::ClassicSwapDataExt;
use crate::rpc::lp_commands::lr_swap_api::lr_api_types::{AskOrBidOrder, AsksForCoin, BidsForCoin};
use coins::eth::{mm_number_from_u256, mm_number_to_u256, u256_from_coins_mm_number, u256_to_coins_mm_number, EthCoin};
use coins::hd_wallet::AddrToString;
use coins::{lp_coinfind_or_err, MarketCoinOps};
use coins::{DexFee, MmCoin, Ticker};
use common::log;
use ethereum_types::{Address as EthAddress, U256};
use futures::future::join_all;
use futures::Future as Future03;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::TakerAction;
use num_traits::CheckedDiv;
use std::collections::HashMap;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use trading_api::one_inch_api::classic_swap_types::{ClassicSwapData, ClassicSwapQuoteCallBuilder};
use trading_api::one_inch_api::client::{ApiClient, PortfolioApiMethods, PortfolioUrlBuilder, SwapApiMethods,
                                        SwapUrlBuilder};
use trading_api::one_inch_api::errors::OneInchError;
use trading_api::one_inch_api::portfolio_types::{CrossPriceParams, CrossPricesSeries, DataGranularity};

/// To estimate src/dst price query price history for last 5 min
const CROSS_PRICES_GRANULARITY: DataGranularity = DataGranularity::FiveMin;
/// Use no more than 1 price history samples to estimate src/dst price
const CROSS_PRICES_LIMIT: u32 = 1;

/// Internal struct to collect data for LR step
#[allow(dead_code)] // 'Clone' is detected as dead code in one combinator
#[derive(Clone)]
struct LrStepData {
    /// Source coin or token ticker (to swap from)
    _src_token: Ticker,
    /// Source token contract address
    src_contract: Option<EthAddress>,
    /// Source token amount (estimated) in smallest units
    src_amount: Option<U256>,
    /// Source token decimals
    src_decimals: Option<u8>,
    /// Destination coin or token ticker (to swap into)
    _dst_token: Ticker,
    /// Destination token contract address
    dst_contract: Option<EthAddress>,
    /// Destination token amount in smallest units
    dst_amount: Option<U256>,
    /// Destination token decimals
    dst_decimals: Option<u8>,
    /// Chain id where LR swap occurs (obtained from the destination token)
    chain_id: Option<u64>,
    /// Estimated src token / dst token price
    lr_price: Option<MmNumber>,
    /// Estimated dex fee and taker fee amounts to include in LR step quote
    /// TODO: return in the rpc result
    taker_swap_params: Option<TakerSwapPreparedParams>,
    /// Dex fee added to the source amount, needed to pay in the atomic swap
    dex_fee: Option<MmNumber>,
    /// A quote from LR provider with destination amount for the LR step
    lr_swap_data: Option<ClassicSwapData>,
}

impl LrStepData {
    fn new(src_ticker: Ticker, dst_ticker: Ticker) -> Self {
        LrStepData {
            _src_token: src_ticker,
            src_contract: None,
            src_decimals: None,
            src_amount: None,
            _dst_token: dst_ticker,
            dst_contract: None,
            dst_amount: None,
            dst_decimals: None,
            chain_id: None,
            lr_price: None,
            taker_swap_params: None,
            dex_fee: None,
            lr_swap_data: None,
        }
    }

    #[allow(clippy::result_large_err)]
    fn get_chain_contract_info(&self) -> MmResult<(String, String, u64), LrSwapError> {
        let src_contract = self
            .src_contract
            .as_ref()
            .ok_or(LrSwapError::InternalError("Source LR contract not set".to_owned()))?
            .addr_to_string();
        let dst_contract = self
            .dst_contract
            .as_ref()
            .ok_or(LrSwapError::InternalError("Destination LR contract not set".to_owned()))?
            .addr_to_string();
        let chain_id = self
            .chain_id
            .ok_or(LrSwapError::InternalError("LR chain id not set".to_owned()))?;
        Ok((src_contract, dst_contract, chain_id))
    }

    /*
    /// Add dex fee and trade fees an amount
    fn add_fees_to_amount(mut amount: MmNumber, token: &Ticker, taker_swap_params: &TakerSwapPreparedParams) -> MmResult<MmNumber, LrSwapError> {
        amount += &taker_swap_params.dex_fee;
        // add trade fee if platform coin
        if token == &taker_swap_params.fee_to_send_dex_fee.coin {
            amount += &taker_swap_params.fee_to_send_dex_fee.amount;
        }
        if token == &taker_swap_params.taker_payment_trade_fee.coin {
            amount += &taker_swap_params.taker_payment_trade_fee.amount;
        }
        Ok(amount)
    }*/
}

struct LrSwapCandidateInfo {
    /// Data for liquidity routing before atomic swap
    lr_data_0: Option<LrStepData>,
    /// Atomic swap order to fill
    maker_order: AskOrBidOrder,
    /// Data for liquidity routing after atomic swap
    lr_data_1: Option<LrStepData>,
}

type LrSwapCandidateInfoMap = HashMap<(Ticker, Ticker), Arc<RwLock<LrSwapCandidateInfo>>>;

/// Array to store data (possible swap route candidated, with prices for each step) needed for estimation
/// of the aggregated swap with liquidity routing, with the best total price
struct LrSwapCandidates {
    // The array of swaps with LR candidates is indexed by HashMaps with LR_0 and LR_1 base/rel pairs, for easy access and update
    // TODO: maybe this is overcomplicated and just a vector of candidates would be sufficicent
    inner0: LrSwapCandidateInfoMap,
    inner1: LrSwapCandidateInfoMap,
}

impl LrSwapCandidates {
    /// Init LR data map from the source token (mytoken) and tokens from orders
    async fn new_with_orders(
        ctx: &MmArc,
        user_base_ticker: Ticker,
        user_rel_ticker: Ticker,
        action: TakerAction,
        asks_coins: Vec<AsksForCoin>,
        bids_coins: Vec<BidsForCoin>,
    ) -> Self {
        async fn tokens_in_same_chain(ctx: &MmArc, coin: &EthCoin, other_ticker: &Ticker) -> bool {
            if let Some(chain_id) = coin.chain_id() {
                if let Some(other_coin) = get_coin_for_one_inch(ctx, other_ticker).await.ok() {
                    if let Some(other_chain_id) = other_coin.0.chain_id() {
                        if chain_id == other_chain_id {
                            return true;
                        }
                    }
                }
            }
            false
        }

        let user_base = get_coin_for_one_inch(ctx, &user_base_ticker).await.ok();
        let user_rel = get_coin_for_one_inch(ctx, &user_rel_ticker).await.ok();
        let src_coin = match action {
            TakerAction::Buy => &user_rel,
            TakerAction::Sell => &user_base,
        };
        let dst_coin = match action {
            TakerAction::Buy => &user_base,
            TakerAction::Sell => &user_rel,
        };
        let mut inner0 = HashMap::new();
        let mut inner1 = HashMap::new();

        for asks_for_coin in asks_coins {
            for order in asks_for_coin.orders {
                let order_coin = order.coin.clone();
                let maker_order = AskOrBidOrder::Ask {
                    base: asks_for_coin.base.clone(),
                    order,
                };
                let mut lr_data_0 = None;
                let mut lr_0_tokens = None;
                let mut lr_data_1 = None;
                let mut lr_1_tokens = None;
                // Add as a LR_0 candidate if the user source token and maker ask rel are in the same EVM chain, but different tokens
                if let Some(src_coin) = src_coin {
                    if tokens_in_same_chain(ctx, &src_coin.0, &order_coin).await
                        && src_coin.0.ticker() != order_coin.as_str()
                    {
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone())),
                            atomic_swap_order,
                            lr_data_1: None,
                        };*/
                        lr_data_0 = Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone()));
                        lr_0_tokens = Some((src_coin.0.ticker().to_owned(), order_coin));
                        // Route from source token to maker ask rel
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner0.insert((src_coin.0.ticker().to_owned(), order_coin), candidate); // Route from source token to maker ask rel
                        //continue; // Prevent possible adding same candidate into lr_data_1
                    }
                }

                // Add as a LR_1 candidate if the ask base and the user destination token are in the same EVM chain, but different tokens
                if let Some(dst_coin) = dst_coin {
                    if tokens_in_same_chain(ctx, &dst_coin.0, &asks_for_coin.base).await
                        && asks_for_coin.base.as_str() != dst_coin.0.ticker()
                    {
                        lr_data_1 = Some(LrStepData::new(
                            asks_for_coin.base.clone(),
                            dst_coin.0.ticker().to_owned(),
                        ));
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: None,
                            atomic_swap_order,
                            lr_data_1: Some(LrStepData::new(asks_for_coin.base.clone(), dst_coin.0.ticker().to_owned())),
                        };*/
                        lr_1_tokens = Some((asks_for_coin.base.clone(), dst_coin.0.ticker().to_owned()));
                        // Route from source token to maker ask rel
                        //lr_1_dst_token = dst_coin.0.ticker().to_owned();
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner1.insert((asks_for_coin.base.clone(), dst_coin.0.ticker().to_owned()), candidate); // Route from maker ask base into destination token
                        //continue;
                    }
                }
                let candidate = LrSwapCandidateInfo {
                    lr_data_0,
                    maker_order,
                    lr_data_1,
                };
                let candidate = Arc::new(RwLock::new(candidate));
                if let Some(lr_0_tokens) = lr_0_tokens {
                    inner0.insert((lr_0_tokens.0, lr_0_tokens.1), candidate.clone());
                }
                if let Some(lr_1_tokens) = lr_1_tokens {
                    inner1.insert((lr_1_tokens.0, lr_1_tokens.1), candidate);
                }
            }
        }

        for bids_for_coin in bids_coins {
            for order in bids_for_coin.orders {
                let order_coin = order.coin.clone();
                let atomic_swap_order = AskOrBidOrder::Bid {
                    rel: bids_for_coin.rel.clone(),
                    order,
                };
                let mut lr_data_0 = None;
                let mut lr_0_tokens = None;
                let mut lr_data_1 = None;
                let mut lr_1_tokens = None;
                // Add as a LR_0 candidate if the user source token and maker bid base are in the same EVM chain, but different tokens
                if let Some(src_coin) = src_coin {
                    if tokens_in_same_chain(ctx, &src_coin.0, &order_coin).await
                        && src_coin.0.ticker() != order_coin.as_str()
                    {
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone())),
                            atomic_swap_order,
                            lr_data_1: None,
                        };*/
                        lr_data_0 = Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone())); // Route from source token to maker bid base
                        lr_0_tokens = Some((src_coin.0.ticker().to_owned(), order_coin));
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner0.insert((src_coin.0.ticker().to_owned(), order_coin), candidate); // Route from source token to maker bid base
                        //continue; // Prevent possible adding same candidate into lr_data_1
                    }
                }

                // Add as a LR_1 candidate if the user destination token and maker bid base are in the same EVM chain, but different tokens
                if let Some(dst_coin) = dst_coin {
                    if tokens_in_same_chain(ctx, &dst_coin.0, &bids_for_coin.rel).await
                        && bids_for_coin.rel.as_str() != dst_coin.0.ticker()
                    {
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: None,
                            atomic_swap_order,
                            lr_data_1: Some(LrStepData::new(bids_for_coin.rel.clone(), dst_coin.0.ticker().to_owned())),
                        };*/
                        lr_data_1 = Some(LrStepData::new(
                            bids_for_coin.rel.clone(),
                            dst_coin.0.ticker().to_owned(),
                        )); // Route from maker bid rel into destination token
                        lr_1_tokens = Some((bids_for_coin.rel.clone(), dst_coin.0.ticker().to_owned()));
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner1.insert((bids_for_coin.rel.clone(), dst_coin.0.ticker().to_owned()), candidate); // Route from maker bid rel into destination token
                        //continue;
                    }
                }
                let candidate = LrSwapCandidateInfo {
                    lr_data_0,
                    maker_order: atomic_swap_order,
                    lr_data_1,
                };
                let candidate = Arc::new(RwLock::new(candidate));
                if let Some(lr_0_tokens) = lr_0_tokens {
                    inner0.insert((lr_0_tokens.0, lr_0_tokens.1), candidate.clone());
                }
                if let Some(lr_1_tokens) = lr_1_tokens {
                    inner1.insert((lr_1_tokens.0, lr_1_tokens.1), candidate);
                }
            }
        }

        Self { inner0, inner1 }
    }

    /// Calculate LR_0 destination tokens amounts required to fill ask orders for the atomic swap maker_amount.
    /// The maker_amount  is the source_amount of the LR_1 (if present) or provided in the params.
    /// The function multiplies the maker_amount by the order price. Base_amount must be in coin units (with decimals)
    async fn calc_lr_1_destination_amounts(
        &mut self,
        ctx: &MmArc,
        user_buy_amount: &MmNumber,
    ) -> MmResult<(), LrSwapError> {
        for candidate in self.inner0.values_mut() {
            let order_ticker = candidate.read().unwrap().maker_order.order().coin.clone();
            let lr_1_src_amount = candidate
                .read()
                .unwrap()
                .lr_data_1
                .as_ref()
                .and_then(|lr_data_1| lr_data_1.src_amount);
            let lr_1_src_decimals = candidate
                .read()
                .unwrap()
                .lr_data_1
                .as_ref()
                .and_then(|lr_data_1| lr_data_1.src_decimals);
            let coin = lp_coinfind_or_err(ctx, &order_ticker).await?;
            let mut candidate_write = candidate.write().unwrap();
            let price: MmNumber = candidate_write.maker_order.sell_price();
            let maker_amount = if let Some(lr_1_src_amount) = lr_1_src_amount {
                let lr_1_src_decimals =
                    lr_1_src_decimals.ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                u256_to_coins_mm_number(lr_1_src_amount, lr_1_src_decimals)?
            } else {
                user_buy_amount.clone()
            };
            let dst_amount = &maker_amount * &price;
            let Some(ref mut lr_data_0) = candidate_write.lr_data_0 else {
                continue;
            };
            let dst_amount = u256_from_coins_mm_number(&dst_amount, coin.decimals())?;
            lr_data_0.dst_amount = Some(dst_amount);
            log::debug!(
                "calc_lr_1_destination_amounts atomic_swap_order.order.coin={} coin.decimals()={} lr_data_0.dst_amount={:?}",
                order_ticker,
                coin.decimals(),
                dst_amount
            );
        }
        Ok(())
    }

    fn update_lr_prices(
        inner: &mut LrSwapCandidateInfoMap,
        mut lr_prices: HashMap<(Ticker, Ticker), Option<MmNumber>>,
    ) {
        for (key, val) in inner.iter_mut() {
            if let Some(ref mut lr_data_0) = val.write().unwrap().lr_data_0 {
                lr_data_0.lr_price = lr_prices.remove(key).flatten();
            }
        }
    }

    fn set_lr_0_src_amount(&mut self, mm_amount: &MmNumber) -> MmResult<(), LrSwapError> {
        for val in self.inner0.values_mut() {
            if let Some(ref mut lr_data_0) = val.write().unwrap().lr_data_0 {
                let src_decimals = lr_data_0
                    .src_decimals
                    .ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                let amount = u256_from_coins_mm_number(mm_amount, src_decimals)?;
                lr_data_0.src_amount = Some(amount);
            }
        }
        Ok(())
    }

    fn update_lr_0_swap_data(&mut self, mut lr_swap_data: HashMap<(Ticker, Ticker), Option<ClassicSwapData>>) {
        for (key, val) in self.inner0.iter_mut() {
            if let Some(ref mut lr_data_0) = val.write().unwrap().lr_data_0 {
                lr_data_0.lr_swap_data = lr_swap_data.remove(key).flatten();
            }
        }
    }

    fn update_lr_1_swap_data(&mut self, mut lr_swap_data: HashMap<(Ticker, Ticker), Option<ClassicSwapData>>) {
        for (key, val) in self.inner1.iter_mut() {
            if let Some(ref mut lr_data_1) = val.write().unwrap().lr_data_1 {
                lr_data_1.lr_swap_data = lr_swap_data.remove(key).flatten();
            }
        }
    }

    async fn set_contracts(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        for ((src_token, dst_token), candidate) in self.inner0.iter_mut() {
            let (src_coin, src_contract) = get_coin_for_one_inch(ctx, src_token).await?;
            let (dst_coin, dst_contract) = get_coin_for_one_inch(ctx, dst_token).await?;
            let mut candidate_write = candidate.write().unwrap();
            let Some(ref mut lr_data_0) = candidate_write.lr_data_0 else {
                continue;
            };
            let src_decimals = src_coin.decimals();
            let dst_decimals = dst_coin.decimals();

            #[cfg(feature = "for-tests")]
            {
                assert_ne!(src_decimals, 0);
                assert_ne!(dst_decimals, 0);
            }

            lr_data_0.src_contract = Some(src_contract);
            lr_data_0.dst_contract = Some(dst_contract);
            lr_data_0.src_decimals = Some(src_decimals);
            lr_data_0.dst_decimals = Some(dst_decimals);
            lr_data_0.chain_id = dst_coin.chain_id();
        }
        Ok(())
    }

    /// Query 1inch token_0/token_1 prices in series and estimate token_0/token_1 average price
    /// Assuming the outer RPC-level code ensures that relation src_tokens : dst_tokens will never be M:N (but only 1:M or M:1)
    async fn query_lr_prices(ctx: &MmArc, inner: &mut LrSwapCandidateInfoMap) -> MmResult<(), LrSwapError> {
        let mut prices_futs = vec![];
        let mut src_dst = vec![];
        for ((src_token, dst_token), candidate) in inner.iter() {
            let candidate_read = candidate.read().unwrap();
            let Some(ref lr_data_0) = candidate_read.lr_data_0 else {
                continue;
            };
            let (src_contract, dst_contract, chain_id) = lr_data_0.get_chain_contract_info()?;
            // Run src / dst token price query:
            let query_params = CrossPriceParams::new(chain_id, src_contract, dst_contract)
                .with_granularity(Some(CROSS_PRICES_GRANULARITY))
                .with_limit(Some(CROSS_PRICES_LIMIT))
                .build_query_params()?;
            let url = PortfolioUrlBuilder::create_api_url_builder(ctx, PortfolioApiMethods::CrossPrices)?
                .with_query_params(query_params)
                .build()?;
            let fut = ApiClient::call_api::<CrossPricesSeries>(url);
            prices_futs.push(fut);
            src_dst.push((src_token.clone(), dst_token.clone()));
        }
        let prices_in_series = join_all(prices_futs).await.into_iter().map(|res| res.ok()); // set bad results to None to preserve prices_in_series length

        let quotes = src_dst
            .into_iter()
            .zip(prices_in_series)
            .map(|((src, dst), series)| {
                let dst_price = cross_prices_average(series); // estimate SRC/DST price as average from series
                ((src, dst), dst_price)
            })
            .collect::<HashMap<_, _>>();

        log_cross_prices(&quotes);
        LrSwapCandidates::update_lr_prices(inner, quotes);
        Ok(())
    }

    /// Estimate the needed source amount for LR_0 step, by dividing the dst amount by the src/dst price.
    /// The dst_amount shold be set
    #[allow(clippy::result_large_err)]
    fn estimate_lr_0_source_amounts(&mut self) -> MmResult<(), LrSwapError> {
        for candidate in self.inner0.values_mut() {
            let order_ticker = candidate.read().unwrap().maker_order.order().coin.clone();
            let mut candidate_write = candidate.write().unwrap();
            let Some(ref mut lr_data_0) = candidate_write.lr_data_0 else {
                continue;
            };
            let Some(ref lr_price) = lr_data_0.lr_price else {
                continue;
            };
            let dst_amount = lr_data_0
                .dst_amount
                .ok_or(LrSwapError::InternalError("no dst_amount".to_owned()))?;
            let dst_amount = mm_number_from_u256(dst_amount);
            if let Some(src_amount) = &dst_amount.checked_div(lr_price) {
                lr_data_0.src_amount = Some(mm_number_to_u256(src_amount)?);
                log::debug!(
                    "estimate_lr_0_source_amounts lr_data.order.coin={} lr_price={} lr_data.src_amount={:?}",
                    order_ticker,
                    lr_price.to_decimal(),
                    src_amount
                );
            }
        }
        Ok(())
    }

    /// Estimate the needed source amount for LR_1 step, by dividing the known dst amount by the src/dst price
    /// The dst amount is set by the User in destination token units
    #[allow(clippy::result_large_err)]
    fn estimate_lr_1_source_amounts_from_dest(&mut self, dst_amount: &MmNumber) -> MmResult<(), LrSwapError> {
        for candidate in self.inner0.values_mut() {
            let order_ticker = candidate.read().unwrap().maker_order.order().coin.clone();
            let mut candidate_write = candidate.write().unwrap();
            let Some(ref mut lr_data_0) = candidate_write.lr_data_0 else {
                continue;
            };
            let Some(ref lr_price) = lr_data_0.lr_price else {
                continue;
            };
            if let Some(src_amount) = &dst_amount.checked_div(lr_price) {
                lr_data_0.src_amount = Some(mm_number_to_u256(src_amount)?);
                log::debug!(
                    "estimate_lr_1_source_amounts_from_dest lr_data.order.coin={} lr_price={} lr_data.src_amount={:?}",
                    order_ticker,
                    lr_price.to_decimal(),
                    src_amount
                );
            }
        }
        Ok(())
    }

    /// Estimate the LR_1 source amount either from the LR_0 destination amount (if the LR_0 data present) or from the User's provided sell amount (in coins)
    /// For LR_0 we need to deduct dex fee
    async fn estimate_lr_1_source_amounts_from_lr_0(
        &mut self,
        ctx: &MmArc,
        user_sell_amount: &MmNumber,
    ) -> MmResult<(), LrSwapError> {
        for candidate in self.inner0.values_mut() {
            let (taker_amount, order_ticker) = {
                // Need a block to ensure the read lock is released
                let candidate_read = candidate.read().unwrap();
                let lr_0_dst_amount = candidate_read
                    .lr_data_0
                    .as_ref()
                    .and_then(|lr_data_0| lr_data_0.src_amount);
                let lr_0_dst_decimals = candidate_read
                    .lr_data_0
                    .as_ref()
                    .and_then(|lr_data_0| lr_data_0.src_decimals);

                let taker_amount = if let Some(lr_0_dst_amount) = lr_0_dst_amount {
                    let lr_0_dst_decimals =
                        lr_0_dst_decimals.ok_or(LrSwapError::InternalError("no dst_decimals".to_owned()))?;
                    let volume_with_fees = u256_to_coins_mm_number(lr_0_dst_amount, lr_0_dst_decimals)?;
                    let maker_ticker = candidate_read.maker_order.maker_ticker();
                    let taker_ticker = candidate_read.maker_order.taker_ticker();
                    let dex_fee_rate = DexFee::dex_fee_rate(&taker_ticker, &maker_ticker);
                    volume_with_fees / (MmNumber::from("1") + dex_fee_rate)
                } else {
                    user_sell_amount.clone()
                };
                let order_ticker = candidate_read.maker_order.order().coin.clone();
                (taker_amount, order_ticker)
            };
            let coin = lp_coinfind_or_err(ctx, &order_ticker).await?;
            let mut candidate_write = candidate.write().unwrap();
            let price: MmNumber = candidate_write.maker_order.sell_price();
            /*let maker_amount = if let Some(lr_1_src_amount) = lr_1_src_amount {
                let lr_1_src_decimals = lr_1_src_decimals.ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                u256_to_coins_mm_number(lr_1_src_amount, lr_1_src_decimals)?
            } else {
                user_buy_amount.clone()
            };*/
            let lr_1_src_amount = &taker_amount * &price;
            let Some(ref mut lr_data_1) = candidate_write.lr_data_1 else {
                continue;
            };
            let lr_1_src_amount = u256_from_coins_mm_number(&lr_1_src_amount, coin.decimals())?;
            lr_data_1.src_amount = Some(lr_1_src_amount);
            log::debug!(
                "estimate_lr_1_source_amounts_from_lr_0 atomic_swap_order.order.coin={} coin.decimals()={} lr_data_1.src_amount={:?}",
                order_ticker,
                coin.decimals(),
                lr_1_src_amount
            );
        }
        Ok(())
    }

    /// Estimate dex and trade fees to do the atomic swap and add them to the source amount.
    /// The dex fee will be deducted from the destination amount (in proportion) when the atomic swap is running.
    #[allow(clippy::result_large_err)]
    async fn estimate_lr_0_fee_amounts(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        for ((src_token, _), candidate) in self.inner0.iter_mut() {
            let (src_amount, src_decimals, atomic_swap_maker_token) = {
                // use a block to ensure read lock released
                let candidate_read = candidate.read().unwrap();
                let Some(ref lr_data_0) = candidate_read.lr_data_0 else {
                    continue;
                };
                let Some(src_amount) = lr_data_0.src_amount else {
                    continue;
                };
                let Some(src_decimals) = lr_data_0.src_decimals else {
                    continue;
                };
                (src_amount, src_decimals, candidate_read.maker_order.maker_ticker())
            };
            let src_amount_mm_num = u256_to_coins_mm_number(src_amount, src_decimals)?;
            let src_coin = lp_coinfind_or_err(ctx, src_token).await?; // TODO: when I used get_coin_for_one_inch(), throwing a error if the order coin not EVM, 'lr_quote.rs' is lost in the error path. Why? dedup()?
                                                                      //let taker_swap_params = create_taker_swap_default_params(src_coin.deref(), taker_coin.deref(), src_amount_mm_num.clone(), FeeApproxStage::TradePreimage).await?;
            let dex_fee = DexFee::new_from_taker_coin(src_coin.deref(), &atomic_swap_maker_token, &src_amount_mm_num)
                .fee_amount();
            let mut candidate_write = candidate.write().unwrap();
            let Some(ref mut lr_data_0) = candidate_write.lr_data_0 else {
                continue;
            };
            // Add dex fee to the amount
            //let src_amount_with_fees = LrStepData::add_fees_to_amount(src_amount_mm_num, src_token, &taker_swap_params)?;
            let src_amount_with_fees = &src_amount_mm_num + &dex_fee;
            println!("estimate_lr_0_fee_amounts src_amount_mm_num={src_amount_mm_num} dex_fee={dex_fee} src_amount_with_fees={src_amount_with_fees}");
            lr_data_0.src_amount = Some(u256_from_coins_mm_number(&src_amount_with_fees, src_decimals)?);
            //lr_data_0.taker_swap_params = Some(taker_swap_params);
            lr_data_0.dex_fee = Some(dex_fee);
        }
        Ok(())
    }

    /// Run 1inch requests to get LR quotes to convert source tokens to tokens in orders
    async fn run_lr_0_quotes(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        let mut src_dst = vec![];
        let mut quote_futs = vec![];
        for ((src_token, dst_token), candidate) in self.inner0.iter() {
            let candidate_read = candidate.read().unwrap();
            let Some(ref lr_data_0) = candidate_read.lr_data_0 else {
                continue;
            };
            /*let Some(src_amount) = lr_data_0.src_amount else {
                continue;
            };*/
            let Some(fut) = create_quote_call(ctx, lr_data_0)? else {
                continue;
            };
            /*let (src_contract, dst_contract, chain_id) = lr_data_0.get_chain_contract_info()?;
            let query_params = ClassicSwapQuoteCallBuilder::new(src_contract, dst_contract, src_amount.to_string())
                .with_include_tokens_info(Some(true))
                .with_include_gas(Some(true))
                .build_query_params()?;
            let url = SwapUrlBuilder::create_api_url_builder(ctx, chain_id, SwapApiMethods::ClassicSwapQuote)?
                .with_query_params(query_params)
                .build()?;
            let fut = ApiClient::call_api::<ClassicSwapData>(url);*/
            quote_futs.push(fut);
            src_dst.push((src_token.clone(), dst_token.clone()));
        }
        let lr_data = join_all(quote_futs).await.into_iter().map(|res| res.ok()); // if a bad result received (for e.g. low liguidity) set to None to preserve swap_data length
        let lr_data_map = src_dst.into_iter().zip(lr_data).collect();
        self.update_lr_0_swap_data(lr_data_map);
        Ok(())
    }

    /// Run 1inch requests to get LR quotes to convert source tokens to tokens in orders
    async fn run_lr_1_quotes(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        let mut src_dst = vec![];
        let mut quote_futs = vec![];
        for ((src_token, dst_token), candidate) in self.inner1.iter() {
            let candidate_read = candidate.read().unwrap();
            let Some(ref lr_data_1) = candidate_read.lr_data_1 else {
                continue;
            };
            let Some(fut) = create_quote_call(ctx, lr_data_1)? else {
                continue;
            };
            quote_futs.push(fut);
            src_dst.push((src_token.clone(), dst_token.clone()));
        }
        let lr_data = join_all(quote_futs).await.into_iter().map(|res| res.ok()); // if a bad result received (for e.g. low liguidity) set to None to preserve swap_data length
        let lr_data_map = src_dst.into_iter().zip(lr_data).collect();
        self.update_lr_1_swap_data(lr_data_map);
        Ok(())
    }

    /// Select the best swap path, by minimum of total swap price, including LR steps and atomic swap)
    #[allow(clippy::result_large_err)]
    fn select_best_swap(&self) -> MmResult<(ClassicSwapDataExt, AskOrBidOrder, MmNumber), LrSwapError> {
        // Calculate swap's total_price (filling the order plus LR swap) as src_amount / order_amount
        // where src_amount is user tokens to pay for the swap with LR, 'order_amount' is amount which will fill the order
        // Tx fee is not accounted here because it is in the platform coin, not token, so we can't compare LR swap tx fee directly here.
        // Instead, GUI may calculate and show to the user the total spendings for LR swap, including fees, in USD or other fiat currency
        let calc_total_price = |src_amount: U256, lr_swap: &ClassicSwapData, order: &AskOrBidOrder| {
            let src_amount = mm_number_from_u256(src_amount);
            let order_price = order.sell_price();
            let dst_amount = MmNumber::from(lr_swap.dst_amount.as_str());
            let amount_to_fill_order = dst_amount.checked_div(&order_price)?;
            let total_price = src_amount.checked_div(&amount_to_fill_order);
            log::debug!(
                "select_best_swap order.coin={} lr_swap.dst_amount={} amount_to_fill_order={} total_price={}",
                order.order().coin,
                lr_swap.dst_amount,
                amount_to_fill_order.to_decimal(),
                total_price.as_ref().unwrap_or(&MmNumber::from(0)).to_decimal()
            );
            total_price
        };

        self.inner0
            .values()
            .filter_map(|candidate| {
                let candidate_read = candidate.read().unwrap();
                let atomic_swap_order = candidate_read.maker_order.clone();
                candidate_read
                    .lr_data_0
                    .as_ref()
                    .map(|lr_data_0| (atomic_swap_order, lr_data_0.clone()))
            })
            // filter out orders for which we did not get LR swap quotes and were not able to estimate needed source amount
            .filter_map(
                |(atomic_swap_order, lr_data_0)| match (lr_data_0.src_amount, lr_data_0.lr_swap_data) {
                    (Some(src_amount), Some(lr_swap_data)) => Some((src_amount, lr_swap_data, atomic_swap_order)),
                    (_, _) => None,
                },
            )
            // calculate total price and filter out orders for which we could not calculate the total price
            .filter_map(|(src_amount, lr_swap_data, order)| {
                calc_total_price(src_amount, &lr_swap_data, &order).map(|total_price| {
                    (
                        ClassicSwapDataExt {
                            api_details: lr_swap_data,
                            src_amount,
                        },
                        order,
                        total_price,
                    )
                })
            })
            .min_by(|(_, _, price_0), (_, _, price_1)| price_0.cmp(price_1))
            .ok_or(MmError::new(LrSwapError::BestLrSwapNotFound))
    }
}

/// Implementation code to find the optimal swap path (with the lowest total price) from the `user_base` coin to the `user_rel` coin
/// (`Aggregated taker swap` path).
/// This path includes:
/// - An atomic swap step: used to fill a specific ask (or, in future, bid) order provided in the parameters.
/// - A liquidity routing (LR) step before and/or after (todo) the atomic swap: converts `user_base` or `user_sell` into the coin in the order.
///
/// This function currently supports only:
/// - Ask orders and User 'sell' requests.
/// - Liquidity routing before the atomic swap.
///
/// TODO: Support bid orders and User 'buy' requests.
/// TODO: Support liquidity routing after the atomic swap (e.g., to convert the output coin into `user_rel`).
/// TODO: Note that in this function we request 1inch quotas (not swap details) so no slippage is applied for this.
/// When the actual aggregated swap is running we would create a new 1inch request for swap detail for these tokens
/// but the new price for them may be different and the estimated amount after the liquidity routing may deviate much
/// from the value needed to fill the atomic maker order (like User wanted this).
/// Maybe we should request for swap details here and this will allow to ensure slippage for the LR amount which we return here.
/// TODO: it's not only the slippage problem though. We try to estimate the needed source amount by querying the OHLC price and
/// this may also add error to the error from the slippage. We should take this error into account too.
pub async fn find_best_swap_path_with_lr(
    ctx: &MmArc,
    user_base: Ticker,
    user_rel: Ticker,
    action: TakerAction,
    asks: Vec<AsksForCoin>,
    bids: Vec<BidsForCoin>,
    amount: &MmNumber,
) -> MmResult<(ClassicSwapDataExt, AskOrBidOrder, MmNumber), LrSwapError> {
    let mut candidates = LrSwapCandidates::new_with_orders(ctx, user_base, user_rel, action.clone(), asks, bids).await;
    candidates.set_contracts(ctx).await?;
    match action {
        TakerAction::Buy => {
            // Calculate amounts from the destination coin 'buy' amount (backwards)

            //candidates.calc_lr_1_destination_amounts(ctx, amount).await?;
            // Query src/dst price for LR_1 step (to estimate the source amount).
            LrSwapCandidates::query_lr_prices(ctx, &mut candidates.inner1).await?;
            candidates.estimate_lr_1_source_amounts_from_dest(amount)?;
            // Query src/dst price for LR_0 step (to estimate the source amount).
            // (TODO: good to query prices for LR_0 and LR_1 in one join)
            LrSwapCandidates::query_lr_prices(ctx, &mut candidates.inner0).await?;
            candidates.calc_lr_1_destination_amounts(ctx, amount).await?;
            candidates.estimate_lr_0_source_amounts()?;
            candidates.estimate_lr_0_fee_amounts(ctx).await?;
            candidates.run_lr_0_quotes(ctx).await?;
            candidates.run_lr_1_quotes(ctx).await?;
        },
        TakerAction::Sell => {
            // Calculate amounts starting from the source coin 'sell' amount (forwards)
            candidates.set_lr_0_src_amount(amount)?;
            candidates.estimate_lr_0_fee_amounts(ctx).await?;
            candidates.run_lr_0_quotes(ctx).await?;
            candidates.estimate_lr_1_source_amounts_from_lr_0(ctx, amount).await?;
            candidates.run_lr_1_quotes(ctx).await?;
        },
    }
    //candidates.calc_destination_token_amounts(ctx, base_amount).await?;
    //candidates.query_destination_token_prices(ctx).await?;
    //candidates.estimate_source_token_amounts()?;
    //candidates.estimate_fee_amounts(ctx).await?;
    //candidates.estimate_lr_0_fee_amounts(ctx).await?;
    //candidates.run_lr_0_quotes(ctx).await?;
    //candidates.run_lr_1_quotes(ctx).await?;

    candidates.select_best_swap()
}

/// Helper to process 1inch token cross prices data and return average price
fn cross_prices_average(series: Option<CrossPricesSeries>) -> Option<MmNumber> {
    let Some(series) = series else {
        return None;
    };
    if series.is_empty() {
        return None;
    }
    let total: MmNumber = series.iter().fold(MmNumber::from(0), |acc, price_data| {
        acc + MmNumber::from(price_data.avg.clone())
    });
    Some(total / MmNumber::from(series.len() as u64))
}

fn log_cross_prices(prices: &HashMap<(Ticker, Ticker), Option<MmNumber>>) {
    for p in prices {
        log::debug!(
            "1inch cross_prices result(averaged)={:?} {:?}",
            p,
            p.1.clone().map(|v| v.to_decimal())
        );
    }
}

fn create_quote_call(
    ctx: &MmArc,
    lr_data: &LrStepData,
) -> MmResult<Option<Pin<Box<dyn Future03<Output = MmResult<ClassicSwapData, OneInchError>> + Send>>>, LrSwapError> {
    let Some(src_amount) = lr_data.src_amount else {
        return Ok(None);
    };
    let (src_contract, dst_contract, chain_id) = lr_data.get_chain_contract_info()?;
    let query_params = ClassicSwapQuoteCallBuilder::new(src_contract, dst_contract, src_amount.to_string())
        .with_include_tokens_info(Some(true))
        .with_include_gas(Some(true))
        .build_query_params()?;
    let url = SwapUrlBuilder::create_api_url_builder(ctx, chain_id, SwapApiMethods::ClassicSwapQuote)?
        .with_query_params(query_params)
        .build()?;
    let fut = ApiClient::call_api::<ClassicSwapData>(url);
    Ok(Some(fut))
}
