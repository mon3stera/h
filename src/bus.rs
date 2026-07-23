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
        tracing::debug!(
            event = "event_bus.subscriber_added",
            subscriber_count = guard.len()
        );

        rx
    }

    pub fn broadcast(&self, event: T)
    where
        T: Clone,
    {
        let mut guard = self.subscribers.lock();
        let before = guard.len();

        guard.retain(|subscriber| subscriber.send(event.clone()).is_ok());

        let dropped = before - guard.len();
        if dropped > 0 {
            tracing::warn!(
                event = "event_bus.subscribers_pruned",
                dropped_subscriber_count = dropped,
                subscriber_count = guard.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EventBus;

    #[test]
    fn dropped_subscribers_are_pruned_without_panicking() {
        let bus = EventBus::new();
        let receiver = bus.subscribe();
        drop(receiver);

        bus.broadcast(1_u8);

        assert!(bus.subscribers.lock().is_empty());
    }
}
