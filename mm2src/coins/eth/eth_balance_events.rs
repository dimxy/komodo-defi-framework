use async_trait::async_trait;
use common::{executor::{AbortSettings, SpawnAbortable, Timer},
             log, Future01CompatExt};
use ethereum_types::Address;
use futures::{channel::oneshot, stream::FuturesUnordered, StreamExt};
use instant::Instant;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::MmError;
use mm2_event_stream::{behaviour::EventBehaviour, ErrorEventName, Event, EventName};
use mm2_number::BigDecimal;
use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

use super::EthCoin;
use crate::streaming_events_config::BalanceEventConfig;
use crate::{eth::{u256_to_big_decimal, Erc20TokenInfo},
            BalanceError, CoinWithDerivationMethod, MmCoin};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SingleEthCoinConfig {
    /// The time in seconds to wait before re-polling the balance and streaming.
    #[serde(default = "default_stream_interval")]
    pub stream_interval_seconds: f64,
}

const fn default_stream_interval() -> f64 { 10. }

pub struct EthBalanceEventStreamer {
    /// Whether the event is enabled for this coin.
    enabled: bool,
    /// The period in seconds between each balance check.
    interval: f64,
    coin: EthCoin,
}

impl EthBalanceEventStreamer {
    pub fn try_new(config: Json, coin: EthCoin) -> serde_json::Result<Self> {
        let config: BalanceEventConfig = serde_json::from_value(config)?;
        let coin_config: Option<SingleEthCoinConfig> = match config.find_coin(&coin.ticker) {
            // Try to parse the coin config.
            Some(c) => Some(serde_json::from_value(c)?),
            None => None,
        };
        Ok(Self {
            enabled: coin_config.is_some(),
            interval: coin_config.map(|c| c.stream_interval_seconds).unwrap_or(0.0),
            coin,
        })
    }
}

struct BalanceData {
    ticker: String,
    address: String,
    balance: BigDecimal,
}

struct BalanceFetchError {
    ticker: String,
    address: String,
    error: MmError<BalanceError>,
}

type BalanceResult = Result<BalanceData, BalanceFetchError>;

/// This implementation differs from others, as they immediately return
/// an error if any of the requests fails. This one completes all futures
/// and returns their results individually.
async fn get_all_balance_results_concurrently(coin: &EthCoin, addresses: HashSet<Address>) -> Vec<BalanceResult> {
    let mut tokens = coin.get_erc_tokens_infos();
    // Workaround for performance purposes.
    //
    // Unlike tokens, the platform coin length is constant (=1). Instead of creating a generic
    // type and mapping the platform coin and the entire token list (which can grow at any time), we map
    // the platform coin to Erc20TokenInfo so that we can use the token list right away without
    // additional mapping.
    tokens.insert(coin.ticker.clone(), Erc20TokenInfo {
        // This is a dummy value, since there is no token address for the platform coin.
        // In the fetch_balance function, we check if the token_ticker is equal to this
        // coin's ticker to avoid using token_address to fetch the balance
        // and to use address_balance instead.
        token_address: Address::default(),
        decimals: coin.decimals,
    });
    drop_mutability!(tokens);

    let mut all_jobs = FuturesUnordered::new();

    for address in addresses {
        let jobs = tokens.iter().map(|(token_ticker, info)| {
            let coin = coin.clone();
            let token_ticker = token_ticker.clone();
            let info = info.clone();
            async move { fetch_balance(&coin, address, token_ticker, &info).await }
        });

        all_jobs.extend(jobs);
    }

    all_jobs.collect().await
}

async fn fetch_balance(
    coin: &EthCoin,
    address: Address,
    token_ticker: String,
    info: &Erc20TokenInfo,
) -> Result<BalanceData, BalanceFetchError> {
    let (balance_as_u256, decimals) = if token_ticker == coin.ticker {
        (
            coin.address_balance(address)
                .compat()
                .await
                .map_err(|error| BalanceFetchError {
                    ticker: token_ticker.clone(),
                    address: address.to_string(),
                    error,
                })?,
            coin.decimals,
        )
    } else {
        (
            coin.get_token_balance(info.token_address)
                .await
                .map_err(|error| BalanceFetchError {
                    ticker: token_ticker.clone(),
                    address: address.to_string(),
                    error,
                })?,
            info.decimals,
        )
    };

    let balance_as_big_decimal = u256_to_big_decimal(balance_as_u256, decimals).map_err(|e| BalanceFetchError {
        ticker: token_ticker.clone(),
        address: address.to_string(),
        error: e.into(),
    })?;

    Ok(BalanceData {
        ticker: token_ticker,
        address: address.to_string(),
        balance: balance_as_big_decimal,
    })
}

