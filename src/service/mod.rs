use std::{error::Error, future::Future, sync::Arc};

use flume::Sender;
use from_config::FromConfig;
use futures_util::{
    sink::Sink,
    stream::{Stream, TryStream},
    StreamExt, TryStreamExt,
};
use tracing::instrument;
use uuid::Uuid;
#[cfg(any(feature = "gcs", feature = "gctts"))]
use {flume::unbounded, tokio::spawn};

use crate::{
    config::Config,
    db::HandlerDatabase,
    recognition::{SpeechRecognitionConfig, SpeechRecognitionRequest},
    synthesis::SpeechSynthesisRequest,
};
#[cfg(feature = "gcs")]
use crate::{
    config::SpeechRecognitionDriver, gcs::GoogleCloudSpeech, recognition::SpeechRecognitionSink,
};
#[cfg(feature = "gctts")]
use crate::{
    config::SpeechSynthesisDriver, gctts::GoogleCloudTextToSpeech, synthesis::SpeechSynthesisSink,
};

/// FromConfig trait.
pub mod from_config;

/// A generic service definition.
///
/// In terms of EMS, anything that accepts a stream of requests and returns a stream of
/// responses is a service. You can take a look at speech recognition and synthesis implementation.
pub trait Service<S>: Send + Sync
where
    S: Stream<Item = Self::Input>,
{
    /// A request for service.
    type Input;

    /// A response from service.
    type Output;

    /// A stream of responses, possibly with [`Result`] wrapping.
    type Ok: Stream<Item = Self::Output>;

    /// An error, that may happen during initialization.
    ///
    /// If an error happens to be during recognition process itself,
    /// wrap [`Self::Output`] in [`Result`].
    type Error: Error + Send + Sync + Unpin;

    /// Returned future.
    type Fut: Future<Output = Result<Self::Ok, Self::Error>>;

    /// Start process of audio streaming.
    fn stream(self, stream: S) -> Self::Fut;
}

#[instrument(skip(config, stream, sink), err)]
async fn spawn_service<C, I, O, S>(
    config: C,
    stream: I,
    mut sink: O,
) -> Result<(), <S as Service<I>>::Error>
where
    I: Stream<Item = <S as Service<I>>::Input> + Send + Sync + 'static,
    O: Sink<<<S as Service<I>>::Ok as TryStream>::Ok, Error = <S as Service<I>>::Error>
        + Send
        + Unpin,
    S: Service<
            I,
            Output = Result<<<S as Service<I>>::Ok as TryStream>::Ok, <S as Service<I>>::Error>,
        > + FromConfig<'static, Config = C>,
    S::Ok: TryStream,
    <S as Service<I>>::Error:
        From<<S as FromConfig<'static>>::Error> + From<<S::Ok as TryStream>::Error>,
{
    S::from_config(config)
        .await?
        .stream(stream)
        .await?
        .map_err(<S as Service<I>>::Error::from)
        .forward(&mut sink)
        .await?;

    Ok(())
}

/// Spawn needed services from current application config.
///
/// Some services require their own respective features to be enabled (`gcs` for Google Cloud Speech as an example).
#[allow(unused_variables)]
pub fn spawn_speech_recognition(
    id: Uuid,
    config: SpeechRecognitionConfig<'static>,
    database: Arc<HandlerDatabase>,
) -> Option<SpawnedSpeechRecognition> {
    match config.application_config.recognition_driver() {
        #[cfg(feature = "gcs")]
        Some(SpeechRecognitionDriver::GoogleCloudSpeech) => {
            let (bytes_sender, bytes_receiver) = unbounded();
            let transcription_sink = SpeechRecognitionSink::new(id, database);

            spawn(spawn_service::<_, _, _, GoogleCloudSpeech>(
                config,
                bytes_receiver.into_stream(),
                transcription_sink,
            ));

            Some(SpawnedSpeechRecognition::new(bytes_sender))
        }
        #[cfg(not(feature = "gcs"))]
        Some(_) => None,
        None => None,
    }
}

#[allow(unused_variables)]
pub fn spawn_speech_synthesis(
    config: &'static Config,
    database: Arc<HandlerDatabase>,
) -> Option<SpawnedSpeechSynthesis> {
    match config.synthesis_driver() {
        #[cfg(feature = "gctts")]
        Some(SpeechSynthesisDriver::GoogleCloudTextToSpeech) => {
            let (text_sender, text_receiver) = unbounded();
            let synthesis_sink = SpeechSynthesisSink::new(database);

            spawn(spawn_service::<_, _, _, GoogleCloudTextToSpeech>(
                config,
                text_receiver.into_stream(),
                synthesis_sink,
            ));

            Some(SpawnedSpeechSynthesis::new(text_sender))
        }
        #[cfg(not(feature = "gctts"))]
        Some(_) => None,
        None => None,
    }
}

/// Active background speech recognition task
pub struct SpawnedSpeechRecognition {
    sender: Sender<SpeechRecognitionRequest>,
}

impl SpawnedSpeechRecognition {
    /// Create new [`SpawnedSpeechRecognition`] from provided [`Sender`].
    #[allow(dead_code)]
    fn new(sender: Sender<SpeechRecognitionRequest>) -> Self {
        Self { sender }
    }

    /// Send new audio for recognition.
    pub fn send(&self, request: SpeechRecognitionRequest) {
        self.sender.send(request).ok();
    }
}

pub struct SpawnedSpeechSynthesis {
    sender: Sender<SpeechSynthesisRequest>,
}

impl SpawnedSpeechSynthesis {
    /// Create new [`SpawnedSpeechRecognition`] from provided [`Sender`].
    #[allow(dead_code)]
    pub fn new(sender: Sender<SpeechSynthesisRequest>) -> Self {
        Self { sender }
    }

    /// Send new text for speech synthesis.
    pub fn send(&self, request: SpeechSynthesisRequest) {
        self.sender.send(request).ok();
    }
}
