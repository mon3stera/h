use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Clone)]
pub struct EventBus<T> {
    subscribers: Arc<Mutex<Vec<UnboundedSender<T>>>>,
}

impl<T> EventBus<T> {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn subscribe(&self) -> UnboundedReceiver<T> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let mut guard = self.subscribers.lock();

        guard.push(tx);

        rx
    }

    pub fn broadcast(&self, event: T)
    where
        T: Clone,
    {
        let guard = self.subscribers.lock();

        for subscriber in guard.iter() {
            subscriber
                .send(event.clone())
                .expect("Failed to broadcast event");
        }
    }
}
