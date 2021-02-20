use std::{future::ready, io::Error, sync::Arc};

use futures_util::{sink::Sink, stream::Stream, SinkExt, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, from_str, to_string, Error as JsonError};
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error as TungsteniteError, Message},
};
use tracing::{debug, error, info, instrument};
use uuid::Uuid;

use crate::{
    config::Config,
    db::HandlerDatabase,
    handler::MessageHandlerAction,
    service::{spawn_speech_synthesis, SpawnedSpeechSynthesis},
    synthesis::SpeechSynthesisRequest,
};

/// Generic WebSocket message for both requests and responses.
#[derive(Serialize, Deserialize)]
struct WsMessage {
    /// ID of handler, that has to receive request, or that sends response.
    id: Uuid,

    /// Inner message data.
    data: WsAction,
}

/// WebSocket actions, that handler can send or execute.
#[derive(Serialize, Deserialize)]
enum WsAction {
    /// Terminate current call.
    Hangup,

    /// Synthesize speech using configurated service and play it on channel.
    Synthesize {
        /// Text to synthesize.
        text: String,

        /// Language and region of of the voice expressed as a BCP-47 language tag.
        language_code: String,

        /// Preferred voice gender.
        gender: Option<String>,
    },

    /// Speech transcription part of current call.
    Transcription(String),
}

/// Errors, that may happen during WebSocket connection.
#[derive(Error, Debug)]
enum WsError {
    #[error("Invalid message received: {0}")]
    InvalidMessageReceived(#[from] JsonError),

    #[error("Internal WebSocket error: {0}")]
    InternalError(#[from] TungsteniteError),

    #[error("Unsupported WebSocket message type")]
    UnsupportedMessageType,
}

/// WebSocket server
pub struct WsServer<'c> {
    config: &'c Config,
    database: Arc<HandlerDatabase>,
}

impl<'c> WsServer<'c> {
    /// Create new WebSocket server from provided config and handler database.
    pub fn new(config: &'c Config, database: Arc<HandlerDatabase>) -> Self {
        Self { config, database }
    }
}

impl WsServer<'static> {
    /// Start WebSocket server on host provided in config.
    #[instrument(skip(self))]
    pub async fn listen(self) -> Result<(), Error> {
        let listener = TcpListener::bind(self.config.websocket_addr()).await?;

        info!("Started listening...");

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    spawn(handle_ws(self.config, self.database.clone(), stream));
                }
                Err(e) => error!(inner = %e, "Unable to accept incoming TCP connection."),
            }
        }
    }
}

/// Handle new WebSocket connection.
async fn handle_ws(config: &'static Config, database: Arc<HandlerDatabase>, stream: TcpStream) {
    let (sink, stream) = accept_async(stream).await.unwrap().split();

    let synthesis = spawn_speech_synthesis(config, database.clone());

    select! {
        _ = accept_messages(database.as_ref(), stream, &synthesis) => {}
        _ = send_transcriptions(database.as_ref(), sink) => {},
    };
}

/// Start accepting incoming messages on provided [`Stream`].
#[instrument(skip(database, stream, synthesis))]
async fn accept_messages<S>(
    database: &HandlerDatabase,
    stream: S,
    synthesis: &Option<SpawnedSpeechSynthesis>,
) where
    S: Stream<Item = Result<Message, TungsteniteError>>,
{
    stream
        .map_err(WsError::InternalError)
        .try_filter(|message| ready(matches!(message, Message::Text(_) | Message::Binary(_))))
        .try_filter_map(|m| async move {
            match m {
                Message::Text(text) => from_str::<WsMessage>(&text)
                    .map_err(WsError::InvalidMessageReceived)
                    .map(Some),
                Message::Binary(bytes) => from_slice(&bytes)
                    .map_err(WsError::InvalidMessageReceived)
                    .map(Some),
                _ => Err(WsError::UnsupportedMessageType),
            }
        })
        .for_each_concurrent(None, |message| async move {
            match message {
                Ok(message) => match (message.data, synthesis) {
                    (WsAction::Hangup, _) => {
                        database.send(&message.id, MessageHandlerAction::Hangup);
                    }
                    (
                        WsAction::Synthesize {
                            text,
                            language_code,
                            gender,
                        },
                        Some(synthesis),
                    ) => {
                        synthesis.send(SpeechSynthesisRequest {
                            id: message.id,
                            text,
                            language_code,
                            gender: gender.as_deref().map(Into::into),
                        });
                    }
                    _ => {}
                },
                Err(e) => debug!(error = %e, "Invalid WebSocket message"),
            }
        })
        .await;
}

