use std::{convert::TryInto, io::ErrorKind, sync::Arc, time::Duration};

use audiosocket::Message;
use flume::{unbounded, Receiver};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufWriter},
    select,
};
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::{
    config::Config,
    db::HandlerDatabase,
    recognition::{SpeechRecognitionConfig, SpeechRecognitionRequest},
    server::ServerError,
    service::{spawn_speech_recognition, SpawnedSpeechRecognition},
    stream::MessageStream,
};

/// Inner [`BufWriter`] capacity for writing data back to AudioSocket client.
const AUDIO_BUF_CAPACITY: usize = 64 * 1024;

/// Exact chunk size to be used when sending or receiving audio using AudioSocket.
pub const CHUNK_SIZE: usize = 320;

/// Message handler, that doesn't have any unique identifier.
///
/// Ignores all messages, until upgraded to [`IdentifiableMessageHandler`].
pub struct AnonymousMessageHandler<'c, 's, ST, SI> {
    config: &'c Config,
    database: Arc<HandlerDatabase>,
    stream: MessageStream<'s, ST>,
    sink: &'s mut SI,
}

impl<'s, ST, SI> AnonymousMessageHandler<'static, 's, ST, SI>
where
    ST: AsyncRead + Unpin,
    SI: AsyncWrite + Unpin,
{
    pub fn new(
        config: &'static Config,
        database: Arc<HandlerDatabase>,
        stream: MessageStream<'s, ST>,
        sink: &'s mut SI,
    ) -> Self {
        AnonymousMessageHandler {
            config,
            database,
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
    fn upgrade(self, id: Uuid) -> IdentifiableMessageHandler<'static, 's, ST, SI> {
        IdentifiableMessageHandler::new(id, self.config, self.stream, self.database, self.sink)
    }
}

/// Actions, that can be sent to [`IdentifiableMessageHandler`], including call hangup and audio playback.
#[cfg_attr(test, derive(Debug))]
pub enum MessageHandlerAction {
    /// Terminate connection.
    Hangup,

    /// Play audio on channel.
    Play(Vec<u8>),
}

/// Message handler, that has a unique identifier attached.
///
/// Listens for all messages, and drives connection event loop.
pub struct IdentifiableMessageHandler<'c, 's, ST, SI> {
    id: Uuid,
    config: &'c Config,
    channel: Receiver<MessageHandlerAction>,
    database: Arc<HandlerDatabase>,
    stream: MessageStream<'s, ST>,
    sink: &'s mut SI,
}

impl<'s, ST, SI> IdentifiableMessageHandler<'static, 's, ST, SI>
where
    ST: AsyncRead + Unpin,
    SI: AsyncWrite + Unpin + 's,
{
    fn new(
        id: Uuid,
        config: &'static Config,
        stream: MessageStream<'s, ST>,
        database: Arc<HandlerDatabase>,
        sink: &'s mut SI,
    ) -> Self {
        let (sender, receiver) = unbounded();

        database.add_handler(id, sender);

        IdentifiableMessageHandler {
            id,
            config,
            channel: receiver,
            database,
            stream,
            sink,
        }
    }

    /// Listen for incoming messages from [`MessageStream`], and execute [`MessageHandlerAction`] events.
    #[instrument(skip(self), err, name = "id_listen", fields(id = %self.id))]
    pub async fn listen(mut self, max_time: Duration) -> Result<(), ServerError> {
        let result = select! {
            res = Self::handle_messages(
                self.id,
                self.config,
                &self.database,
                &mut self.stream,
                max_time
            ) => Err(res),
            res = Self::action_listen(&self.channel, self.sink) => res,
        };

        match result {
            Ok(_) | Err(ServerError::ClientDisconnected(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Listen for incoming AudioSocket messages.
    async fn handle_messages<'a>(
        id: Uuid,
        config: &'static Config,
        database: &'a Arc<HandlerDatabase>,
        stream: &'a mut MessageStream<'s, ST>,
        max_time: Duration,
    ) -> ServerError {
        let recognition = prepare_recognition_service(id, config, database.clone());

        loop {
            match (stream.recv(max_time).await, recognition.as_ref()) {
                (Ok(Message::Identifier(_)), _) => {
                    warn!("Received identifier message on identified message handler")
                }
                (Ok(Message::Audio(Some(audio))), Some(dispatched)) => {
                    dispatched.send(SpeechRecognitionRequest {
                        audio: audio.to_vec(),
                    });
                }
                (Ok(Message::Terminate), _) => {
                    debug!("Obtained termination message");
                    return ServerError::ClientDisconnected(None);
                }
                (Err(ServerError::IoError(e)), _)
                    if e.kind() == ErrorKind::BrokenPipe
                        || e.kind() == ErrorKind::UnexpectedEof
                        || e.kind() == ErrorKind::ConnectionReset =>
                {
                    return ServerError::ClientDisconnected(Some(e));
                }
                (Err(e), _) => return e,
                _ => {}
            }
        }
    }

    /// Listen for incoming actions from channel, and send call events (hangup, audio playback, etc.) to sink.
    async fn action_listen(
        channel: &Receiver<MessageHandlerAction>,
        sink: &mut SI,
    ) -> Result<(), ServerError> {
        let mut sink = BufWriter::with_capacity(AUDIO_BUF_CAPACITY, sink);

        while let Ok(event) = channel.recv_async().await {
            match event {
                MessageHandlerAction::Hangup => {
                    sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Terminate)?)
                        .await?;
                }
                MessageHandlerAction::Play(audio) => {
                    let chunks = audio.chunks_exact(CHUNK_SIZE);
                    let remainder = chunks.remainder();

                    for chunk in chunks {
                        sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Audio(Some(chunk)))?)
                            .await?;
                    }

                    let mut remainder = remainder.to_owned();
                    remainder.resize(CHUNK_SIZE, 0);

                    sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Audio(Some(
                        &remainder,
                    )))?)
                    .await?;
                }
            }

            sink.flush().await?;
        }

        Err(ServerError::ClientDisconnected(None))
    }
}

