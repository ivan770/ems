use std::{convert::TryInto, io::ErrorKind, sync::Arc, time::Duration};

use audiosocket::Message;
use flume::{
    r#async::{RecvStream, SendSink},
    unbounded, Receiver,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    try_join,
};
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::{
    db::HandlerDatabase,
    recognition::{DispatchedSpeechRecognition, RecognitionDriver, SpeechRecognition},
    server::ServerError,
    stream::MessageStream,
};

/// Message handler, that doesn't have any unique identifier.
///
/// Ignores all messages, until upgraded to [`IdentifiableMessageHandler`].
pub struct AnonymousMessageHandler<'s, ST, SI, D> {
    database: Arc<HandlerDatabase>,
    stream: MessageStream<'s, ST>,
    sink: &'s mut SI,
    recognition_driver: Option<D>,
}

impl<'s, ST, SI, D> AnonymousMessageHandler<'s, ST, SI, D>
where
    ST: AsyncRead + Unpin,
    SI: AsyncWrite + Unpin,
    D: RecognitionDriver<
            RecvStream<'static, Vec<u8>>,
            Item = Result<
                Option<String>,
                <D as RecognitionDriver<RecvStream<'static, Vec<u8>>>>::Error,
            >,
        > + 'static,
{
    pub fn new(
        database: Arc<HandlerDatabase>,
        stream: MessageStream<'s, ST>,
        sink: &'s mut SI,
        recognition_driver: Option<D>,
    ) -> Self {
        AnonymousMessageHandler {
            database,
            recognition_driver,
            stream,
            sink,
        }
    }

    /// Listen for incoming events from [`MessageStream`].
    ///
    /// Just as in [`IdentifiableMessageHandler`], `listen` is an event loop,
    /// though here it listens for `Message::Identifier` only, and then upgrades
    /// [`AnonymousMessageHandler`] to [`IdentifiableMessageHandler`].
    #[instrument(skip(self, max_time), err, name = "anon_listen")]
    pub async fn listen(mut self, max_time: Duration) -> Result<(), ServerError> {
        loop {
            match self.stream.recv(max_time).await? {
                Message::Identifier(id) => {
                    debug!("Upgrading message handler from anonymous to identifiable");
                    return self.upgrade(id).listen(max_time).await;
                }
                _ => warn!("Received non-identifier message on anonymous message handler"),
            }
        }
    }

    /// Upgrade [`AnonymousMessageHandler`] to [`IdentifiableMessageHandler`]
    fn upgrade(self, id: Uuid) -> IdentifiableMessageHandler<'s, ST, SI, D> {
        IdentifiableMessageHandler::new(
            id,
            self.stream,
            self.database,
            self.sink,
            self.recognition_driver,
        )
    }
}

/// Actions, that can be sent to [`IdentifiableMessageHandler`], including call hangup and audio playback.
pub enum MessageHandlerAction {
    /// Terminate connection.
    Hangup,

    /// Play audio on channel.
    Play(Vec<u8>),
}

/// Message handler, that has a unique identifier attached.
///
/// Listens for all messages, and drives connection event loop.
pub struct IdentifiableMessageHandler<'s, ST, SI, D> {
    id: Uuid,
    channel: Receiver<MessageHandlerAction>,
    database: Arc<HandlerDatabase>,
    stream: MessageStream<'s, ST>,
    sink: &'s mut SI,
    recognition_driver: Option<D>,
}

impl<'s, ST, SI, D> IdentifiableMessageHandler<'s, ST, SI, D>
where
    ST: AsyncRead + Unpin,
    SI: AsyncWrite + Unpin + 's,
    D: RecognitionDriver<
            RecvStream<'static, Vec<u8>>,
            Item = Result<
                Option<String>,
                <D as RecognitionDriver<RecvStream<'static, Vec<u8>>>>::Error,
            >,
        > + 'static,
{
    fn new(
        id: Uuid,
        stream: MessageStream<'s, ST>,
        database: Arc<HandlerDatabase>,
        sink: &'s mut SI,
        recognition_driver: Option<D>,
    ) -> Self {
        let (sender, receiver) = unbounded();

        database.add_handler(id, sender);

        IdentifiableMessageHandler {
            id,
            channel: receiver,
            database,
            stream,
            sink,
            recognition_driver,
        }
    }

    /// Listen for incoming messages from [`MessageStream`], and execute [`MessageHandlerAction`] events.
    #[instrument(skip(self), err, name = "id_listen", fields(id = %self.id))]
    pub async fn listen(mut self, max_time: Duration) -> Result<(), ServerError> {
        let result = try_join!(
            Self::handle_messages(
                self.id,
                &self.database,
                &mut self.stream,
                &mut self.recognition_driver,
                max_time
            ),
            Self::action_listen(&self.channel, self.sink)
        );

        match result {
            Ok(_) | Err(ServerError::ClientDisconnected(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Listen for incoming AudioSocket messages.
    async fn handle_messages(
        id: Uuid,
        database: &Arc<HandlerDatabase>,
        stream: &mut MessageStream<'s, ST>,
        recognition_driver: &mut Option<D>,
        max_time: Duration,
    ) -> Result<(), ServerError> {
        // `take` here is actually not necessary, but is used to silence compiler because of Drop impl.
        let mut recognition_driver = recognition_driver
            .take()
            .map(|driver| Self::prepare_audio_sender(id, database.clone(), driver));

        loop {
            match (stream.recv(max_time).await, recognition_driver.as_mut()) {
                (Ok(Message::Identifier(_)), _) => {
                    warn!("Received identifier message on identified message handler")
                }
                (Ok(Message::Audio(Some(audio))), Some(dispatched)) => {
                    dispatched.send(audio.to_vec()).await;
                }
                (Ok(Message::Terminate), _) => {
                    debug!("Obtained termination message");
                    break;
                }
                (Err(ServerError::IoError(e)), _)
                    if e.kind() == ErrorKind::BrokenPipe
                        || e.kind() == ErrorKind::UnexpectedEof
                        || e.kind() == ErrorKind::ConnectionReset =>
                {
                    return Err(ServerError::ClientDisconnected(e));
                }
                (Err(e), _) => return Err(e),
                _ => {}
            }
        }

        Ok(())
    }

    /// Listen for incoming actions from channel, and send call events (hangup, audio playback, etc.) to sink.
    async fn action_listen(
        channel: &Receiver<MessageHandlerAction>,
        sink: &mut SI,
    ) -> Result<(), ServerError> {
        while let Ok(event) = channel.recv_async().await {
            match event {
                MessageHandlerAction::Hangup => {
                    sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Terminate)?)
                        .await?;
                }
                MessageHandlerAction::Play(audio) => {
                    sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Audio(Some(&audio)))?)
                        .await?;
                }
            }
        }

        Ok(())
    }

    fn prepare_audio_sender(
        id: Uuid,
        database: Arc<HandlerDatabase>,
        recognition_driver: D,
    ) -> DispatchedSpeechRecognition<SendSink<'static, Vec<u8>>> {
        let (sender, receiver) = unbounded();

        SpeechRecognition::new(
            id,
            database,
            sender.into_sink(),
            receiver.into_stream(),
            recognition_driver,
        )
        .spawn()
    }
}

// TODO: Check if we are closing connection gracefully, as debug logs
// don't show GoAway packages when shutting down server.
impl<'s, ST, SI, D> Drop for IdentifiableMessageHandler<'s, ST, SI, D> {
    fn drop(&mut self) {
        self.database.remove_handler(self.id);
    }
}
