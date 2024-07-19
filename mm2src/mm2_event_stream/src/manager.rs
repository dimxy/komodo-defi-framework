use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::{Controller, Event, EventStreamer};
use common::executor::abortable_queue::WeakSpawner;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use tokio::sync::mpsc::Receiver;

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
    /// The client is not known/registered.
    UnknownClient(u64),
    /// A client with the same ID already exists.
    ClientExists(u64),
    /// The client is already listening to the streamer.
    ClientAlreadyListening(u64, String),
}

struct StreamerInfo {
    /// The communication channel to the streamer.
    data_in: Option<UnboundedSender<Box<dyn Any + Send>>>,
    /// Clients the streamer is serving for.
    clients: HashSet<u64>,
    /// The shutdown handle of the streamer.
    _shutdown: oneshot::Sender<()>,
}

impl StreamerInfo {
    fn new(data_in: Option<UnboundedSender<Box<dyn Any + Send>>>, shutdown: oneshot::Sender<()>) -> Self {
        Self {
            data_in,
            clients: HashSet::new(),
            _shutdown: shutdown,
        }
    }
}

#[derive(Clone, Default)]
pub struct StreamingManager {
    /// A map from streamer IDs to their communication channels (if present) and shutdown handles.
    streamers: Arc<RwLock<HashMap<String, StreamerInfo>>>,
    /// The stream-out controller/broadcaster that all streamers use to stream data to the clients (using SSE).
    controller: Arc<RwLock<Controller<Event>>>,
    /// An inverse map from clients to the streamers they are listening to.
    clients: Arc<RwLock<HashMap<u64, HashSet<String>>>>,
}

impl StreamingManager {
    /// Spawns and adds a new streamer `streamer` to the manager.
    pub async fn add(
        &self,
        client_id: u64,
        streamer: impl EventStreamer,
        spawner: WeakSpawner,
    ) -> Result<(), StreamingManagerError> {
        // Make sure we know this client.
        if !self.clients.read().unwrap().contains_key(&client_id) {
            return Err(StreamingManagerError::UnknownClient(client_id));
        }
        let streamer_id = streamer.streamer_id();
        // If a streamer is already up and running, add the client to it and return.
        if let Some(streamer_info) = self.streamers.write().unwrap().get_mut(&streamer_id) {
            if streamer_info.clients.contains(&client_id) {
                // FIXME: such info could have been known from `client_to_streamers` map
                // check the atomicity of these operations (considering we are running inside an async context).
                return Err(StreamingManagerError::ClientAlreadyListening(client_id, streamer_id));
            }
            streamer_info.clients.insert(client_id);
            // FIXME: The streamer running is probably running with different configuration.
            // We might want to inform the client that the configuration they asked for wasn't
            // applied and return the active configuration instead?
            // Note that there is no way to (re)spawn a streamer with a different configuration
            // unless then we want to have different streamer for each and every client!
            return Ok(());
        }
        // Otherwise spawn a new streamer.
        let (shutdown, data_in) = streamer
            .spawn(spawner, self.clone())
            .await
            .map_err(StreamingManagerError::SpawnError)?;
        let streamer_info = StreamerInfo::new(data_in, shutdown);
        // Note that while we were spawning the streamer (potentially a long operation) and not holding the lock
        // to ths streamers map (and we indeed shouldn't), another client might have added the same streamer.
        // So we shouldn't assume that the streamer doesn't exist based solely on the check above.
        self.streamers
            .write()
            .unwrap()
            .entry(streamer_id.clone())
            .or_insert(streamer_info)
            .clients
            .insert(client_id);
        self.clients
            .write()
            .unwrap()
            .entry(client_id)
            .or_insert(HashSet::new())
            .insert(streamer_id);
        Ok(())
    }

    /// Sends data to a streamer with `streamer_id`.
    pub fn send<T: Send + 'static>(&self, streamer_id: &str, data: T) -> Result<(), StreamingManagerError> {
        let streamers = self.streamers.read().unwrap();
        let streamer_info = streamers
            .get(streamer_id)
            .ok_or(StreamingManagerError::StreamerNotFound)?;
        let data_in = streamer_info.data_in.as_ref().ok_or(StreamingManagerError::NoDataIn)?;
        data_in
            .unbounded_send(Box::new(data))
            .map_err(|_| StreamingManagerError::SendError)
    }

    /// Stops streaming from the streamer with `streamer_id` to the client with `client_id`.
    pub fn stop(&self, client_id: u64, streamer_id: &str) {
        self.clients.write().unwrap().get_mut(&client_id).map(|streamer_ids| {
            streamer_ids.remove(streamer_id);
        });
        let mut streamers = self.streamers.write().unwrap();
        streamers.get_mut(streamer_id).map(|info| {
            info.clients.remove(&client_id);
        });
        // If there are no more listening clients, terminate the streamer.
        if streamers.get(streamer_id).map(|info| info.clients.len()) == Some(0) {
            streamers.remove(streamer_id);
        }
    }

    /// Broadcasts some event directly to listening clients.
    ///
    /// In contrast to `StreamingManager::send`, which sends some data to a streamer,
    /// this method broadcasts an event to the listening *clients* directly, independently
    /// of any streamer (i.e. bypassing any streamer).
    pub fn broadcast(&self, event: Event) {
        if let Some(client_ids) = self
            .streamers
            .read()
            .unwrap()
            .get(event.origin())
            .map(|info| &info.clients)
        {
            self.controller.read().unwrap().broadcast(event, Some(client_ids))
        }
    }

    /// Forcefully broadcasts an event to all clients even if they are not listening for such an event.
    pub fn broadcast_all(&self, event: Event) { self.controller.read().unwrap().broadcast(event, None) }

    pub fn new_client(&self, client_id: u64) -> Result<Receiver<Arc<Event>>, StreamingManagerError> {
        let mut clients = self.clients.write().unwrap();
        if clients.contains_key(&client_id) {
            return Err(StreamingManagerError::ClientExists(client_id));
        }
        clients.insert(client_id, HashSet::new());
        // Note that events queued in the channel are `Arc` shared.
        // So 1024 long buffer won't be heavy on memory.
        Ok(self.controller.write().unwrap().create_channel(client_id, 1024))
    }

    pub fn remove_client(&self, client_id: u64) -> Result<(), StreamingManagerError> {
        let streamer_ids = self
            .clients
            .write()
            .unwrap()
            .remove(&client_id)
            .ok_or(StreamingManagerError::UnknownClient(client_id))?;
        self.controller.write().unwrap().remove_channel(&client_id);

        let mut streamers = self.streamers.write().unwrap();
        for streamer_id in streamer_ids {
            if let Some(streamer_info) = streamers.get_mut(&streamer_id) {
                streamer_info.clients.remove(&client_id);
            };
            // If there are no more listening clients, terminate the streamer.
            if streamers.get(&streamer_id).map(|info| info.clients.len()) == Some(0) {
                streamers.remove(&streamer_id);
            }
        }
        Ok(())
    }
}