// TODO: Check if we are closing connection gracefully, as debug logs
// don't show GoAway packages when shutting down server.
impl<'c, 's, ST, SI> Drop for IdentifiableMessageHandler<'c, 's, ST, SI> {
    fn drop(&mut self) {
        self.database.remove_handler(self.id);
    }
}

fn prepare_recognition_service(
    id: Uuid,
    config: &'static Config,
    database: Arc<HandlerDatabase>,
) -> Option<SpawnedSpeechRecognition> {
    let recognition_config = SpeechRecognitionConfig {
        application_config: config,
        language: String::from("ru-RU"),
        profanity_filter: false,
        punctuation: false,
    };

    spawn_speech_recognition(id, recognition_config, database)
}

#[cfg(test)]
mod tests {
    use std::{convert::TryInto, sync::Arc, time::Duration};

    use audiosocket::Message;
    use tokio::{
        io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream},
        spawn,
        task::JoinHandle,
        time::sleep,
    };
    use tracing_test::traced_test;
    use uuid::Uuid;

    use crate::{
        config::TEST_CONFIG,
        db::HandlerDatabase,
        handler::{AnonymousMessageHandler, MessageHandlerAction},
        server::ServerError,
        stream::MessageStream,
    };

    fn prepare_handler() -> (
        Arc<HandlerDatabase>,
        JoinHandle<Result<(), ServerError>>,
        DuplexStream,
        DuplexStream,
    ) {
        let database = Arc::new(HandlerDatabase::default());

        let (as_sender, mut as_receiver) = duplex(128);
        let (mut data_sender, data_receiver) = duplex(128);

        let db_clone = database.clone();
        let handle = spawn(async move {
            let handler = AnonymousMessageHandler::new(
                &*TEST_CONFIG,
                db_clone,
                MessageStream::new(&mut as_receiver),
                &mut data_sender,
            );

            handler.listen(Duration::from_secs(5)).await
        });

        (database, handle, as_sender, data_receiver)
    }

    #[tokio::test]
    #[traced_test]
    async fn upgrades_on_identifier() {
        let (_, _, mut sender, _) = prepare_handler();

        sender
            .write_all(&TryInto::<Vec<u8>>::try_into(Message::Identifier(Uuid::nil())).unwrap())
            .await
            .unwrap();
        sender
            .write_all(&TryInto::<Vec<u8>>::try_into(Message::Identifier(Uuid::nil())).unwrap())
            .await
            .unwrap();

        sleep(Duration::from_secs(1)).await;

        assert!(logs_contain(
            "Received identifier message on identified message handler"
        ));
    }

    #[tokio::test]
    async fn terminates() {
        let (_, handle, mut sender, _) = prepare_handler();

        sender
            .write_all(&TryInto::<Vec<u8>>::try_into(Message::Identifier(Uuid::nil())).unwrap())
            .await
            .unwrap();
        sender
            .write_all(&TryInto::<Vec<u8>>::try_into(Message::Terminate).unwrap())
            .await
            .unwrap();

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn hangups() {
        let (database, _, mut sender, mut receiver) = prepare_handler();

        sender
            .write_all(&TryInto::<Vec<u8>>::try_into(Message::Identifier(Uuid::nil())).unwrap())
            .await
            .unwrap();

        // Wait for an upgrade and other stuff
        sleep(Duration::from_secs(1)).await;

        database.send(&Uuid::nil(), MessageHandlerAction::Hangup);

        // Wait for handler to send hangup back
        sleep(Duration::from_secs(1)).await;

        let mut buf = [0; 3];

        receiver.read_exact(&mut buf).await.unwrap();

        assert_eq!(buf, [0, 0, 0]);
    }

    #[tokio::test]
    async fn transmits_audio() {
        let (database, _, mut sender, mut receiver) = prepare_handler();

        sender
            .write_all(&TryInto::<Vec<u8>>::try_into(Message::Identifier(Uuid::nil())).unwrap())
            .await
            .unwrap();

        // Wait for an upgrade and other stuff
        sleep(Duration::from_secs(1)).await;

        database.send(
            &Uuid::nil(),
            MessageHandlerAction::Play(vec![1, 2, 3, 4, 5]),
        );

        database.send(
            &Uuid::nil(),
            MessageHandlerAction::Play(Vec::from([1; 320])),
        );

        // Wait for handler to send audio back
        sleep(Duration::from_secs(1)).await;

        let mut buf = [0; 323];

        receiver.read_exact(&mut buf).await.unwrap();

        assert_eq!(
            buf,
            [
                16, 1, 64, 1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        receiver.read_exact(&mut buf).await.unwrap();

        assert_eq!(
            buf,
            [
                16, 1, 64, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1
            ]
        );
    }
}
