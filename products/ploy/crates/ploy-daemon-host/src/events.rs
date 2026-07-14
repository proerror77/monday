use ploy_operator_contracts::OperatorEvent;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct EventBroker {
    subscribers: Mutex<Vec<Sender<OperatorEvent>>>,
}

impl EventBroker {
    pub fn subscribe(&self) -> Receiver<OperatorEvent> {
        let (sender, receiver) = mpsc::channel();
        self.subscribers
            .lock()
            .expect("event broker lock")
            .push(sender);
        receiver
    }

    pub fn publish(&self, event: OperatorEvent) {
        let mut subscribers = self.subscribers.lock().expect("event broker lock");
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }
}
