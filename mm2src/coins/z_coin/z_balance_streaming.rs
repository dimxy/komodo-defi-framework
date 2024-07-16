use crate::common::Future01CompatExt;
use crate::streaming_events_config::{BalanceEventConfig, EmptySubConfig};
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
use mm2_event_stream::{Event, EventStreamer, StreamerHandleInput};
use serde_json::Value as Json;
use std::sync::Arc;

pub type ZBalanceEventSender = UnboundedSender<()>;
pub type ZBalanceEventHandler = Arc<AsyncMutex<UnboundedReceiver<()>>>;

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
            Some(c) => serde_json::from_value::<EmptySubConfig>(c).map(|_| true)?,
            None => false,
        };
        Ok(Self { enabled, coin })
    }
}

#[async_trait]
impl EventStreamer for ZCoinBalanceEventStreamer {
    type DataInType = ();

    fn streamer_id(&self) -> String { format!("BALANCE:{}", self.coin.ticker()) }

    async fn handle(self, tx: oneshot::Sender<Result<(), String>>, data_rx: impl StreamerHandleInput<()>) {
        const RECEIVER_DROPPED_MSG: &str = "Receiver is dropped, which should never happen.";
        let streamer_id = self.streamer_id();
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
                        .broadcast(Event::new(streamer_id.clone(), payload, None))
                        .await;
                },
                Err(err) => {
                    let ticker = coin.ticker();
                    error!("Failed getting balance for '{ticker}'. Error: {err}");
                    let e = serde_json::to_value(err).expect("Serialization should't fail.");
                    return ctx
                        .stream_channel_controller
                        .broadcast(Event::err(streamer_id.clone(), e, None))
                        .await;
                },
            };
        }
    }
}
