use dashmap::DashMap;
use deadqueue::unlimited;
use flume::Sender;
use unlimited::Queue;
use uuid::Uuid;

use crate::handler::MessageHandlerAction;

/// Message handler database.
///
/// Can be used to send various events to message handlers (call hangup, audio playback, etc.)
#[derive(Default)]
pub struct HandlerDatabase {
    transcription: Queue<(Uuid, String)>,
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

    /// Add new recognized speech transcription.
    pub fn add_transcription(&self, id: Uuid, transcription: String) {
        self.transcription.push((id, transcription));
    }

    /// Receive new recognized speech transcription.
    ///
    /// This method will yield to executor if there are no transcriptions in queue.
    pub async fn recv_transcription(&self) -> (Uuid, String) {
        self.transcription.pop().await
    }

    /// Send an action to a message handler.
    pub fn send(&self, id: &Uuid, action: MessageHandlerAction) -> Option<()> {
        self.handler_map.get(id)?.send(action).ok()
    }
}
