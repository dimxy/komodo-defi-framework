use async_trait::async_trait;
use common::{http_uri_to_ws_address, log};
use futures::channel::oneshot;
use futures_util::{SinkExt, StreamExt};
use jsonrpc_core::MethodCall;
use jsonrpc_core::{Id as RpcId, Params as RpcParams, Value as RpcValue, Version as RpcVersion};
use mm2_core::mm_ctx::MmArc;
use mm2_event_stream::{Controller, Event, EventStreamer, NoDataIn, StreamHandlerInput};
use mm2_number::BigDecimal;
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

use super::TendermintCoin;
use crate::streaming_events_config::{BalanceEventConfig, EmptySubConfig};
use crate::{tendermint::TendermintCommons, utxo::utxo_common::big_decimal_from_sat_unsigned, MarketCoinOps};

pub struct TendermintBalanceEventStreamer {
    /// Whether the event is enabled for this coin.
    enabled: bool,
    coin: TendermintCoin,
}

impl TendermintBalanceEventStreamer {
    pub fn try_new(config: Json, coin: TendermintCoin) -> serde_json::Result<Self> {
        let config: BalanceEventConfig = serde_json::from_value(config)?;
        let enabled = match config.find_coin(coin.ticker()) {
            // This is just an extra check to make sure the config is correct (no config)
            Some(c) => serde_json::from_value::<EmptySubConfig>(c).map(|_| true)?,
            None => false,
        };
        Ok(Self { enabled, coin })
    }
}

#[async_trait]
impl EventStreamer for TendermintBalanceEventStreamer {
    type DataInType = NoDataIn;

    fn streamer_id(&self) -> String { format!("BALANCE:{}", self.coin.ticker()) }

    async fn handle(
        self,
        broadcaster: Controller<Event>,
        ready_tx: oneshot::Sender<Result<(), String>>,
        _: impl StreamHandlerInput<NoDataIn>,
    ) {
        const RECEIVER_DROPPED_MSG: &str = "Receiver is dropped, which should never happen.";
        let streamer_id = self.streamer_id();
        let coin = self.coin;

        fn generate_subscription_query(query_filter: String) -> String {
            let mut params = serde_json::Map::with_capacity(1);
            params.insert("query".to_owned(), RpcValue::String(query_filter));

            let q = MethodCall {
                id: RpcId::Num(0),
                jsonrpc: Some(RpcVersion::V2),
                method: "subscribe".to_owned(),
                params: RpcParams::Map(params),
            };

            serde_json::to_string(&q).expect("This should never happen")
        }

        let ctx = match MmArc::from_weak(&coin.ctx) {
            Some(ctx) => ctx,
            None => {
                let msg = "MM context must have been initialized already.";
                ready_tx.send(Err(msg.to_owned())).expect(RECEIVER_DROPPED_MSG);
                panic!("{}", msg);
            },
        };

        let account_id = coin.account_id.to_string();
        let mut current_balances: HashMap<String, BigDecimal> = HashMap::new();

        let receiver_q = generate_subscription_query(format!("coin_received.receiver = '{}'", account_id));
        let receiver_q = tokio_tungstenite_wasm::Message::Text(receiver_q);

        let spender_q = generate_subscription_query(format!("coin_spent.spender = '{}'", account_id));
        let spender_q = tokio_tungstenite_wasm::Message::Text(spender_q);

        ready_tx.send(Ok(())).expect(RECEIVER_DROPPED_MSG);

        loop {
            let node_uri = match coin.rpc_client().await {
                Ok(client) => client.uri(),
                Err(e) => {
                    log::error!("{e}");
                    continue;
                },
            };

            let socket_address = format!("{}/{}", http_uri_to_ws_address(node_uri), "websocket");

            let mut wsocket = match tokio_tungstenite_wasm::connect(socket_address).await {
                Ok(ws) => ws,
                Err(e) => {
                    log::error!("{e}");
                    continue;
                },
            };

            // Filter received TX events
            if let Err(e) = wsocket.send(receiver_q.clone()).await {
                log::error!("{e}");
                continue;
            }

            // Filter spent TX events
            if let Err(e) = wsocket.send(spender_q.clone()).await {
                log::error!("{e}");
                continue;
            }

            while let Some(message) = wsocket.next().await {
                let msg = match message {
                    Ok(tokio_tungstenite_wasm::Message::Text(data)) => data.clone(),
                    Ok(tokio_tungstenite_wasm::Message::Close(_)) => break,
                    Err(err) => {
                        log::error!("Server returned an unknown message type - {err}");
                        break;
                    },
                    _ => continue,
                };

                // Here, we receive raw data from the socket.
                // To examine this data, you can use tools like wscat/websocat or visit
                // https://pastebin.pl/view/499cbf2c for sample data.
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&msg) {
                    let transfers: Vec<String> =
                        serde_json::from_value(json_val["result"]["events"]["transfer.amount"].clone())
                            .unwrap_or_default();

                    let denoms: HashSet<String> = transfers
                        .iter()
                        .map(|t| {
                            let amount: String = t.chars().take_while(|c| c.is_numeric()).collect();
                            let denom = &t[amount.len()..];
                            denom.to_owned()
                        })
                        .collect();

                    let mut balance_updates = vec![];
                    for denom in denoms {
                        if let Some((ticker, decimals)) = coin.active_ticker_and_decimals_from_denom(&denom) {
                            let balance_denom = match coin.account_balance_for_denom(&coin.account_id, denom).await {
                                Ok(balance_denom) => balance_denom,
                                Err(e) => {
                                    log::error!("Failed getting balance for '{ticker}'. Error: {e}");
                                    let e = serde_json::to_value(e).expect("Serialization should't fail.");
                                    broadcaster.broadcast(Event::err(streamer_id.clone(), e, None)).await;

                                    continue;
                                },
                            };

                            let balance_decimal = big_decimal_from_sat_unsigned(balance_denom, decimals);

                            // Only broadcast when balance is changed
                            let mut broadcast = false;
                            if let Some(balance) = current_balances.get_mut(&ticker) {
                                if *balance != balance_decimal {
                                    *balance = balance_decimal.clone();
                                    broadcast = true;
                                }
                            } else {
                                current_balances.insert(ticker.clone(), balance_decimal.clone());
                                broadcast = true;
                            }

                            if broadcast {
                                balance_updates.push(json!({
                                    "ticker": ticker,
                                    "balance": { "spendable": balance_decimal, "unspendable": BigDecimal::default() }
                                }));
                            }
                        }
                    }

                    if !balance_updates.is_empty() {
                        broadcaster
                            .broadcast(Event::new(streamer_id.clone(), json!(balance_updates), None))
                            .await;
                    }
                }
            }
        }
    }
}
