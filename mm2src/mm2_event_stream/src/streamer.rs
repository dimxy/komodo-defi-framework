use std::any::{self, Any};

use common::executor::{abortable_queue::WeakSpawner, AbortSettings, SpawnAbortable, SpawnFuture};
use common::log::{error, info};

use async_trait::async_trait;
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;

/// A marker to indicate that the event streamer doesn't take any input data.
pub struct NoDataIn;

#[async_trait]
pub trait EventStreamer
where
    Self: Sized + Send + 'static,
{
    type DataInType: Send;

    /// Returns a human readable unique identifier for the event streamer.
    /// No other event streamer should have the same identifier.
    fn streamer_id(&self) -> String { unimplemented!() }

    /// Event handler that is responsible for broadcasting event data to the streaming channels.
    ///
    /// `ready_tx` is a oneshot sender that is used to send the initialization status of the event.
    /// `data_rx` is a receiver that the streamer *could* use to receive data from the outside world.
    async fn handle(
        self,
        ready_tx: oneshot::Sender<Result<(), String>>,
        data_rx: mpsc::UnboundedReceiver<Self::DataInType>,
    );

    /// Spawns the `Self::handle` in a separate thread.
    ///
    /// Returns a `oneshot::Sender` to shutdown the handler and an optional `mpsc::UnboundedSender`
    /// to send data to the handler.
    ///
    /// This method should not be overridden.
    async fn spawn(
        self,
        // FIXME: Might be better to let the implementors store the spawner themselves and
        // we can use `self.spawner()` here to get it.
        // Also for AbortSettings, we can make this customizable with a default impl.
        spawner: WeakSpawner,
    ) -> Result<(oneshot::Sender<()>, Option<mpsc::UnboundedSender<Box<dyn Any + Send>>>), String> {
        let streamer_id = self.streamer_id();
        info!("Spawning event streamer: {streamer_id}");

        // A oneshot channel to receive the initialization status of the handler through.
        let (tx_ready, ready_rx) = oneshot::channel();
        // A oneshot channel to shutdown the handler.
        let (tx_shutdown, rx_shutdown) = oneshot::channel::<()>();
        // An unbounded channel to send data to the handler.
        let (data_sender, data_receiver) = mpsc::unbounded();

        // FIXME: select! between shutdown or handle here.
        let handler_with_shutdown = async move {
            drop(rx_shutdown);
            self.handle(tx_ready, data_receiver).await;
        };
        let settings = AbortSettings::info_on_abort(format!("{streamer_id} streamer has stopped."));
        spawner.spawn_with_settings(handler_with_shutdown, settings);

        ready_rx.await.unwrap_or_else(|e| {
            Err(format!(
                "The handler was aborted before sending event initialization status: {e}"
            ))
        })?;

        // If the handler takes no input data, return `None` for the data sender.
        if any::TypeId::of::<Self::DataInType>() == any::TypeId::of::<NoDataIn>() {
            Ok((tx_shutdown, None))
        } else {
            let (any_data_sender, mut rx) = mpsc::unbounded::<Box<dyn Any + Send>>();

            // To store the data senders in the `StreamingManager`, they have to be casted to the same type
            // (`UnboundedSender<Box<dyn Any>>`). But for convenience, we want the implementors of the trait
            // to think that the sending channel is of type `UnboundedSender<Self::DataInType>`.
            // This spawned task will listen to the incoming `Box<dyn Any>` data and convert it to `Self::DataInType`.
            //
            // One other solution is to let `Self::handle` take a `UnboundedReceiver<Box<dyn Any>>` and cast
            // it internally, but this means this (confusing) boiler plate will need to be done for each
            // implementation of this trait.
            //
            // FIXME: Check if something like `rx_to_stream` (rpc_clients.rs) could work without an extra thread.
            spawner.spawn(async move {
                while let Some(any_input_data) = rx.next().await {
                    if let Ok(input_data) = any_input_data.downcast() {
                        if let Err(_) = data_sender.unbounded_send(*input_data) {
                            // The handler dropped the data receiver.
                            return;
                        }
                    }
                    else {
                        error!("Couldn't downcast a received message to {}. This message wasn't intended to be sent to this streamer ({streamer_id}).", any::type_name::<Self::DataInType>());
                    }
                }
            });

            Ok((tx_shutdown, Some(any_data_sender)))
        }
    }
}
