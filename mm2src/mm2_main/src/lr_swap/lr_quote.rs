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
use futures::future::{join_all, BoxFuture};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_number::MmNumber;
use mm2_rpc::data::legacy::TakerAction;
use num_traits::CheckedDiv;
use std::collections::HashMap;
use std::ops::Deref;
use trading_api::one_inch_api::classic_swap_types::{ClassicSwapData, ClassicSwapQuoteCallBuilder};
use trading_api::one_inch_api::client::{ApiClient, PortfolioApiMethods, PortfolioUrlBuilder, SwapApiMethods,
                                        SwapUrlBuilder};
use trading_api::one_inch_api::errors::OneInchError;
use trading_api::one_inch_api::portfolio_types::{CrossPriceParams, CrossPricesSeries, DataGranularity};

/// To estimate src/dst price query price history for last 5 min
const CROSS_PRICES_GRANULARITY: DataGranularity = DataGranularity::FiveMin;
/// Use no more than 1 price history samples to estimate src/dst price
const CROSS_PRICES_LIMIT: u32 = 1;

type ClassicSwapDataResult = MmResult<ClassicSwapData, OneInchError>;

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
    /// Estimated src token / dst token price. NOTE: the price is calculated in smallest units
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
}

struct LrSwapCandidateInfo {
    /// Data for liquidity routing before atomic swap
    lr_data_0: Option<LrStepData>,
    /// Atomic swap order to fill
    maker_order: AskOrBidOrder,
    /// Estimated sell amount to start atomic swap, if LR_0 not present
    atomic_swap_taker_amount: Option<U256>,
    /// Estimated buy amount received from the atomic swap, if LR_0 not present
    atomic_swap_maker_amount: Option<U256>,
    /// Data for liquidity routing after atomic swap
    lr_data_1: Option<LrStepData>,
}

/// Array to store data (possible swap route candidated, with prices for each step) needed for estimation
/// of the aggregated swap with liquidity routing, with the best total price
struct LrSwapCandidates {
    // The array of swaps with LR candidates
    inner: Vec<LrSwapCandidateInfo>,
}

/// Mutable iterator over lr_data_0 field
struct LrStepDataMut0<'a> {
    inner: std::slice::IterMut<'a, LrSwapCandidateInfo>,
}

impl<'a> Iterator for LrStepDataMut0<'a> {
    type Item = &'a mut LrStepData;

    fn next(&mut self) -> Option<Self::Item> { self.inner.next().and_then(|item| item.lr_data_0.as_mut()) }
}

/// Mutable iterator over lr_data_1 field
struct LrStepDataMut1<'a> {
    inner: std::slice::IterMut<'a, LrSwapCandidateInfo>,
}

impl<'a> Iterator for LrStepDataMut1<'a> {
    type Item = &'a mut LrStepData;

    fn next(&mut self) -> Option<Self::Item> { self.inner.next().and_then(|item| item.lr_data_1.as_mut()) }
}

impl LrSwapCandidates {
    fn iter_mut_lr_data_0(&mut self) -> LrStepDataMut0 {
        LrStepDataMut0 {
            inner: self.inner.iter_mut(),
        }
    }

