use crate::common::Future01CompatExt;
use crate::streaming_events_config::BalanceEventConfig;
use crate::z_coin::ZCoin;
use crate::{MarketCoinOps, MmCoin};

use async_trait::async_trait;
use common::executor::{AbortSettings, SpawnAbortable};
use common::log::{error, info};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use futures::lock::Mutex as AsyncMutex;
use futures_util::StreamExt;
use mm2_core::mm_ctx::MmArc;
use mm2_event_stream::behaviour::EventBehaviour;
use mm2_event_stream::{ErrorEventName, Event, EventName};
use serde::Deserialize;
use serde_json::Value as Json;
use std::sync::Arc;

pub type ZBalanceEventSender = UnboundedSender<()>;
pub type ZBalanceEventHandler = Arc<AsyncMutex<UnboundedReceiver<()>>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyConfig {}

pub struct ZCoinBalanceEventStreamer {
    /// Whether the event is enabled for this coin.
    enabled: bool,
    coin: ZCoin,
}

impl ZCoinBalanceEventStreamer {
    pub fn try_new(config: Json, coin: ZCoin) -> serde_json::Result<Self> {
        let config: BalanceEventConfig = serde_json::from_value(config)?;
        let enabled = match config.find_coin(coin.ticker()) {
            // This is just an extra check to make sure the config is correct (no config)
            Some(c) => serde_json::from_value::<EmptyConfig>(c).map(|_| true)?,
            None => false,
        };
        Ok(Self { enabled, coin })
    }
}

#[async_trait]
impl EventBehaviour for ZCoinBalanceEventStreamer {
    fn event_name() -> EventName { EventName::CoinBalance }

    fn error_event_name() -> ErrorEventName { ErrorEventName::CoinBalanceError }

    async fn handle(self, tx: oneshot::Sender<Result<(), String>>) {
        const RECEIVER_DROPPED_MSG: &str = "Receiver is dropped, which should never happen.";
        let coin = self.coin;

        macro_rules! send_status_on_err {
            ($match: expr, $sender: tt, $msg: literal) => {
                match $match {
                    Some(t) => t,
                    None => {
                        $sender.send(Err($msg.to_owned())).expect(RECEIVER_DROPPED_MSG);
                        panic!("{}", $msg);
                    },
                }
            };
        }

        let ctx = send_status_on_err!(
            MmArc::from_weak(&coin.as_ref().ctx),
            tx,
            "MM context must have been initialized already."
        );
        let z_balance_change_handler = send_status_on_err!(
            coin.z_fields.z_balance_event_handler.as_ref(),
            tx,
            "Z balance change receiver can not be empty."
        );

        tx.send(Ok(())).expect(RECEIVER_DROPPED_MSG);

        // Locks the balance change handler, iterates through received events, and updates balance changes accordingly.
        let mut bal = z_balance_change_handler.lock().await;
        while (bal.next().await).is_some() {
            match coin.my_balance().compat().await {
                Ok(balance) => {
                    let payload = json!({
                        "ticker": coin.ticker(),
                        "address": coin.my_z_address_encoded(),
                        "balance": { "spendable": balance.spendable, "unspendable": balance.unspendable }
                    });

                    ctx.stream_channel_controller
                        .broadcast(Event::new(Self::event_name().to_string(), payload.to_string()))
                        .await;
                },
                Err(err) => {
                    let ticker = coin.ticker();
                    error!("Failed getting balance for '{ticker}'. Error: {err}");
                    let e = serde_json::to_value(err).expect("Serialization should't fail.");
                    return ctx
                        .stream_channel_controller
                        .broadcast(Event::new(
                            format!("{}:{}", Self::error_event_name(), ticker),
                            e.to_string(),
                        ))
                        .await;
                },
            };
        }
    }

    async fn spawn(self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        info!(
            "{} event is activated for {} address {}.",
            Self::event_name(),
            self.coin.ticker(),
            self.coin.my_z_address_encoded(),
        );

        let (tx, rx) = oneshot::channel();
        let settings = AbortSettings::info_on_abort(format!(
            "{} event is stopped for {}.",
            Self::event_name(),
            self.coin.ticker()
        ));
        self.coin.spawner().spawn_with_settings(self.handle(tx), settings);

        rx.await.unwrap_or_else(|e| {
            Err(format!(
                "The handler was aborted before sending event initialization status: {e}"
            ))
        })
    }
}
