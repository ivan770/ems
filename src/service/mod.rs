use std::{error::Error, future::Future, sync::Arc};

use flume::{unbounded, Sender};
use from_config::FromConfig;
use futures_util::{
    sink::Sink,
    stream::{Stream, TryStream},
    StreamExt, TryStreamExt,
};
use tokio::spawn;
use tracing::instrument;
use uuid::Uuid;

use crate::{
    config::{Config, SpeechRecognitionDriver},
    db::HandlerDatabase,
    recognition::SpeechRecognitionSink,
};

#[cfg(feature = "gcs")]
use crate::gcs::driver::GoogleCloudSpeech;

/// FromConfig trait.
pub mod from_config;

/// A generic service definition.
///
/// In terms of EMS, anything that accepts a stream of requests and returns a stream of
/// responses is a service. You can take a look at speech recognition and synthesis implementation.
pub trait Service<S>: Send + Sync
where
    S: Stream<Item = Self::Input> + Send + Sync + 'static,
{
    /// A request for service.
    type Input;

    /// A response from service.
    type Output;

    /// A stream of responses, possibly with [`Result`] wrapping.
    type Ok: Stream<Item = Self::Output> + Send + Sync + Unpin;

    /// An error, that may happen during initialization.
    ///
    /// If an error happens to be during recognition process itself,
    /// wrap [`Self::Output`] in [`Result`].
    type Error: Error + Send + Sync + Unpin;

    /// Returned future.
    type Fut: Future<Output = Result<Self::Ok, Self::Error>> + Send;

    /// Start process of audio streaming.
    fn stream(self, stream: S) -> Self::Fut;
}

pub struct ServiceSpawner<'c> {
    id: Uuid,
    config: &'c Config,
    database: Arc<HandlerDatabase>,
}

impl<'c> ServiceSpawner<'c> {
    pub fn new(id: Uuid, config: &'c Config, database: Arc<HandlerDatabase>) -> Self {
        Self {
            id,
            config,
            database,
        }
    }
}

impl ServiceSpawner<'static> {
    #[instrument(skip(config, stream, sink), err)]
    async fn spawn_service<I, O, S>(
        config: &'static Config,
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
            > + FromConfig<'static>,
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

    pub fn spawn_from_config(self) -> (Option<SpawnedSpeechRecognition>,) {
        let recognition = match self.config.recognition_driver() {
            #[cfg(feature = "gcs")]
            Some(SpeechRecognitionDriver::GoogleCloudSpeech) => {
                let (bytes_sender, bytes_receiver) = unbounded();
                let transcription_sink = SpeechRecognitionSink::new(self.id, self.database);

                spawn(Self::spawn_service::<_, _, GoogleCloudSpeech>(
                    self.config,
                    bytes_receiver.into_stream(),
                    transcription_sink,
                ));

                Some(SpawnedSpeechRecognition::new(bytes_sender))
            }
            None => None,
        };

        (recognition,)
    }
}

pub struct SpawnedSpeechRecognition {
    sender: Sender<Vec<u8>>,
}

impl SpawnedSpeechRecognition {
    fn new(sender: Sender<Vec<u8>>) -> Self {
        Self { sender }
    }

    pub fn send(&self, audio: Vec<u8>) {
        self.sender.send(audio).ok();
    }
}