    fn iter_mut_lr_data_1(&mut self) -> LrStepDataMut1 {
        LrStepDataMut1 {
            inner: self.inner.iter_mut(),
        }
    }

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
                if let Ok(other_coin) = get_coin_for_one_inch(ctx, other_ticker).await {
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
        let (user_src_ticker, user_src_coin, user_dst_ticker, user_dst_coin) = match action {
            TakerAction::Buy => (user_rel_ticker, user_rel, user_base_ticker, user_base),
            TakerAction::Sell => (user_base_ticker, user_base, user_rel_ticker, user_rel),
        };

        let mut inner = vec![];
        for asks_for_coin in asks_coins {
            for order in asks_for_coin.orders {
                let order_coin = order.coin.clone();
                let maker_order = AskOrBidOrder::Ask {
                    base: asks_for_coin.base.clone(),
                    order,
                };
                let mut lr_data_0 = None;
                //let mut lr_0_tokens = None;
                let mut lr_data_1 = None;
                //let mut lr_1_tokens = None;
                // Add as a LR_0 candidate if the user source token and maker ask rel are in the same EVM chain, but different tokens
                if let Some(src_coin) = user_src_coin.as_ref() {
                    if tokens_in_same_chain(ctx, &src_coin.0, &order_coin).await
                        && src_coin.0.ticker() != order_coin.as_str()
                    {
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone())),
                            atomic_swap_order,
                            lr_data_1: None,
                        };*/
                        // Route from source token to maker ask rel
                        lr_data_0 = Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone()));
                        //lr_0_tokens = Some((src_coin.0.ticker().to_owned(), order_coin));
                        // Route from source token to maker ask rel
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner0.insert((src_coin.0.ticker().to_owned(), order_coin), candidate); // Route from source token to maker ask rel
                        //continue; // Prevent possible adding same candidate into lr_data_1
                    }
                }

                // Add as a LR_1 candidate if the ask base and the user destination token are in the same EVM chain, but different tokens
                if let Some(dst_coin) = user_dst_coin.as_ref() {
                    if tokens_in_same_chain(ctx, &dst_coin.0, &asks_for_coin.base).await
                        && asks_for_coin.base.as_str() != dst_coin.0.ticker()
                    {
                        // Route from source token to maker ask rel
                        lr_data_1 = Some(LrStepData::new(
                            asks_for_coin.base.clone(),
                            dst_coin.0.ticker().to_owned(),
                        ));
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: None,
                            atomic_swap_order,
                            lr_data_1: Some(LrStepData::new(asks_for_coin.base.clone(), dst_coin.0.ticker().to_owned())),
                        };*/
                        //lr_1_tokens = Some((asks_for_coin.base.clone(), dst_coin.0.ticker().to_owned()));
                        // Route from source token to maker ask rel
                        //lr_1_dst_token = dst_coin.0.ticker().to_owned();
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner1.insert((asks_for_coin.base.clone(), dst_coin.0.ticker().to_owned()), candidate); // Route from maker ask base into destination token
                        //continue;
                    }
                }
                // do not add orders w/o any LR or w/o our coin
                if (lr_data_0.is_some() || maker_order.taker_ticker() == user_src_ticker)
                    && (lr_data_1.is_some() || maker_order.maker_ticker() == user_dst_ticker)
                {
                    let candidate = LrSwapCandidateInfo {
                        lr_data_0,
                        maker_order,
                        atomic_swap_taker_amount: None,
                        atomic_swap_maker_amount: None,
                        lr_data_1,
                    };
                    inner.push(candidate);
                }
            }
        }

        for bids_for_coin in bids_coins {
            for order in bids_for_coin.orders {
                let order_coin = order.coin.clone();
                let maker_order = AskOrBidOrder::Bid {
                    rel: bids_for_coin.rel.clone(),
                    order,
                };
                let mut lr_data_0 = None;
                //let mut lr_0_tokens = None;
                let mut lr_data_1 = None;
                //let mut lr_1_tokens = None;
                // Add as a LR_0 candidate if the user source token and maker bid base are in the same EVM chain, but different tokens
                if let Some(src_coin) = user_src_coin.as_ref() {
                    if tokens_in_same_chain(ctx, &src_coin.0, &order_coin).await
                        && src_coin.0.ticker() != order_coin.as_str()
                    {
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone())),
                            atomic_swap_order,
                            lr_data_1: None,
                        };*/
                        // Route from source token to maker bid base
                        lr_data_0 = Some(LrStepData::new(src_coin.0.ticker().to_owned(), order_coin.clone()));
                        //lr_0_tokens = Some((src_coin.0.ticker().to_owned(), order_coin));
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner0.insert((src_coin.0.ticker().to_owned(), order_coin), candidate); // Route from source token to maker bid base
                        //continue; // Prevent possible adding same candidate into lr_data_1
                    }
                }

                // Add as a LR_1 candidate if the user destination token and maker bid base are in the same EVM chain, but different tokens
                if let Some(dst_coin) = user_dst_coin.as_ref() {
                    if tokens_in_same_chain(ctx, &dst_coin.0, &bids_for_coin.rel).await
                        && bids_for_coin.rel.as_str() != dst_coin.0.ticker()
                    {
                        /*let candidate = LrSwapCandidateInfo {
                            lr_data_0: None,
                            atomic_swap_order,
                            lr_data_1: Some(LrStepData::new(bids_for_coin.rel.clone(), dst_coin.0.ticker().to_owned())),
                        };*/
                        // Route from maker bid rel into destination token
                        lr_data_1 = Some(LrStepData::new(
                            bids_for_coin.rel.clone(),
                            dst_coin.0.ticker().to_owned(),
                        ));
                        //lr_1_tokens = Some((bids_for_coin.rel.clone(), dst_coin.0.ticker().to_owned()));
                        //let candidate = Arc::new(RwLock::new(candidate));
                        //inner1.insert((bids_for_coin.rel.clone(), dst_coin.0.ticker().to_owned()), candidate); // Route from maker bid rel into destination token
                        //continue;
                    }
                }

                // do not add orders w/o any LR or w/o our coin
                if (lr_data_0.is_some() || maker_order.taker_ticker() == user_src_ticker)
                    && (lr_data_1.is_some() || maker_order.maker_ticker() == user_dst_ticker)
                {
                    let candidate = LrSwapCandidateInfo {
                        lr_data_0,
                        maker_order,
                        atomic_swap_taker_amount: None,
                        atomic_swap_maker_amount: None,
                        lr_data_1,
                    };
                    inner.push(candidate);
                }
            }
        }

        Self { inner }
    }

    /// Calculate LR_0 destination tokens amounts required to fill maker orders for the required maker_amount.
    /// The maker_amount is the source_amount of the LR_1 (if present) or the one provided in the params.
    /// The function multiplies the maker_amount by the order price.
    /// The maker_amount must be in coin units (with decimals)
    async fn calc_lr_0_destination_amounts(
        &mut self,
        ctx: &MmArc,
        user_buy_amount: &MmNumber,
    ) -> MmResult<(), LrSwapError> {
        for candidate in self.inner.iter_mut() {
            let order_ticker = &candidate.maker_order.order().coin;
            let lr_1_src_amount = candidate.lr_data_1.as_ref().and_then(|lr_data_1| lr_data_1.src_amount);
            let lr_1_src_decimals = candidate
                .lr_data_1
                .as_ref()
                .and_then(|lr_data_1| lr_data_1.src_decimals);
            let maker_coin = lp_coinfind_or_err(ctx, order_ticker).await?;
            let maker_sell_price: MmNumber = candidate.maker_order.sell_price();
            let maker_amount = if let Some(lr_1_src_amount) = lr_1_src_amount {
                let lr_1_src_decimals =
                    lr_1_src_decimals.ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                u256_to_coins_mm_number(lr_1_src_amount, lr_1_src_decimals)?
            } else {
                user_buy_amount.clone()
            };
            let dst_amount = &maker_amount * &maker_sell_price;
            let dst_amount = u256_from_coins_mm_number(&dst_amount, maker_coin.decimals())?;
            if let Some(ref mut lr_data_0) = candidate.lr_data_0 {
                lr_data_0.dst_amount = Some(dst_amount);
            } else {
                candidate.atomic_swap_taker_amount = Some(dst_amount);
            }

            log::debug!(
                "calc_lr_0_destination_amounts order_ticker={} maker_coin.decimals()={} lr_data_0.dst_amount={:?}",
                order_ticker,
                maker_coin.decimals(),
                dst_amount
            );
        }
        Ok(())
    }

    fn update_lr_prices(
        lr_data_refs: Vec<&mut LrStepData>,
        mut lr_prices: HashMap<(Ticker, Ticker), Option<MmNumber>>,
    ) {
        for item in lr_data_refs {
            if let Some(prices) = lr_prices.remove(&(item._src_token.clone(), item._dst_token.clone())) {
                item.lr_price = prices;
            }
        }
    }

    /// Set LR_0 src_amount to user sell amount, or if LR_0 not present, set atomic_swap_src_amount
    #[allow(clippy::result_large_err)]
    async fn set_lr_0_src_amount(&mut self, ctx: &MmArc, mm_amount: &MmNumber) -> MmResult<(), LrSwapError> {
        for candidate in self.inner.iter_mut() {
            if let Some(ref mut lr_data_0) = candidate.lr_data_0 {
                let src_decimals = lr_data_0
                    .src_decimals
                    .ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                let amount = u256_from_coins_mm_number(mm_amount, src_decimals)?;
                lr_data_0.src_amount = Some(amount);
            } else {
                let taker_coin = lp_coinfind_or_err(ctx, &candidate.maker_order.taker_ticker()).await?;
                let taker_amount = u256_from_coins_mm_number(mm_amount, taker_coin.decimals())?;
                candidate.atomic_swap_taker_amount = Some(taker_amount);
            }
        }
        Ok(())
    }

    fn update_lr_0_swap_data(&mut self, lr_swap_data: Vec<(usize, Option<ClassicSwapData>)>) {
        for item in lr_swap_data {
            if let Some(candidate) = self.inner.get_mut(item.0) {
                if let Some(lr_data_0) = candidate.lr_data_0.as_mut() {
                    lr_data_0.lr_swap_data = item.1;
                }
            }
        }
    }

    fn update_lr_1_swap_data(&mut self, lr_swap_data: Vec<(usize, Option<ClassicSwapData>)>) {
        for item in lr_swap_data {
            if let Some(candidate) = self.inner.get_mut(item.0) {
                if let Some(lr_data_1) = candidate.lr_data_1.as_mut() {
                    lr_data_1.lr_swap_data = item.1;
                }
            }
        }
    }

    async fn set_contracts(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        for candidate in self.inner.iter_mut() {
            if let Some(ref mut lr_data_0) = candidate.lr_data_0 {
                let (src_coin, src_contract) = get_coin_for_one_inch(ctx, &lr_data_0._src_token).await?;
                let (dst_coin, dst_contract) = get_coin_for_one_inch(ctx, &lr_data_0._dst_token).await?;
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
            if let Some(ref mut lr_data_1) = candidate.lr_data_1 {
                let (src_coin, src_contract) = get_coin_for_one_inch(ctx, &lr_data_1._src_token).await?;
                let (dst_coin, dst_contract) = get_coin_for_one_inch(ctx, &lr_data_1._dst_token).await?;
                let src_decimals = src_coin.decimals();
                let dst_decimals = dst_coin.decimals();

                #[cfg(feature = "for-tests")]
                {
                    assert_ne!(src_decimals, 0);
                    assert_ne!(dst_decimals, 0);
                }

                lr_data_1.src_contract = Some(src_contract);
                lr_data_1.dst_contract = Some(dst_contract);
                lr_data_1.src_decimals = Some(src_decimals);
                lr_data_1.dst_decimals = Some(dst_decimals);
                lr_data_1.chain_id = dst_coin.chain_id();
            }
        }
        Ok(())
    }

    /// Query 1inch token_0/token_1 prices in series and estimate token_0/token_1 average price
    /// Assuming the outer RPC-level code ensures that relation src_tokens : dst_tokens will never be M:N (but only 1:M or M:1)
    async fn query_lr_prices(ctx: &MmArc, lr_data_refs: Vec<&mut LrStepData>) -> MmResult<(), LrSwapError> {
        let mut prices_futs = vec![];
        let mut src_dst = vec![];
        for lr_data in lr_data_refs.iter() {
            let (src_contract, dst_contract, chain_id) = lr_data.get_chain_contract_info()?;
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
            src_dst.push((lr_data._src_token.clone(), lr_data._dst_token.clone()));
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
        LrSwapCandidates::update_lr_prices(lr_data_refs, quotes);
        Ok(())
    }

    /// Estimate the needed source amount for LR_0 step, by dividing the dst amount by the src/dst price.
    /// The dst_amount shold be set
    #[allow(clippy::result_large_err)]
    fn estimate_lr_0_source_amounts(&mut self) -> MmResult<(), LrSwapError> {
        for candidate in self.inner.iter_mut() {
            let Some(ref mut lr_data_0) = candidate.lr_data_0 else {
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
                // Note: lr_price is calculated in smallest units
                lr_data_0.src_amount = Some(mm_number_to_u256(src_amount)?);
                log::debug!(
                    "estimate_lr_0_source_amounts lr_data.order.coin={} lr_price={} lr_data.src_amount={:?}",
                    candidate.maker_order.order().coin,
                    lr_price.to_decimal(),
                    lr_data_0.src_amount.unwrap_or_default()
                );
            }
        }
        Ok(())
    }

    /// Estimate the needed source amount for LR_1 step, by dividing the known dst amount by the src/dst price
    /// The dst amount is set by the User in the 'buy' coin units
    #[allow(clippy::result_large_err)]
    async fn estimate_lr_1_source_amounts_from_dest(
        &mut self,
        ctx: &MmArc,
        dst_amount: &MmNumber,
    ) -> MmResult<(), LrSwapError> {
        for candidate in self.inner.iter_mut() {
            if let Some(ref mut lr_data_1) = candidate.lr_data_1 {
                let Some(ref lr_price) = lr_data_1.lr_price else {
                    continue;
                };
                let dst_decimals = lr_data_1
                    .dst_decimals
                    .ok_or(LrSwapError::InternalError("no dst_decimals".to_owned()))?;
                let dst_amount = u256_from_coins_mm_number(dst_amount, dst_decimals)?;
                let dst_amount = mm_number_from_u256(dst_amount);
                if let Some(src_amount) = &dst_amount.checked_div(lr_price) {
                    // Note: lr_price in calculated in smallest units
                    // let src_decimals = lr_data_1.src_decimals.ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                    lr_data_1.src_amount = Some(mm_number_to_u256(src_amount)?);
                    log::debug!(
                        "estimate_lr_1_source_amounts_from_dest lr_data_1._src_token={} lr_price={} lr_data.src_amount={:?}",
                        lr_data_1._src_token,
                        lr_price.to_decimal(),
                        src_amount
                    );
                }
            } else {
                let maker_coin = lp_coinfind_or_err(ctx, &candidate.maker_order.maker_ticker()).await?;
                let dst_amount = u256_from_coins_mm_number(dst_amount, maker_coin.decimals())?;
                candidate.atomic_swap_maker_amount = Some(dst_amount);
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
        for candidate in self.inner.iter_mut() {
            let dst_amount = candidate.lr_data_0.as_ref().and_then(|lr_data_0| lr_data_0.dst_amount);
            let dst_decimals = candidate
                .lr_data_0
                .as_ref()
                .and_then(|lr_data_0| lr_data_0.dst_decimals);
            let taker_amount = if let Some(dst_amount) = dst_amount {
                let dst_decimals = dst_decimals.ok_or(LrSwapError::InternalError("no dst_decimals".to_owned()))?;
                let volume_with_fees = u256_to_coins_mm_number(dst_amount, dst_decimals)?;
                let maker_ticker = candidate.maker_order.maker_ticker();
                let taker_ticker = candidate.maker_order.taker_ticker();
                let dex_fee_rate = DexFee::dex_fee_rate(&taker_ticker, &maker_ticker);
                volume_with_fees / (MmNumber::from("1") + dex_fee_rate)
            } else {
                user_sell_amount.clone() // TODO: use atomic_swap_taker_amount
            };
            let order_ticker = &candidate.maker_order.order().coin;
            let maker_coin = lp_coinfind_or_err(ctx, order_ticker).await?;
            let maker_sell_price: MmNumber = candidate.maker_order.sell_price();
            let maker_amount = &taker_amount * &maker_sell_price;
            let maker_amount = u256_from_coins_mm_number(&maker_amount, maker_coin.decimals())?;
            if let Some(ref mut lr_data_1) = candidate.lr_data_1 {
                lr_data_1.src_amount = Some(maker_amount);
            } else {
                candidate.atomic_swap_maker_amount = Some(maker_amount); // if no LR_1, store as atomic_swap dest amount for future use
            }
            log::debug!(
                "estimate_lr_1_source_amounts_from_lr_0 order_ticker={} maker_coin.decimals()={} maker_amount={:?}",
                order_ticker,
                maker_coin.decimals(),
                maker_amount
            );
        }
        Ok(())
    }

    /// Estimate dex and trade fees to do the atomic swap and add them to the source amount.
    /// The dex fee will be deducted from the destination amount (in proportion) when the atomic swap is running.
    #[allow(clippy::result_large_err)]
    async fn estimate_lr_0_fee_amounts(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        for candidate in self.inner.iter_mut() {
            let Some(ref lr_data_0) = candidate.lr_data_0 else {
                continue;
            };
            let Some(src_amount) = lr_data_0.src_amount else {
                continue;
            };
            let Some(src_decimals) = lr_data_0.src_decimals else {
                continue;
            };
            let atomic_swap_maker_token = candidate.maker_order.maker_ticker();
            let src_amount_mm_num = u256_to_coins_mm_number(src_amount, src_decimals)?;
            let src_coin = lp_coinfind_or_err(ctx, &lr_data_0._src_token).await?; // TODO: when I used get_coin_for_one_inch(), throwing a error if the order coin not EVM, 'lr_quote.rs' is lost in the error path. Why? dedup()?
                                                                                  //let taker_swap_params = create_taker_swap_default_params(src_coin.deref(), taker_coin.deref(), src_amount_mm_num.clone(), FeeApproxStage::TradePreimage).await?;
                                                                                  // TODO: use rate
            let dex_fee = DexFee::new_from_taker_coin(src_coin.deref(), &atomic_swap_maker_token, &src_amount_mm_num)
                .fee_amount();
            let Some(ref mut lr_data_0) = candidate.lr_data_0 else {
                continue;
            };
            // Add dex fee to the amount
            //let src_amount_with_fees = LrStepData::add_fees_to_amount(src_amount_mm_num, src_token, &taker_swap_params)?;
            let src_amount_with_fees = &src_amount_mm_num + &dex_fee;
            log::debug!("estimate_lr_0_fee_amounts src_amount_mm_num={src_amount_mm_num} dex_fee={dex_fee} src_amount_with_fees={src_amount_with_fees}");
            lr_data_0.src_amount = Some(u256_from_coins_mm_number(&src_amount_with_fees, src_decimals)?);
            //lr_data_0.taker_swap_params = Some(taker_swap_params);
            lr_data_0.dex_fee = Some(dex_fee);
        }
        Ok(())
    }

    /// Run 1inch requests to get LR quotes to convert source tokens to tokens in orders
    async fn run_lr_0_quotes(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        let mut idx = vec![];
        let mut quote_futs = vec![];
        for candidate in self.inner.iter().enumerate() {
            let Some(ref lr_data_0) = candidate.1.lr_data_0 else {
                continue;
            };
            let Some(fut) = create_quote_call(ctx, lr_data_0)? else {
                continue;
            };
            quote_futs.push(fut);
            idx.push(candidate.0);
        }
        let lr_quotes = join_all(quote_futs).await.into_iter().map(|res| res.ok()); // if a bad result received (for e.g. low liguidity) set to None to preserve swap_data length
        let lr_quotes_indexed = idx.into_iter().zip(lr_quotes).collect();
        self.update_lr_0_swap_data(lr_quotes_indexed);
        Ok(())
    }

    /// Run 1inch requests to get LR quotes to convert source tokens to tokens in orders
    async fn run_lr_1_quotes(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        let mut idx = vec![];
        let mut quote_futs = vec![];
        for candidate in self.inner.iter().enumerate() {
            let Some(ref lr_data_1) = candidate.1.lr_data_1 else {
                continue;
            };
            let Some(fut) = create_quote_call(ctx, lr_data_1)? else {
                continue;
            };
            // TODO: combine index in the future
            quote_futs.push(fut);
            idx.push(candidate.0);
        }
        let lr_quotes = join_all(quote_futs).await.into_iter().map(|res| res.ok()); // if a bad result received (for e.g. low liguidity) set to None to preserve swap_data length
        let lr_quotes_indexed = idx.into_iter().zip(lr_quotes).collect();
        self.update_lr_1_swap_data(lr_quotes_indexed);
        Ok(())
    }

    /// Select the best swap path, by minimum of total swap price, including LR steps and atomic swap)
    #[allow(clippy::result_large_err)]
    async fn select_best_swap(
        self,
        ctx: &MmArc,
    ) -> MmResult<
        (
            Option<ClassicSwapDataExt>,
            AskOrBidOrder,
            Option<ClassicSwapDataExt>,
            MmNumber,
        ),
        LrSwapError,
    > {
        let mut best_price = None;
        let mut best_candidate = None;
        let candidate_len = self.inner.len();
        for candidate in self.inner {
            let mut lr_0_src_token_print = None;
            let mut lr_0_dst_token_print = None;
            let src_amount = if let Some(ref lr_data_0) = candidate.lr_data_0 {
                lr_0_src_token_print = Some(lr_data_0._src_token.clone());
                lr_0_dst_token_print = Some(lr_data_0._dst_token.clone());
                if lr_data_0.lr_swap_data.is_none() {
                    continue; // No provider quote - skip this candidate
                }
                let src_amount = lr_data_0
                    .src_amount
                    .ok_or(LrSwapError::InternalError("no src_amount".to_owned()))?;
                let src_decimals = lr_data_0
                    .src_decimals
                    .ok_or(LrSwapError::InternalError("no src_decimals".to_owned()))?;
                // Deduct dex fee
                let volume_with_fees = u256_to_coins_mm_number(src_amount, src_decimals)?;
                let maker_ticker = candidate.maker_order.maker_ticker();
                let taker_ticker = candidate.maker_order.taker_ticker();
                let dex_fee_rate = DexFee::dex_fee_rate(&taker_ticker, &maker_ticker);
                volume_with_fees / (MmNumber::from("1") + dex_fee_rate)
            } else {
                let atomic_swap_taker_amount = candidate
                    .atomic_swap_taker_amount
                    .ok_or(LrSwapError::InternalError("no atomic swap taker amount".to_owned()))?;
                let taker_coin = lp_coinfind_or_err(ctx, &candidate.maker_order.taker_ticker()).await?;
                u256_to_coins_mm_number(atomic_swap_taker_amount, taker_coin.decimals())?
            };

            let dst_amount = if let Some(ref lr_data_1) = candidate.lr_data_1 {
                if lr_data_1.lr_swap_data.is_none() {
                    continue; // No provider quote - skip this candidate
                }
                let dst_amount = lr_data_1
                    .dst_amount
                    .ok_or(LrSwapError::InternalError("no dst_amount".to_owned()))?;
                let dst_decimals = lr_data_1
                    .dst_decimals
                    .ok_or(LrSwapError::InternalError("no dst_decimals".to_owned()))?;
                // Deduct dex fee
                u256_to_coins_mm_number(dst_amount, dst_decimals)?
            } else {
                let atomic_swap_maker_amount = candidate
                    .atomic_swap_maker_amount
                    .ok_or(LrSwapError::InternalError("no atomic swap maker amount".to_owned()))?;
                let maker_coin = lp_coinfind_or_err(ctx, &candidate.maker_order.maker_ticker()).await?;
                u256_to_coins_mm_number(atomic_swap_maker_amount, maker_coin.decimals())?
            };

            let Some(total_price) = src_amount.checked_div(&dst_amount) else {
                continue;
            };
            log::debug!("select_best_swap: LR_0: {lr_0_src_token_print:?}/{lr_0_dst_token_print:?}, src_amount={} dst_amount={} total_price={}", src_amount.to_decimal(), dst_amount.to_decimal(), total_price.to_decimal());
            if let Some(best_price_unwrapped) = best_price.as_ref() {
                if &total_price < best_price_unwrapped {
                    best_price = Some(total_price);
                    best_candidate = Some(candidate);
                }
            } else {
                best_price = Some(total_price);
                best_candidate = Some(candidate);
            }
        }

        /*self.inner
        .iter()
        .filter_map(|candidate| {
            let atomic_swap_order = candidate.maker_order.clone();
            candidate
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
        .min_by(|(_, _, price_0), (_, _, price_1)| price_0.cmp(price_1))*/
        if let Some(best_price) = best_price {
            let best_candidate = best_candidate.ok_or(LrSwapError::InternalError("no best_candidate".to_owned()))?;
            let lr_data_ext_0 = best_candidate
                .lr_data_0
                .map(|lr_data| -> Result<_, LrSwapError> {
                    Ok(ClassicSwapDataExt {
                        src_amount: lr_data
                            .src_amount
                            .ok_or(LrSwapError::InternalError("no src_amount".to_owned()))?,
                        api_details: lr_data
                            .lr_swap_data
                            .ok_or(LrSwapError::InternalError("no lr_swap_data".to_owned()))?,
                    })
                })
                .transpose()?;
            let lr_data_ext_1 = best_candidate
                .lr_data_1
                .map(|lr_data| -> Result<_, LrSwapError> {
                    Ok(ClassicSwapDataExt {
                        src_amount: lr_data
                            .src_amount
                            .ok_or(LrSwapError::InternalError("no src_amount".to_owned()))?,
                        api_details: lr_data
                            .lr_swap_data
                            .ok_or(LrSwapError::InternalError("no lr_swap_data".to_owned()))?,
                    })
                })
                .transpose()?;
            Ok((lr_data_ext_0, best_candidate.maker_order, lr_data_ext_1, best_price))
        } else {
            MmError::err(LrSwapError::BestLrSwapNotFound {
                candidates: candidate_len as u32,
            })
        }
    }
}

/// Implementation code to find the optimal swap path (with the lowest total price) from the `user_base` coin to the `user_rel` coin
/// (`Aggregated taker swap` path).
/// This path includes:
/// - An atomic swap step: used to fill a specific ask (or, in future, bid) order provided in the parameters.
/// - A liquidity routing (LR) step before and/or after (todo) the atomic swap: converts `user_base` or `user_sell` into the coin in the order.
///
/// TODO: Note that in this function we request 1inch quotas (not swap details) so no slippage is applied for this.
/// When the actual aggregated swap is running we would create a new 1inch request for swap detail for these tokens
/// but the new price for them may be different and the estimated amount after the liquidity routing may deviate much
/// from the value needed to fill the atomic maker order (like User wanted this).
/// Maybe we should request for swap details here and this will allow to ensure slippage for the LR amount which we return here.
///
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
) -> MmResult<
    (
        Option<ClassicSwapDataExt>,
        AskOrBidOrder,
        Option<ClassicSwapDataExt>,
        MmNumber,
    ),
    LrSwapError,
> {
    let mut candidates = LrSwapCandidates::new_with_orders(ctx, user_base, user_rel, action.clone(), asks, bids).await;
    candidates.set_contracts(ctx).await?;
    match action {
        TakerAction::Buy => {
            // Calculate amounts from the destination coin 'buy' amount (backwards)
            // Query src/dst price for LR_1 step (to estimate the source amount).
            LrSwapCandidates::query_lr_prices(ctx, candidates.iter_mut_lr_data_1().collect::<Vec<_>>()).await?;
            candidates.estimate_lr_1_source_amounts_from_dest(ctx, amount).await?;
            // Query src/dst price for LR_0 step (to estimate the source amount).
            // TODO: good to query prices for LR_0 and LR_1 in one join
            LrSwapCandidates::query_lr_prices(ctx, candidates.iter_mut_lr_data_0().collect::<Vec<_>>()).await?;
            candidates.calc_lr_0_destination_amounts(ctx, amount).await?;
            candidates.estimate_lr_0_source_amounts()?;
            candidates.estimate_lr_0_fee_amounts(ctx).await?;
            candidates.run_lr_0_quotes(ctx).await?;
            candidates.run_lr_1_quotes(ctx).await?;
        },
        TakerAction::Sell => {
            // Calculate amounts starting from the source coin 'sell' amount (forwards)
            candidates.set_lr_0_src_amount(ctx, amount).await?;
            candidates.estimate_lr_0_fee_amounts(ctx).await?;
            candidates.run_lr_0_quotes(ctx).await?;
            candidates.estimate_lr_1_source_amounts_from_lr_0(ctx, amount).await?;
            candidates.run_lr_1_quotes(ctx).await?;
        },
    }
    candidates.select_best_swap(ctx).await
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

#[allow(clippy::result_large_err)]
fn create_quote_call<'a>(
    ctx: &MmArc,
    lr_data: &'a LrStepData,
) -> MmResult<Option<BoxFuture<'a, ClassicSwapDataResult>>, LrSwapError> {
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
    Ok(Some(Box::pin(fut)))
}
