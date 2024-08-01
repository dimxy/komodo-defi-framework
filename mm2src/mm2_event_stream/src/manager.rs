use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::{controller::Controller, Broadcaster, Event, EventStreamer};
use common::executor::abortable_queue::WeakSpawner;
use common::log;

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use parking_lot::RwLock;
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
            if let Some(streamer_info) = self.streamers.write().get_mut(&streamer_id) {
                // Register the client as a listener to the streamer.
                streamer_info.clients.insert(client_id);
                // Register the streamer as listened-to by the client.
                clients
                    .get_mut(&client_id)
                    .map(|listening_to| listening_to.insert(streamer_id.clone()));
                return Ok(streamer_id);
            }
        }

        // Spawn a new streamer.
        let (shutdown, data_in) = streamer
            .spawn(spawner, Broadcaster::new(self.clone()))
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
        let streamers = self.streamers.read();
        let streamer_info = streamers
            .get(streamer_id)
            .ok_or(StreamingManagerError::StreamerNotFound)?;
        let data_in = streamer_info.data_in.as_ref().ok_or(StreamingManagerError::NoDataIn)?;
        data_in
            .unbounded_send(Box::new(data))
            .map_err(|e| StreamingManagerError::SendError(e.to_string()))
    }

    /// Same as `StreamingManager::send`, but computes that data to send to a streamer using a closure,
    /// thus avoiding computations & cloning if the intended streamer isn't running (more like the
    /// laziness of `*_or_else()` functions).
    ///
    /// `data_fn` will only be evaluated if the streamer is found and accepts an input.
    pub fn send_fn<T: Send + 'static>(
        &self,
        streamer_id: &str,
        data_fn: impl FnOnce() -> T,
    ) -> Result<(), StreamingManagerError> {
        let streamers = self.streamers.read();
        let streamer_info = streamers
            .get(streamer_id)
            .ok_or(StreamingManagerError::StreamerNotFound)?;
        let data_in = streamer_info.data_in.as_ref().ok_or(StreamingManagerError::NoDataIn)?;
        data_in
            .unbounded_send(Box::new(data_fn()))
            .map_err(|e| StreamingManagerError::SendError(e.to_string()))
    }

    /// Stops streaming from the streamer with `streamer_id` to the client with `client_id`.
    pub fn stop(&self, client_id: u64, streamer_id: &str) -> Result<(), StreamingManagerError> {
        let mut clients = self.clients.lock().unwrap();
        if let Some(listening_to) = clients.get_mut(&client_id) {
            listening_to.remove(streamer_id);

            let mut streamers = self.streamers.write();
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
        if let Some(client_ids) = self.streamers.read().get(event.origin()).map(|info| &info.clients) {
            self.controller.read().broadcast(event, Some(client_ids))
        }
    }

    /// Forcefully broadcasts an event to all known clients even if they are not listening for such an event.
    pub fn broadcast_all(&self, event: Event) { self.controller.read().broadcast(event, None) }

    pub fn new_client(&self, client_id: u64) -> Result<Receiver<Arc<Event>>, StreamingManagerError> {
        let mut clients = self.clients.lock().unwrap();
        if clients.contains_key(&client_id) {
            return Err(StreamingManagerError::ClientExists);
        }
        clients.insert(client_id, HashSet::new());
        // Note that events queued in the channel are `Arc<` shared.
        // So a 1024 long buffer isn't actually heavy on memory.
        Ok(self.controller.write().create_channel(client_id, 1024))
    }

    pub fn remove_client(&self, client_id: u64) -> Result<(), StreamingManagerError> {
        let mut clients = self.clients.lock().unwrap();
        // Remove the client from our known-clients map.
        let listening_to = clients.remove(&client_id).ok_or(StreamingManagerError::UnknownClient)?;
        // Remove the client from all the streamers it was listening to.
        let mut streamers = self.streamers.write();
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
        self.controller.write().remove_channel(&client_id);
        Ok(())
    }

    /// Removes a streamer if it is no longer running.
    ///
    /// Aside from us shutting down a streamer when all its clients are disconnected,
    /// the streamer might die by itself (e.g. the spawner it was spawned with aborted).
    /// In this case, we need to remove the streamer and de-list it from all clients.
    fn remove_streamer_if_down(&self, streamer_id: &str) {
        let mut clients = self.clients.lock().unwrap();
        let mut streamers = self.streamers.write();
        if let Some(streamer_info) = streamers.get(streamer_id) {
            if streamer_info.is_down() {
                // Remove the streamer from all clients listening to it.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streamer::test_utils::{InitErrorStreamer, PeriodicStreamer, ReactiveStreamer};

    use common::executor::{abortable_queue::AbortableQueue, AbortableSystem, Timer};
    use serde_json::json;

    #[tokio::test]
    async fn test_add_remove_client() {
        let manager = StreamingManager::default();
        let client_id1 = 1;
        let client_id2 = 2;
        let client_id3 = 3;

        assert!(matches!(manager.new_client(client_id1), Ok(..)));
        // Adding the same client again should fail.
        assert!(matches!(
            manager.new_client(client_id1),
            Err(StreamingManagerError::ClientExists)
        ));
        // Adding a different new client should be OK.
        assert!(matches!(manager.new_client(client_id2), Ok(..)));

        assert!(matches!(manager.remove_client(client_id1), Ok(())));
        // Removing a removed client should fail.
        assert!(matches!(
            manager.remove_client(client_id1),
            Err(StreamingManagerError::UnknownClient)
        ));
        // Same as removing a non-existent client.
        assert!(matches!(
            manager.remove_client(client_id3),
            Err(StreamingManagerError::UnknownClient)
        ));
    }

    #[tokio::test]
    async fn test_broadcast_all() {
        // Create a manager and add register two clients with it.
        let manager = StreamingManager::default();
        let mut client1 = manager.new_client(1).unwrap();
        let mut client2 = manager.new_client(2).unwrap();
        let event = Event::new("test".to_string(), json!("test"));

        // Broadcast the event to all clients.
        manager.broadcast_all(event.clone());

        // The clients should receive the events.
        assert_eq!(*client1.try_recv().unwrap(), event);
        assert_eq!(*client2.try_recv().unwrap(), event);

        // Remove the clients.
        manager.remove_client(1).unwrap();
        manager.remove_client(2).unwrap();

        // `recv` shouldn't work at this point since the client is removed.
        assert!(client1.try_recv().is_err());
        assert!(client2.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_periodic_streamer() {
        let manager = StreamingManager::default();
        let system = AbortableQueue::default();
        let (client_id1, client_id2) = (1, 2);
        // Register a new client with the manager.
        let mut client1 = manager.new_client(client_id1).unwrap();
        // Another client whom we won't have it subscribe to the streamer.
        let mut client2 = manager.new_client(client_id2).unwrap();
        // Subscribe the new client to PeriodicStreamer.
        let streamer_id = manager
            .add(client_id1, PeriodicStreamer, system.weak_spawner())
            .await
            .unwrap();

        // We should be hooked now. try to receive some events from the streamer.
        for _ in 0..3 {
            // The streamer should send an event every 0.1s. Wait for 0.2s for safety.
            Timer::sleep(0.15).await;
            let event = client1.try_recv().unwrap();
            assert_eq!(event.origin(), streamer_id);
        }

        // The other client shouldn't have received any events.
        assert!(client2.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_reactive_streamer() {
        let manager = StreamingManager::default();
        let system = AbortableQueue::default();
        let (client_id1, client_id2) = (1, 2);
        // Register a new client with the manager.
        let mut client1 = manager.new_client(client_id1).unwrap();
        // Another client whom we won't have it subscribe to the streamer.
        let mut client2 = manager.new_client(client_id2).unwrap();
        // Subscribe the new client to ReactiveStreamer.
        let streamer_id = manager
            .add(client_id1, ReactiveStreamer, system.weak_spawner())
            .await
            .unwrap();

        // We should be hooked now. try to receive some events from the streamer.
        for i in 1..=3 {
            let msg = format!("send{}", i);
            manager.send(&streamer_id, msg.clone()).unwrap();
            // Wait for a little bit to make sure the streamer received the data we sent.
            Timer::sleep(0.1).await;
            // The streamer should broadcast some event to the subscribed clients.
            let event = client1.try_recv().unwrap();
            assert_eq!(event.origin(), streamer_id);
            // It's an echo streamer, so the message should be the same.
            assert_eq!(event.get().1, json!(msg));
        }

        // If we send the wrong datatype (void here instead of String), the streamer should ignore it.
        manager.send(&streamer_id, ()).unwrap();
        Timer::sleep(0.1).await;
        assert!(client1.try_recv().is_err());

        // The other client shouldn't have received any events.
        assert!(client2.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_erroring_streamer() {
        let manager = StreamingManager::default();
        let system = AbortableQueue::default();
        let client_id = 1;
        // Register a new client with the manager.
        let _client = manager.new_client(client_id).unwrap();
        // Subscribe the new client to InitErrorStreamer.
        let error = manager
            .add(client_id, InitErrorStreamer, system.weak_spawner())
            .await
            .unwrap_err();

        assert!(matches!(error, StreamingManagerError::SpawnError(..)));
    }

    #[tokio::test]
    async fn test_remove_streamer_if_down() {
        let manager = StreamingManager::default();
        let system = AbortableQueue::default();
        let client_id = 1;
        // Register a new client with the manager.
        let _client = manager.new_client(client_id).unwrap();
        // Subscribe the new client to PeriodicStreamer.
        let streamer_id = manager
            .add(client_id, PeriodicStreamer, system.weak_spawner())
            .await
            .unwrap();

        // The streamer is up and streaming to `client_id`.
        assert!(manager
            .streamers
            .read()
            .get(&streamer_id)
            .unwrap()
            .clients
            .contains(&client_id));

        // The client should be registered and listening to `streamer_id`.
        assert!(manager
            .clients
            .lock()
            .unwrap()
            .get(&client_id)
            .unwrap()
            .contains(&streamer_id));

        // Abort the system to kill the streamer.
        system.abort_all().unwrap();
        // Wait a little bit since the abortion doesn't take effect immediately (the aborted task needs to yield first).
        Timer::sleep(0.1).await;

        manager.remove_streamer_if_down(&streamer_id);

        // The streamer should be removed.
        assert!(manager.streamers.read().get(&streamer_id).is_none());
        // And the client is no more listening to it.
        assert!(!manager
            .clients
            .lock()
            .unwrap()
            .get(&client_id)
            .unwrap()
            .contains(&streamer_id));
    }
}
