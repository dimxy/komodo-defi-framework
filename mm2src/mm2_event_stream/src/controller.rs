use std::{collections::{HashMap, HashSet},
          sync::Arc};
use tokio::sync::mpsc::{self, Receiver, Sender};

#[derive(Clone, Default)]
/// Root controller of streaming channels
pub struct Controller<M> {
    channels: HashMap<u64, Sender<Arc<M>>>,
}

impl<M: Send + Sync> Controller<M> {
    /// Creates a new channel and returns it's events receiver
    pub(crate) fn create_channel(&mut self, channel_id: u64, concurrency: usize) -> Receiver<Arc<M>> {
        let (tx, rx) = mpsc::channel(concurrency);
        self.channels.insert(channel_id, tx);
        rx
    }

    /// Removes the channel from the controller
    pub(crate) fn remove_channel(&mut self, channel_id: &u64) { self.channels.remove(channel_id); }

    /// Broadcast message to all channels
    pub(crate) fn broadcast(&self, message: M, client_ids: Option<&HashSet<u64>>) {
        let msg = Arc::new(message);
        for tx in self.channels(client_ids) {
            // Only `try_send` here. If the receiver's channel is full (receiver is slow), it will
            // not receive the message. This avoids blocking the broadcast to other receivers.
            tx.try_send(msg.clone()).ok();
        }
    }

    /// Returns the channels that are associated with the specified client ids.
    ///
    /// If no client ids are specified, all the channels are returned.
    fn channels(&self, client_ids: Option<&HashSet<u64>>) -> Vec<Sender<Arc<M>>> {
        if let Some(client_ids) = client_ids {
            self.channels
                .iter()
                .filter_map(|(id, sender)| client_ids.contains(id).then_some(sender))
                .cloned()
                .collect()
        } else {
            // Returns all the channels if no client ids where specifically requested.
            self.channels.values().cloned().collect()
        }
    }
}

#[cfg(any(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use common::cross_test;
    use tokio::sync::mpsc::error::TryRecvError;

    common::cfg_wasm32! {
        use wasm_bindgen_test::*;
        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
    }

    cross_test!(test_create_channel_and_broadcast, {
        let mut controller = Controller::default();
        let mut channel_receiver = controller.create_channel(0, 1);

        controller.broadcast("Message".to_string(), None);

        let received_msg = channel_receiver.recv().await.unwrap();
        assert_eq!(*received_msg, "Message".to_string());
    });

    cross_test!(test_multiple_channels_and_broadcast, {
        let mut controller = Controller::default();

        let mut receivers = Vec::new();
        for channel_id in 0..3 {
            receivers.push(controller.create_channel(channel_id, 1));
        }

        controller.broadcast("Message".to_string(), None);

        for receiver in &mut receivers {
            let received_msg = receiver.recv().await.unwrap();
            assert_eq!(*received_msg, "Message".to_string());
        }
    });

    cross_test!(test_broadcast_across_channels, {
        let mut controller = Controller::default();

        let mut receivers = Vec::new();
        for channel_id in 0..3 {
            receivers.push(controller.create_channel(channel_id, 1));
        }

        controller.broadcast("Message".to_string(), None);

        for receiver in &mut receivers {
            let received_msg = receiver.recv().await.unwrap();
            assert_eq!(*received_msg, "Message".to_string());
        }
    });

    cross_test!(test_msgs_drop_past_capacity, {
        let mut controller = Controller::default();
        let mut channel_receiver = controller.create_channel(0, 6);

        controller.broadcast("Message 1".to_string(), None);
        controller.broadcast("Message 2".to_string(), None);
        controller.broadcast("Message 3".to_string(), None);
        controller.broadcast("Message 4".to_string(), None);
        controller.broadcast("Message 5".to_string(), None);
        controller.broadcast("Message 6".to_string(), None);
        // This message should be dropped.
        controller.broadcast("Message 7".to_string(), None);

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

        assert!(matches!(channel_receiver.try_recv(), Err(TryRecvError::Empty)));
    });
}
