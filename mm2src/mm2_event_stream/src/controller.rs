use parking_lot::Mutex;
use std::{collections::{HashMap, HashSet},
          sync::Arc};
use tokio::sync::mpsc::{self, Receiver, Sender};

type ChannelId = u64;

/// Root controller of streaming channels
pub struct Controller<M>(Arc<Mutex<ChannelsInner<M>>>);

impl<M> Clone for Controller<M> {
    fn clone(&self) -> Self { Self(Arc::clone(&self.0)) }
}

/// Inner part of the controller
pub struct ChannelsInner<M> {
    last_id: u64,
    channels: HashMap<ChannelId, Sender<Arc<M>>>,
}

/// A smart channel receiver that will run a hook when it's dropped to remove itself from the controller.
pub struct ChannelReceiver<M: Send + Sync + 'static> {
    /// The receiver end of the channel.
    rx: Receiver<Arc<M>>,
    /// A hook that's ran when the receiver channel receiver is dropped.
    channel_drop_hook: Box<dyn Fn() + Send>,
}

impl<M: Send + Sync> Controller<M> {
    /// Creates a new channels controller
    pub fn new() -> Self { Default::default() }

    /// Creates a new channel and returns it's events receiver
    pub fn create_channel(&self, concurrency: usize) -> ChannelReceiver<M> {
        let (tx, rx) = mpsc::channel::<Arc<M>>(concurrency);

        let mut inner = self.0.lock();
        let channel_id = inner.last_id.overflowing_add(1).0;
        inner.channels.insert(channel_id, tx);
        inner.last_id = channel_id;

        let channel_drop_hook = {
            let controller = self.clone();
            // Remove the channel from the controller when the receiver is dropped.
            Box::new(move || {
                common::log::debug!("Dropping event receiver");
                controller.remove_channel(&channel_id)
            })
        };
        ChannelReceiver { rx, channel_drop_hook }
    }

    /// Returns number of active channels
    pub fn num_connections(&self) -> usize { self.0.lock().channels.len() }

    /// Broadcast message to all channels
    pub async fn broadcast(&self, message: M, client_ids: Option<Arc<HashSet<u64>>>) {
        let msg = Arc::new(message);
        for tx in self.channels(client_ids) {
            // Only `try_send` here. If the receiver's channel is full (receiver is slow), it will
            // not receive the message. This avoids blocking the broadcast to other receivers.
            tx.try_send(msg.clone()).ok();
        }
    }

    /// Removes the channel from the controller
    fn remove_channel(&self, channel_id: &ChannelId) {
        let mut inner = self.0.lock();
        inner.channels.remove(channel_id);
    }

    /// Returns the channels that are associated with the specified client ids.
    ///
    /// If no client ids are specified, all the channels are returned.
    fn channels(&self, client_ids: Option<Arc<HashSet<u64>>>) -> Vec<Sender<Arc<M>>> {
        if let Some(client_ids) = client_ids {
            self.0
                .lock()
                .channels
                .iter()
                .filter_map(|(id, sender)| client_ids.contains(value).then(sender.clone()));
        } else {
            // Returns all the channels if no client ids where specifically requested.
            self.0.lock().channels.values().cloned().collect()
        }
    }
}

impl<M> Default for Controller<M> {
    fn default() -> Self {
        let inner = ChannelsInner {
            last_id: 0,
            channels: HashMap::new(),
        };
        Self(Arc::new(Mutex::new(inner)))
    }
}

impl<M: Send + Sync> ChannelReceiver<M> {
    /// Receives the next event from the channel
    pub async fn recv(&mut self) -> Option<Arc<M>> { self.rx.recv().await }
}

impl<M: Send + Sync> Drop for ChannelReceiver<M> {
    fn drop(&mut self) { (self.channel_drop_hook)(); }
}

#[cfg(any(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use common::cross_test;

    common::cfg_wasm32! {
        use wasm_bindgen_test::*;
        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    }

    cross_test!(test_create_channel_and_broadcast, {
        let controller = Controller::new();
        let mut channel_receiver = controller.create_channel(1);

        controller.broadcast("Message".to_string()).await;

        let received_msg = channel_receiver.recv().await.unwrap();
        assert_eq!(*received_msg, "Message".to_string());
    });

    cross_test!(test_multiple_channels_and_broadcast, {
        let controller = Controller::new();

        let mut receivers = Vec::new();
        for _ in 0..3 {
            receivers.push(controller.create_channel(1));
        }

        controller.broadcast("Message".to_string()).await;

        for receiver in &mut receivers {
            let received_msg = receiver.recv().await.unwrap();
            assert_eq!(*received_msg, "Message".to_string());
        }
    });

    cross_test!(test_channel_cleanup_on_drop, {
        let controller: Controller<()> = Controller::new();
        let channel_receiver = controller.create_channel(1);

        assert_eq!(controller.num_connections(), 1);

        drop(channel_receiver);

        assert_eq!(controller.num_connections(), 0);
    });

    cross_test!(test_broadcast_across_channels, {
        let controller = Controller::new();

        let mut receivers = Vec::new();
        for _ in 0..3 {
            receivers.push(controller.create_channel(1));
        }

        controller.broadcast("Message".to_string()).await;

        for receiver in &mut receivers {
            let received_msg = receiver.recv().await.unwrap();
            assert_eq!(*received_msg, "Message".to_string());
        }
    });

    cross_test!(test_multiple_messages_and_drop, {
        let controller = Controller::new();
        let mut channel_receiver = controller.create_channel(6);

        controller.broadcast("Message 1".to_string()).await;
        controller.broadcast("Message 2".to_string()).await;
        controller.broadcast("Message 3".to_string()).await;
        controller.broadcast("Message 4".to_string()).await;
        controller.broadcast("Message 5".to_string()).await;
        controller.broadcast("Message 6".to_string()).await;

        let mut received_msgs = Vec::new();
        for _ in 0..6 {
            let received_msg = channel_receiver.recv().await.unwrap();
            received_msgs.push(received_msg);
        }

        assert_eq!(*received_msgs[0], "Message 1".to_string());
        assert_eq!(*received_msgs[1], "Message 2".to_string());
        assert_eq!(*received_msgs[2], "Message 3".to_string());
        assert_eq!(*received_msgs[3], "Message 4".to_string());
        assert_eq!(*received_msgs[4], "Message 5".to_string());
        assert_eq!(*received_msgs[5], "Message 6".to_string());

        // Drop the channel receiver to trigger the drop hook (remove itself from the controller).
        drop(channel_receiver);

        assert_eq!(controller.num_connections(), 0);
    });
}
