use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use crate::{controller::Controller, Event, EventStreamer};
use common::executor::abortable_queue::WeakSpawner;
use common::log;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use tokio::sync::mpsc::Receiver;

/// The errors that could originate from the streaming manager.
#[derive(Debug)]
pub enum StreamingManagerError {
    /// There is no streamer with the given ID.
    StreamerNotFound,
    /// Couldn't send the data to the streamer.
    SendError(String),
    /// The streamer doesn't accept an input.
    NoDataIn,
    /// Couldn't spawn the streamer.
    SpawnError(String),
    /// The client is not known/registered.
    UnknownClient,
    /// A client with the same ID already exists.
    ClientExists,
    /// The client is already listening to the streamer.
    ClientAlreadyListening,
}

struct StreamerInfo {
    /// The communication channel to the streamer.
    data_in: Option<UnboundedSender<Box<dyn Any + Send>>>,
    /// Clients the streamer is serving for.
    clients: HashSet<u64>,
    /// The shutdown handle of the streamer.
    shutdown: oneshot::Sender<()>,
}

impl StreamerInfo {
    fn new(data_in: Option<UnboundedSender<Box<dyn Any + Send>>>, shutdown: oneshot::Sender<()>) -> Self {
        Self {
            data_in,
            clients: HashSet::new(),
            shutdown,
        }
    }

    fn is_down(&self) -> bool { self.shutdown.is_canceled() }
}

#[derive(Clone, Default)]
pub struct StreamingManager {
    /// A map from streamer IDs to their communication channels (if present) and shutdown handles.
    // Using an RwLock assuming `StreamingManager::send` will be heavily used.
    streamers: Arc<RwLock<HashMap<String, StreamerInfo>>>,
    /// The stream-out controller/broadcaster that all streamers use to stream data to the clients (using SSE).
    // Using an RwLock assuming `StreamingManager::broadcast` will be heavily used.
    controller: Arc<RwLock<Controller<Event>>>,
    /// An inverse map from clients to the streamers they are listening to.
    /// Lists known clients and is always kept in sync with `streamers`.
    // NOTE: Any operation writing to this field will also write to `streamers` and potentially `controller`.
    // Make sure to lock this field first and all the way until the changes are made to other fields.
    clients: Arc<Mutex<HashMap<u64, HashSet<String>>>>,
}

impl StreamingManager {
    /// Spawns and adds a new streamer `streamer` to the manager.
    pub async fn add(
        &self,
        client_id: u64,
        streamer: impl EventStreamer,
        spawner: WeakSpawner,
    ) -> Result<String, StreamingManagerError> {
        let streamer_id = streamer.streamer_id();
        // Remove the streamer if it died for some reason.
        self.remove_streamer_if_down(&streamer_id);

        // Pre-checks before spawning the streamer. Inside another scope to drop the locks early.
        {
            let mut clients = self.clients.lock().unwrap();
            let mut streamers = self.streamers.write().unwrap();

            match clients.get(&client_id) {
                // We don't know that client. We don't have a connection to it.
                None => return Err(StreamingManagerError::UnknownClient),
                // The client is already listening to that streamer.
                Some(listening_to) if listening_to.contains(&streamer_id) => {
                    return Err(StreamingManagerError::ClientAlreadyListening);
                },
                _ => (),
            }

            // If a streamer is already up and running, we won't spawn another one.
            if let Some(streamer_info) = streamers.get_mut(&streamer_id) {
                // Register the client as a listener to the streamer.
                streamer_info.clients.insert(client_id);
                // Register the streamer as listened-to by the client.
                clients
                    .get_mut(&client_id)
                    .map(|listening_to| listening_to.insert(streamer_id.clone()));
                // FIXME: The streamer running is probably running with different configuration.
                // We might want to inform the client that the configuration they asked for wasn't
                // applied and return the active configuration instead?
                //
                // Note that there is no way to (re)spawn a streamer with a different configuration
                // unless then we want to have different streamer for each and every client!
                //
                // Another restricted solution is to not let clients set the streamer configuration
                // and only allow it through MM2 configuration file.
                return Ok(streamer_id);
            }
        }

        // Spawn a new streamer.
        let (shutdown, data_in) = streamer
            .spawn(spawner, self.clone())
            .await
            .map_err(StreamingManagerError::SpawnError)?;
        let streamer_info = StreamerInfo::new(data_in, shutdown);

        // Note that we didn't hold the locks while spawning the streamer (potentially a long operation).
        // This means we can't assume either that the client still exists at this point or
        // that the streamer still doesn't exist.
        let mut clients = self.clients.lock().unwrap();
        if let Some(listening_to) = clients.get_mut(&client_id) {
            listening_to.insert(streamer_id.clone());
            self.streamers
                .write()
                .unwrap()
                .entry(streamer_id.clone())
                .or_insert(streamer_info)
                .clients
                .insert(client_id);
        } else {
            // The client was removed while we were spawning the streamer.
            // We no longer have a connection for it.
            return Err(StreamingManagerError::UnknownClient);
        }
        Ok(streamer_id)
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
            .map_err(|e| StreamingManagerError::SendError(e.to_string()))
    }

