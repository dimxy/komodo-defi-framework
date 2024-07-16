use std::any::{self, Any};

use common::executor::{abortable_queue::WeakSpawner, AbortSettings, SpawnAbortable};
use common::log::{error, info};

use async_trait::async_trait;
use futures::channel::{mpsc, oneshot};
use futures::{select, FutureExt, Stream, StreamExt};

/// A marker to indicate that the event streamer doesn't take any input data.
pub struct NoDataIn;

/// A mixture trait combining `Stream`, `Send` & `Unpin` together (to avoid confusing annotation).
pub trait StreamHandlerInput<D>: Stream<Item = D> + Send + Unpin {}
/// Implement the trait for all types `T` that implement `Stream<Item = D> + Send + Unpin` for any `D`.
impl<T, D> StreamHandlerInput<D> for T where T: Stream<Item = D> + Send + Unpin {}

#[async_trait]
pub trait EventStreamer
where
    Self: Sized + Send + 'static,
{
    type DataInType: Send;

    /// Returns a human readable unique identifier for the event streamer.
    /// No other event streamer should have the same identifier.
    fn streamer_id(&self) -> String;

    /// Event handler that is responsible for broadcasting event data to the streaming channels.
    ///
    /// `ready_tx` is a oneshot sender that is used to send the initialization status of the event.
    /// `data_rx` is a receiver that the streamer *could* use to receive data from the outside world.
    async fn handle(
        self,
        ready_tx: oneshot::Sender<Result<(), String>>,
        data_rx: impl StreamHandlerInput<Self::DataInType>,
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
        let (any_data_sender, any_data_receiver) = mpsc::unbounded::<Box<dyn Any + Send>>();
        // A middleware to cast the data of type `Box<dyn Any>` to the actual input datatype of this streamer.
        let data_receiver = any_data_receiver.filter_map({
            let streamer_id = streamer_id.clone();
            move |any_input_data| {
                let streamer_id = streamer_id.clone();
                Box::pin(async move {
                    if let Ok(input_data) = any_input_data.downcast() {
                        Some(*input_data)
                    } else {
                        error!("Couldn't downcast a received message to {}. This message wasn't intended to be sent to this streamer ({streamer_id}).", any::type_name::<Self::DataInType>());
                        None
                    }
                })
            }
        });

        let handler_with_shutdown = {
            let streamer_id = streamer_id.clone();
            async move {
                select! {
                    _ = rx_shutdown.fuse() => {
                        info!("Manually shutting down event streamer: {streamer_id}.")
                    }
                    _ = self.handle(tx_ready, data_receiver).fuse() => {}
                }
            }
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
            Ok((tx_shutdown, Some(any_data_sender)))
        }
    }
}
