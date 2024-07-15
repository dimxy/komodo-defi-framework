use std::any::Any;
use std::collections::HashMap;

use super::EventStreamer;
use common::executor::abortable_queue::WeakSpawner;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;

/// The errors that could originate from the streaming manager.
pub enum StreamingSendError {
    /// There is no streamer with the given ID.
    StreamerNotFound,
    /// Couldn't send the data to the streamer.
    SendError,
    /// The streamer doesn't accept an input.
    NoDataIn,
}

pub struct StreamingManager {
    /// A map from streamer IDs to their communication channels (if present) and shutdown handles.
    streamers: HashMap<String, (Option<UnboundedSender<Box<dyn Any + Send>>>, oneshot::Sender<()>)>,
}

impl StreamingManager {
    /// Spawns and adds a new streamer `streamer` to the manager.
    pub async fn add(&mut self, streamer: impl EventStreamer, spawner: WeakSpawner) -> Result<(), String> {
        let streamer_id = streamer.streamer_id();
        // Spawn the streamer.
        streamer.spawn(spawner).await.map(|(shutdown_signal, data_in_channel)| {
            // And store its handles if spawned successfully.
            self.streamers
                .insert(streamer_id.clone(), (data_in_channel, shutdown_signal))
                // Two different streamers shouldn't conflict if IDs are unique, so this is a bug.
                .map(|_| {
                    common::log::error!(
                        "A streamer with the same id ({}) existed and now has been removed.",
                        streamer_id
                    )
                });
        })
    }

    /// Sends data to a streamer of a known ID.
    pub fn send<T: Send + 'static>(&self, streamer_id: &str, data: T) -> Result<(), StreamingSendError> {
        let (data_in, _) = self
            .streamers
            .get(streamer_id)
            .ok_or(StreamingSendError::StreamerNotFound)?;
        let data_in = data_in.as_ref().ok_or(StreamingSendError::NoDataIn)?;
        data_in
            .unbounded_send(Box::new(data))
            .map_err(|_| StreamingSendError::SendError)
    }

    /// Shuts down a streamer of a known ID.
    pub fn shut(&mut self, streamer_id: &str) -> Result<(), StreamingSendError> {
        self.streamers
            .remove(streamer_id)
            .ok_or(StreamingSendError::StreamerNotFound)?;
        Ok(())
    }
}
