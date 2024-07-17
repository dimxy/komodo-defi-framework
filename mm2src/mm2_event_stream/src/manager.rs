use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::{Controller, Event, EventStreamer};

use common::executor::abortable_queue::WeakSpawner;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;

/// The errors that could originate from the streaming manager.
#[derive(Debug)]
pub enum StreamingManagerError {
    /// There is no streamer with the given ID.
    StreamerNotFound,
    /// Couldn't send the data to the streamer.
    SendError,
    /// The streamer doesn't accept an input.
    NoDataIn,
    /// Couldn't spawn the streamer.
    SpawnError(String),
    /// Couldn't add the streamer, because it already exists.
    AddError(String),
}

#[derive(Clone, Default)]
pub struct StreamingManager {
    /// A map from streamer IDs to their communication channels (if present) and shutdown handles.
    streamers: Arc<RwLock<HashMap<String, (oneshot::Sender<()>, Option<UnboundedSender<Box<dyn Any + Send>>>)>>>,
    /// The stream-out controller/broadcaster that all streamers use to stream data to the clients (using SSE).
    controller: Controller<Event>,
}

impl StreamingManager {
    /// Spawns and adds a new streamer `streamer` to the manager.
    pub async fn add(&self, streamer: impl EventStreamer, spawner: WeakSpawner) -> Result<(), StreamingManagerError> {
        let streamer_id = streamer.streamer_id();
        // NOTE: We spawn the streamer *before* checking if it can be added or not because
        // we don't know how long will it take to spawn up and we don't want to lock the
        // manager for too long.
        let channels = streamer
            .spawn(spawner, self.controller())
            .await
            .map_err(StreamingManagerError::SpawnError)?;
        let mut streamers = self.streamers.write().unwrap();
        // If that streamer already exists, refuse to add it.
        if streamers.contains_key(&streamer_id) {
            return Err(StreamingManagerError::AddError(format!(
                "A streamer with the same id ({streamer_id}) exists, it must be shutdown before re-using the same id."
            )));
        }
        streamers.insert(streamer_id, channels);
        Ok(())
    }

    /// Sends data to a streamer of a known ID.
    pub fn send<T: Send + 'static>(&self, streamer_id: &str, data: T) -> Result<(), StreamingManagerError> {
        let streamers = self.streamers.read().unwrap();
        let (_, data_in) = streamers
            .get(streamer_id)
            .ok_or(StreamingManagerError::StreamerNotFound)?;
        let data_in = data_in.as_ref().ok_or(StreamingManagerError::NoDataIn)?;
        data_in
            .unbounded_send(Box::new(data))
            .map_err(|_| StreamingManagerError::SendError)
    }

    /// Shuts down a streamer of a known ID.
    pub fn shut(&self, streamer_id: &str) -> Result<(), StreamingManagerError> {
        self.streamers
            .write()
            .unwrap()
            .remove(streamer_id)
            .ok_or(StreamingManagerError::StreamerNotFound)?;
        Ok(())
    }

    /// Broadcasts some event directly to listening clients.
    ///
    /// In contrast to `StreamingManager::send`, which sends some data to a streamer,
    /// this method broadcasts an event to the listening *clients* directly, independently
    /// of any streamer (i.e. bypassing any streamer).
    pub async fn broadcast(&self, event: Event) { self.controller.broadcast(event).await }

    /// Returns the controller/broadcaster (the middle tie between the streamers and the clients).
    pub fn controller(&self) -> Controller<Event> { self.controller.clone() }
}