    /// Stops streaming from the streamer with `streamer_id` to the client with `client_id`.
    pub fn stop(&self, client_id: u64, streamer_id: &str) -> Result<(), StreamingManagerError> {
        let mut clients = self.clients.lock().unwrap();
        if let Some(listening_to) = clients.get_mut(&client_id) {
            listening_to.remove(streamer_id);

            let mut streamers = self.streamers.write().unwrap();
            streamers
                .get_mut(streamer_id)
                .ok_or(StreamingManagerError::StreamerNotFound)?
                .clients
                .remove(&client_id);

            // If there are no more listening clients, terminate the streamer.
            if streamers.get(streamer_id).map(|info| info.clients.len()) == Some(0) {
                streamers.remove(streamer_id);
            }
            Ok(())
        } else {
            Err(StreamingManagerError::UnknownClient)
        }
    }

    /// Broadcasts some event to clients listening to it.
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
        let mut clients = self.clients.lock().unwrap();
        if clients.contains_key(&client_id) {
            return Err(StreamingManagerError::ClientExists);
        }
        clients.insert(client_id, HashSet::new());
        // Note that events queued in the channel are `Arc<` shared.
        // So a 1024 long buffer isn't actually heavy on memory.
        Ok(self.controller.write().unwrap().create_channel(client_id, 1024))
    }

    pub fn remove_client(&self, client_id: u64) -> Result<(), StreamingManagerError> {
        let mut clients = self.clients.lock().unwrap();
        // Remove the client from our known-clients map.
        let listening_to = clients.remove(&client_id).ok_or(StreamingManagerError::UnknownClient)?;
        // Remove the client from all the streamers it was listening to.
        let mut streamers = self.streamers.write().unwrap();
        for streamer_id in listening_to {
            if let Some(streamer_info) = streamers.get_mut(&streamer_id) {
                streamer_info.clients.remove(&client_id);
            } else {
                log::error!(
                    "Client {client_id} was listening to a non-existent streamer {streamer_id}. This is a bug!"
                );
            }
            // If there are no more listening clients, terminate the streamer.
            if streamers.get(&streamer_id).map(|info| info.clients.len()) == Some(0) {
                streamers.remove(&streamer_id);
            }
        }
        // Remove our channel with this client.
        self.controller.write().unwrap().remove_channel(&client_id);
        Ok(())
    }

    /// Removes a streamer if it is no longer running.
    ///
    /// Aside from us shutting down a streamer when all its clients are disconnected,
    /// the streamer might die by itself (e.g. the spawner it was spawned with aborted).
    /// In this case, we need to remove the streamer and de-list it from all clients.
    fn remove_streamer_if_down(&self, streamer_id: &str) {
        let mut clients = self.clients.lock().unwrap();
        let mut streamers = self.streamers.write().unwrap();
        if let Some(streamer_info) = streamers.get(streamer_id) {
            // Remove the streamer from all clients listening to it.
            if streamer_info.is_down() {
                for client_id in &streamer_info.clients {
                    clients
                        .get_mut(client_id)
                        .map(|listening_to| listening_to.remove(streamer_id));
                }
                // And remove the streamer from our registry.
                streamers.remove(streamer_id);
            }
        }
    }
}
