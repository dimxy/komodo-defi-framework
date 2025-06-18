//! Finding best quote to do swaps with liquidity routing (LR) support
//! Swaps with LR run additional interim swaps in EVM chains to convert one token into another token suitable to do a normal atomic swap.

use super::lr_errors::LrSwapError;
use super::lr_helpers::{get_coin_for_one_inch, u256_from_coins_mm_number};
use crate::lp_ordermatch::RpcOrderbookEntryV2;
use crate::lr_swap::lr_helpers::{mm_number_from_u256, mm_number_to_u256};
use crate::lr_swap::ClassicSwapDataExt;
use coins::hd_wallet::AddrToString;
use coins::lp_coinfind_or_err;
use coins::MmCoin;
use coins::Ticker;
use common::log;
use ethereum_types::{Address as EthAddress, U256};
use futures::future::join_all;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_number::MmNumber;
use num_traits::CheckedDiv;
use std::collections::HashMap;
use trading_api::one_inch_api::classic_swap_types::{ClassicSwapData, ClassicSwapQuoteCallBuilder};
use trading_api::one_inch_api::client::{ApiClient, PortfolioApiMethods, PortfolioUrlBuilder, SwapApiMethods,
                                        SwapUrlBuilder};
use trading_api::one_inch_api::portfolio_types::{CrossPriceParams, CrossPricesSeries, DataGranularity};

/// To estimate src/dst price query price history for last 5 min
const CROSS_PRICES_GRANULARITY: DataGranularity = DataGranularity::FiveMin;
/// Use no more than 1 price history samples to estimate src/dst price
const CROSS_PRICES_LIMIT: u32 = 1;

/// Internal struct to collect data for selecting the best swap with LR
struct LrData {
    /// Order to fill
    order: RpcOrderbookEntryV2,
    /// Source token contract address (to do interim LR swap from)
    src_contract: Option<EthAddress>,
    /// Source token amount (estimated) in smallest units
    src_amount: Option<U256>,
    /// Source token decimals
    src_decimals: Option<u8>,
    /// Destination token contract address (to do interim LR swap into)
    dst_contract: Option<EthAddress>,
    /// Destination token amount in smallest units
    dst_amount: Option<U256>,
    /// Destination token decimals
    dst_decimals: Option<u8>,
    /// Chain id where interim LR swap occurs (obtained from the destination token)
    chain_id: Option<u64>,
    /// Estimated src token / dst token price
    lr_price: Option<MmNumber>,
    /// A quote from LR provider with tx data to do interim LR swap of src token to dst token
    lr_swap_data: Option<ClassicSwapData>,
}

impl LrData {
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

struct LrDataMap {
    /// Map to store data needed for best price estimations for swaps with LR,
    /// the key is the source and destination token pair from the LR swap part
    inner: HashMap<(Ticker, Ticker), LrData>,
}

impl LrDataMap {
    /// Init LR data map from the source token (mytoken) and tokens from orders
    fn new_with_src_token(src_token: Ticker, orders: Vec<RpcOrderbookEntryV2>) -> Self {
        Self {
            inner: orders
                .into_iter()
                .map(|order| {
                    ((src_token.clone(), order.coin.clone()), LrData {
                        order,
                        src_contract: None,
                        src_decimals: None,
                        src_amount: None,
                        dst_contract: None,
                        dst_amount: None,
                        dst_decimals: None,
                        chain_id: None,
                        lr_price: None,
                        lr_swap_data: None,
                    })
                })
                .collect::<HashMap<(_, _), _>>(),
        }
    }

    /// Calculate amounts of LR-swap-0 destination tokens required to fill ask orders for the requested base_amount:
    /// multiplies base_amount by the order price. Base_amount must be in coin units (with decimals)
    async fn calc_destination_token_amounts(
        &mut self,
        ctx: &MmArc,
        base_amount: &MmNumber,
    ) -> MmResult<(), LrSwapError> {
        for lr_data in self.inner.values_mut() {
            let price: MmNumber = lr_data.order.price.rational.clone().into();
            let dst_amount = base_amount * &price;
            let dst_coin = lp_coinfind_or_err(ctx, &lr_data.order.coin).await?;
            lr_data.dst_amount = Some(u256_from_coins_mm_number(&dst_amount, dst_coin.decimals())?);
            log::debug!(
                "calc_destination_token_amounts base_amount[coins]={} lr_data.order.coin={} price={} dst_coin.decimals()={} lr_data.dst_amount[smallest]={:?}",
                base_amount.to_decimal(),
                lr_data.order.coin,
                price.to_decimal(),
                dst_coin.decimals(),
                lr_data.dst_amount
            );
        }
        Ok(())
    }

    fn update_with_lr_prices(&mut self, mut lr_prices: HashMap<(Ticker, Ticker), Option<MmNumber>>) {
        for (key, val) in self.inner.iter_mut() {
            val.lr_price = lr_prices.remove(key).flatten();
        }
    }

    fn update_with_lr_swap_data(&mut self, mut lr_swap_data: HashMap<(Ticker, Ticker), Option<ClassicSwapData>>) {
        for (key, val) in self.inner.iter_mut() {
            val.lr_swap_data = lr_swap_data.remove(key).flatten();
        }
    }

    async fn update_with_contracts(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        for ((src_token, dst_token), lr_data) in self.inner.iter_mut() {
            let (src_coin, src_contract) = get_coin_for_one_inch(ctx, src_token).await?;
            let (dst_coin, dst_contract) = get_coin_for_one_inch(ctx, dst_token).await?;
            let src_decimals = src_coin.decimals();
            let dst_decimals = dst_coin.decimals();

            #[cfg(feature = "for-tests")]
            {
                assert_ne!(src_decimals, 0);
                assert_ne!(dst_decimals, 0);
            }

            lr_data.src_contract = Some(src_contract);
            lr_data.dst_contract = Some(dst_contract);
            lr_data.src_decimals = Some(src_decimals);
            lr_data.dst_decimals = Some(dst_decimals);
            lr_data.chain_id = dst_coin.chain_id();
        }
        Ok(())
    }

