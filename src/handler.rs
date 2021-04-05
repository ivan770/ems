use std::{convert::TryInto, io::ErrorKind, sync::Arc, time::Duration};

use audiosocket::Message;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufWriter},
    select,
    sync::oneshot,
    time::{sleep, timeout},
};
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::{
    config::Config,
    db::HandlerDatabase,
    recognition::{
        SpeechRecognitionConfig, SpeechRecognitionRequest, SpeechRecognitionServiceConfig,
    },
    server::ServerError,
    service::{spawn_speech_recognition, SpawnedSpeechRecognition},
    stream::MessageStream,
    ws::WsNotification,
};

/// Exact chunk size to be used when sending or receiving audio using AudioSocket.
pub const CHUNK_SIZE: usize = 320;

/// Duration between each AudioSocket audio response tick.
///
/// See <https://github.com/CyCoreSystems/audiosocket/blob/807c189aae2b651b779b1e2ce1a17e1ecd364973/chunk.go#L23> for more details.
const TICK_DURATION: Duration = Duration::from_millis(20);

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

    /// Provide speech recognition config to handler
    RecognitionConfig(SpeechRecognitionConfig),
}

/// Message handler, that has a unique identifier attached.
///
/// Listens for all messages, and drives connection event loop.
pub struct IdentifiableMessageHandler<'c, 's, ST, SI> {
    id: Uuid,
    config: &'c Config,
    channel: flume::Receiver<MessageHandlerAction>,
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
        let (sender, receiver) = flume::unbounded();

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
        self.database
            .add_notification(WsNotification::RecognitionConfigRequest(self.id));

        let (recognition_sender, recognition_receiver) = oneshot::channel();

        let result = select! {
            res = Self::handle_messages(
                self.id,
                self.config,
                &self.database,
                recognition_receiver,
                &mut self.stream,
                max_time
            ) => Err(res),
            res = Self::action_listen(&self.channel, recognition_sender, self.sink) => res,
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
        recognition_receiver: oneshot::Receiver<SpeechRecognitionConfig>,
        stream: &'a mut MessageStream<'s, ST>,
        max_time: Duration,
    ) -> ServerError {
        let recognition =
            prepare_recognition_service(id, config, database.clone(), recognition_receiver).await;

        loop {
            match (stream.recv(max_time).await, recognition.as_ref()) {
                (Ok(Message::Identifier(_)), _) => {
                    warn!("Received identifier message on identified message handler")
                }
                (Ok(Message::Audio(Some(audio))), Some(dispatched)) => {
                    if config.loopback_audio() {
                        database.send(&id, MessageHandlerAction::Play(audio.to_owned()));
                    }

                    dispatched.send(SpeechRecognitionRequest {
                        audio: audio.to_owned(),
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
        channel: &flume::Receiver<MessageHandlerAction>,
        recognition_sender: oneshot::Sender<SpeechRecognitionConfig>,
        sink: &mut SI,
    ) -> Result<(), ServerError> {
        // We use Option here as a cell to silence compiler because of taking oneshot::Sender by value in send method.
        let mut recognition_sender = Some(recognition_sender);
        // FIXME: Do we really need this buffer? It maintains CHUNK_SIZE buf internally, yet chunks are CHUNK_SIZE len too.
        // Buffer maintains 3 bytes for header + CHUNK_SIZE in case if we want to send an audio package.
        let mut sink = BufWriter::with_capacity(CHUNK_SIZE + 3, sink);

        while let Ok(event) = channel.recv_async().await {
            match event {
                MessageHandlerAction::Hangup => {
                    sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Terminate)?)
                        .await?;

                    sink.flush().await?;
                }
                MessageHandlerAction::RecognitionConfig(config) => {
                    if let Some(sender) = recognition_sender.take() {
                        sender.send(config).ok();
                    }
                }
                MessageHandlerAction::Play(audio) => {
                    let chunks = audio.chunks_exact(CHUNK_SIZE);
                    let remainder = chunks.remainder();

                    for chunk in chunks {
                        write_audio(&mut sink, chunk).await?;
                    }

                    if !remainder.is_empty() {
                        let mut remainder = remainder.to_owned();
                        remainder.resize(CHUNK_SIZE, 0);

                        write_audio(&mut sink, &remainder).await?;
                    }
                }
            }
        }

        Err(ServerError::ClientDisconnected(None))
    }
}

impl<'c, 's, ST, SI> Drop for IdentifiableMessageHandler<'c, 's, ST, SI> {
    fn drop(&mut self) {
        self.database.remove_handler(self.id);
    }
}

async fn prepare_recognition_service(
    id: Uuid,
    application_config: &'static Config,
    database: Arc<HandlerDatabase>,
    recognition_receiver: oneshot::Receiver<SpeechRecognitionConfig>,
) -> Option<SpawnedSpeechRecognition> {
    let recognition_config = timeout(
        application_config.recognition_config_timeout(),
        recognition_receiver,
    )
    .await
    .ok()
    .and_then(Result::ok)
    .or_else(|| {
        application_config
            .fallback_recognition_config()
            .as_ref()
            .cloned()
    })
    .unwrap_or_default();

    let recognition_config = SpeechRecognitionServiceConfig {
        application_config,
        recognition_config,
    };

    spawn_speech_recognition(id, recognition_config, database)
}

async fn write_audio<S>(mut sink: S, chunk: &[u8]) -> Result<(), ServerError>
where
    S: AsyncWrite + Unpin,
{
    sleep(TICK_DURATION).await;

    sink.write_all(&TryInto::<Vec<u8>>::try_into(Message::Audio(Some(chunk)))?)
        .await?;
    sink.flush().await?;

    Ok(())
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

        sleep(Duration::from_secs(1)).await;

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

        database.send(
            &Uuid::nil(),
            MessageHandlerAction::Play(Vec::from([1; 320 * 2])),
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
