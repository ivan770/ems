use dashmap::DashMap;
use deadqueue::unlimited;
use flume::Sender;
use unlimited::Queue;
use uuid::Uuid;

use crate::{handler::MessageHandlerAction, ws::WsNotification};

/// Message handler database.
///
/// Can be used to send various events to message handlers (call hangup, audio playback, etc.)
#[derive(Default)]
pub struct HandlerDatabase {
    ws_notifications: Queue<WsNotification>,
    // TODO: Check if it's possible to use Queue here to avoid double-Arc.
    handler_map: DashMap<Uuid, Sender<MessageHandlerAction>>,
}

impl HandlerDatabase {
    /// Add new message handler to database.
    pub fn add_handler(&self, id: Uuid, sender: Sender<MessageHandlerAction>) {
        self.handler_map.insert(id, sender);
    }

    /// Remove handler from database.
    ///
    /// This should be called when call was terminated.
    pub fn remove_handler(&self, id: Uuid) {
        self.handler_map.remove(&id);
    }

    /// Add new WebSocket notification.
    pub fn add_notification(&self, notification: WsNotification) {
        self.ws_notifications.push(notification);
    }

    /// Receive new WebSocket notification.
    ///
    /// This method will yield to executor if there are no notifications in queue.
    pub async fn recv_notification(&self) -> WsNotification {
        self.ws_notifications.pop().await
    }

    /// Send an action to a message handler.
    pub fn send(&self, id: &Uuid, action: MessageHandlerAction) -> Option<()> {
        self.handler_map.get(id)?.send(action).ok()
    }

    /// Check if there are no active handlers in database.
    pub fn is_empty(&self) -> bool {
        self.handler_map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use flume::unbounded;
    use uuid::Uuid;

    use super::HandlerDatabase;
    use crate::{handler::MessageHandlerAction, ws::WsNotification};

    const TEST_ID: Uuid = Uuid::nil();

    #[test]
    fn test_handler_actions() {
        let database = HandlerDatabase::default();

        let (sender, receiver) = unbounded();

        database.add_handler(TEST_ID, sender);
        database.send(&TEST_ID, MessageHandlerAction::Hangup);
        assert!(matches!(
            receiver.recv().unwrap(),
            MessageHandlerAction::Hangup
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_transcription() {
        let database = HandlerDatabase::default();

        database.add_notification(WsNotification::Transcription(TEST_ID, String::from("test")));

        assert!(
            matches!(database.recv_notification().await, WsNotification::Transcription(id, message) if id == TEST_ID && message == String::from("test"))
        );
    }
}