    /// Query 1inch token_0/token_1 prices in series and estimate token_0/token_1 average price
    /// Assuming the outer RPC-level code ensures that relation src_tokens : dst_tokens will never be M:N (but only 1:M or M:1)
    async fn query_destination_token_prices(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        let mut prices_futs = vec![];
        let mut src_dst = vec![];
        for ((src_token, dst_token), lr_data) in self.inner.iter() {
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
        self.update_with_lr_prices(quotes);
        Ok(())
    }

    /// Estimate the needed source amount for LR swap, by dividing the known dst amount by the src/dst price
    #[allow(clippy::result_large_err)]
    fn estimate_source_token_amounts(&mut self) -> MmResult<(), LrSwapError> {
        for lr_data in self.inner.values_mut() {
            let Some(ref dst_price) = lr_data.lr_price else {
                continue;
            };
            let dst_amount = lr_data
                .dst_amount
                .ok_or(LrSwapError::InternalError("no dst_amount".to_owned()))?;
            let dst_amount = mm_number_from_u256(dst_amount);
            if let Some(src_amount) = &dst_amount.checked_div(dst_price) {
                lr_data.src_amount = Some(mm_number_to_u256(src_amount)?);
                log::debug!(
                    "estimate_source_token_amounts lr_data.order.coin={} dst_price={} lr_data.src_amount[smallest]={:?}",
                    lr_data.order.coin,
                    dst_price.to_decimal(),
                    lr_data.src_amount
                );
            }
        }
        Ok(())
    }

    /// Run 1inch requests to get LR quotes to convert source tokens to tokens in orders
    async fn run_lr_quotes(&mut self, ctx: &MmArc) -> MmResult<(), LrSwapError> {
        let mut src_dst = vec![];
        let mut quote_futs = vec![];
        for ((src_token, dst_token), lr_data) in self.inner.iter() {
            let Some(src_amount) = lr_data.src_amount else {
                continue;
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
            quote_futs.push(fut);
            src_dst.push((src_token.clone(), dst_token.clone()));
        }
        let swap_data = join_all(quote_futs).await.into_iter().map(|res| res.ok()); // if a bad result received (for e.g. low liguidity) set to None to preserve swap_data length
        let swap_data_map = src_dst.into_iter().zip(swap_data.into_iter()).collect();
        self.update_with_lr_swap_data(swap_data_map);
        Ok(())
    }

    /// Select the best swap path, by minimum of total swap price (including order and LR swap)
    #[allow(clippy::result_large_err)]
    fn select_best_swap(&self) -> MmResult<(ClassicSwapDataExt, RpcOrderbookEntryV2, MmNumber), LrSwapError> {
        // Calculate swap's total_price (filling the order plus LR swap) as src_amount / order_amount
        // where src_amount is user tokens to pay for the swap with LR, 'order_amount' is amount which will fill the order
        // Tx fee is not accounted here because it is in the platform coin, not token, so we can't compare LR swap tx fee directly here.
        // Instead, GUI may calculate and show to the user the total spendings for LR swap, including fees, in USD or other fiat currency
        let calc_total_price = |src_amount: U256, lr_swap: &ClassicSwapData, order: &RpcOrderbookEntryV2| {
            let src_amount = mm_number_from_u256(src_amount);
            let order_price = MmNumber::from(order.price.rational.clone());
            let dst_amount = MmNumber::from(lr_swap.dst_amount.as_str());
            let amount_to_fill_order = dst_amount.checked_div(&order_price)?;
            let total_price = src_amount.checked_div(&amount_to_fill_order);
            log::debug!("select_best_swap order.coin={} lr_swap.dst_amount=[smallest]{} amount_to_fill_order[smallest]={} total_price={}", 
                order.coin, lr_swap.dst_amount, amount_to_fill_order.to_decimal(), total_price.as_ref().unwrap_or(&MmNumber::from(0)).to_decimal());
            total_price
        };

        self.inner
            .values()
            // filter out orders for which we did not get LR swap quotes and were not able to estimate needed source amount
            .filter_map(|lr_data| match (lr_data.src_amount, lr_data.lr_swap_data.as_ref()) {
                (Some(src_amount), Some(lr_swap_data)) => {
                    Some((src_amount, lr_swap_data.clone(), lr_data.order.clone()))
                },
                (_, _) => None,
            })
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

/// Finds the best swap path to fill ask order. The path includes an LR-swap quote to sell my_token for the rel tokens from the orders
/// base_amount is amount of UTXO coins user would like to buy
pub async fn find_best_fill_ask_with_lr(
    ctx: &MmArc,
    user_token: Ticker,
    orders: Vec<RpcOrderbookEntryV2>,
    base_amount: &MmNumber,
) -> MmResult<(ClassicSwapDataExt, RpcOrderbookEntryV2, MmNumber), LrSwapError> {
    let mut lr_data_map = LrDataMap::new_with_src_token(user_token, orders);
    lr_data_map.update_with_contracts(ctx).await?;
    lr_data_map.calc_destination_token_amounts(ctx, base_amount).await?;
    lr_data_map.query_destination_token_prices(ctx).await?;
    lr_data_map.estimate_source_token_amounts()?;
    lr_data_map.run_lr_quotes(ctx).await?;

    lr_data_map.select_best_swap()
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