#[async_trait]
impl EventBehaviour for EthBalanceEventStreamer {
    fn event_name() -> EventName { EventName::BALANCE }

    fn error_event_name() -> ErrorEventName { ErrorEventName::CoinBalanceError }

    async fn handle(self, tx: oneshot::Sender<Result<(), String>>) {
        const RECEIVER_DROPPED_MSG: &str = "Receiver is dropped, which should never happen.";

        async fn start_polling(coin: EthCoin, ctx: MmArc, interval: f64) {
            async fn sleep_remaining_time(interval: f64, now: Instant) {
                // If the interval is x seconds,
                // our goal is to broadcast changed balances every x seconds.
                // To achieve this, we need to subtract the time complexity of each iteration.
                // Given that an iteration already takes 80% of the interval,
                // this will lead to inconsistency in the events.
                let remaining_time = interval - now.elapsed().as_secs_f64();
                // Not worth to make a call for less than `0.1` durations
                if remaining_time >= 0.1 {
                    Timer::sleep(remaining_time).await;
                }
            }

            let mut cache: HashMap<String, HashMap<String, BigDecimal>> = HashMap::new();

            loop {
                let now = Instant::now();

                let addresses = match coin.all_addresses().await {
                    Ok(addresses) => addresses,
                    Err(e) => {
                        log::error!("Failed getting addresses for {}. Error: {}", coin.ticker, e);
                        let e = serde_json::to_value(e).expect("Serialization shouldn't fail.");
                        ctx.stream_channel_controller
                            .broadcast(Event::new(
                                format!("{}:{}", EthBalanceEventStreamer::error_event_name(), coin.ticker),
                                e.to_string(),
                            ))
                            .await;
                        sleep_remaining_time(interval, now).await;
                        continue;
                    },
                };

                let mut balance_updates = vec![];
                for result in get_all_balance_results_concurrently(&coin, addresses).await {
                    match result {
                        Ok(res) => {
                            if Some(&res.balance) == cache.get(&res.ticker).and_then(|map| map.get(&res.address)) {
                                continue;
                            }

                            balance_updates.push(json!({
                                "ticker": res.ticker,
                                "address": res.address,
                                "balance": { "spendable": res.balance, "unspendable": BigDecimal::default() }
                            }));
                            cache
                                .entry(res.ticker.clone())
                                .or_insert_with(HashMap::new)
                                .insert(res.address, res.balance);
                        },
                        Err(err) => {
                            log::error!(
                                "Failed getting balance for '{}:{}' with {interval} interval. Error: {}",
                                err.ticker,
                                err.address,
                                err.error
                            );
                            let e = serde_json::to_value(err.error).expect("Serialization shouldn't fail.");
                            ctx.stream_channel_controller
                                .broadcast(Event::new(
                                    format!(
                                        "{}:{}:{}",
                                        EthBalanceEventStreamer::error_event_name(),
                                        err.ticker,
                                        err.address
                                    ),
                                    e.to_string(),
                                ))
                                .await;
                        },
                    };
                }

                if !balance_updates.is_empty() {
                    ctx.stream_channel_controller
                        .broadcast(Event::new(
                            EthBalanceEventStreamer::event_name().to_string(),
                            json!(balance_updates).to_string(),
                        ))
                        .await;
                }

                sleep_remaining_time(interval, now).await;
            }
        }

        let ctx = match MmArc::from_weak(&self.coin.ctx) {
            Some(ctx) => ctx,
            None => {
                let msg = "MM context must have been initialized already.";
                tx.send(Err(msg.to_owned())).expect(RECEIVER_DROPPED_MSG);
                panic!("{}", msg);
            },
        };

        tx.send(Ok(())).expect(RECEIVER_DROPPED_MSG);

        start_polling(self.coin, ctx, self.interval).await
    }

    async fn spawn(self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        log::info!(
            "{} event is activated for {}, and polling every {}s",
            Self::event_name(),
            self.coin.ticker,
            self.interval
        );

        let (tx, rx) = oneshot::channel();
        let settings = AbortSettings::info_on_abort(format!(
            "{} event is stopped for {}.",
            Self::event_name(),
            self.coin.ticker
        ));
        self.coin.spawner().spawn_with_settings(self.handle(tx), settings);

        rx.await.unwrap_or_else(|e| {
            Err(format!(
                "The handler was aborted before sending event initialization status: {e}"
            ))
        })
    }
}