/// Start sending transcriptions to provided [`Sink`].
#[instrument(skip(database, sink), err)]
async fn send_transcriptions<S>(database: &HandlerDatabase, mut sink: S) -> Result<(), WsError>
where
    S: Sink<Message, Error = TungsteniteError> + Unpin,
{
    loop {
        let (id, transcription) = database.recv_transcription().await;

        sink.send(Message::Text(to_string(&WsMessage {
            id,
            data: WsAction::Transcription(transcription),
        })?))
        .await?;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::ready,
        marker::PhantomData,
        pin::Pin,
        task::{Context, Poll},
    };

    use flume::{unbounded, Sender};
    use futures_util::{sink::Sink, stream::once};
    use serde_json::from_str;
    use tokio::spawn;
    use tokio_tungstenite::tungstenite::Message;
    use tracing_test::traced_test;
    use uuid::Uuid;

    use super::{accept_messages, send_transcriptions, WsAction, WsMessage};
    use crate::{db::HandlerDatabase, handler::MessageHandlerAction};

    const TEST_ID: Uuid = Uuid::nil();

    struct TestSink<T, E>(Sender<T>, PhantomData<E>);

    impl<T, E> TestSink<T, E> {
        pub fn new(sender: Sender<T>) -> Self {
            TestSink(sender, PhantomData::default())
        }
    }

    impl<T, E> Sink<T> for TestSink<T, E>
    where
        T: Unpin,
        E: Unpin,
    {
        type Error = E;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
            Ok(self.get_mut().0.send(item).unwrap())
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    #[traced_test]
    async fn accept_invalid_ws_message() {
        let stream = once(ready(Ok(Message::Text(format!(
            r#"
            {{
                "id": "{}",
                "data": "InvalidMessageData"
            }}
        "#,
            TEST_ID
        )))));

        let database = HandlerDatabase::default();

        let (sender, _) = unbounded();

        database.add_handler(TEST_ID, sender);

        accept_messages(&database, stream, &None).await;

        assert!(logs_contain("Invalid WebSocket message"));
    }

    #[tokio::test]
    async fn accept_ws_hangup() {
        let stream = once(ready(Ok(Message::Text(format!(
            r#"
            {{
                "id": "{}",
                "data": "Hangup"
            }}
        "#,
            TEST_ID
        )))));

        let database = HandlerDatabase::default();

        let (sender, receiver) = unbounded();

        database.add_handler(TEST_ID, sender);

        accept_messages(&database, stream, &None).await;

        let recv = receiver.recv().unwrap();

        assert!(matches!(recv, MessageHandlerAction::Hangup));
    }

    #[tokio::test]
    async fn send_ws_transcriptions() {
        let (sender, receiver) = unbounded();

        let sink = TestSink::new(sender);

        let database = HandlerDatabase::default();

        database.add_transcription(TEST_ID, String::from("Hello, world"));

        spawn(async move {
            send_transcriptions(&database, sink).await.unwrap();
        });

        match receiver.recv_async().await.unwrap() {
            Message::Text(text) => {
                let ws_message = from_str::<WsMessage>(&text).unwrap();
                assert!(
                    matches!(ws_message, WsMessage { id, data: WsAction::Transcription(text) } if id == TEST_ID && text == String::from("Hello, world"))
                );
            }
            _ => panic!("Invalid message type"),
        }
    }
}
