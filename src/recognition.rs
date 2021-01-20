use std::{
    convert::Infallible,
    error::Error,
    future::{ready, Future},
    iter::{empty, Empty},
    marker::PhantomData,
    sync::Arc,
};

use futures_util::{
    sink::{Sink, SinkExt},
    stream::{iter, Iter, Stream, TryStreamExt},
};
use tokio::spawn;
use tracing::instrument;
use uuid::Uuid;

use crate::db::HandlerDatabase;

/// Recognition driver, that allows to use different speech-to-text services
pub trait RecognitionDriver<S>: Send + Sync
where
    S: Stream<Item = Vec<u8>> + Send + Sync + 'static,
{
    /// A response from recognition service.
    type Item;

    /// A stream of responses, possibly with [`Result`] wrapping.
    type Ok: Stream<Item = Self::Item> + Send + Sync + Unpin;

    /// An error, that may happen during initialization.
    ///
    /// If an error happens to be during recognition process itself,
    /// wrap [`Self::Item`] in [`Result`].
    type Error: Error + Send + Sync + Unpin;

    /// Returned future.
    type Fut: Future<Output = Result<Self::Ok, Self::Error>> + Send;

    /// Start process of audio streaming.
    fn stream(self, stream: S) -> Self::Fut;
}

/// An empty driver implementation, that can be used to silence missing driver type
/// in case if all existing drivers were disabled.
///
/// Always returns [`None`].
pub struct DummyDriver<I> {
    item: PhantomData<I>,
}

impl<I> DummyDriver<I> {
    /// Create [`None`] variant of [`Option`] with `DummyDriver` as T.
    ///
    /// Can be used to silence compiler in case if all other drivers
    /// are not available.
    pub fn none() -> Option<Self> {
        None
    }
}

impl<I, S> RecognitionDriver<S> for DummyDriver<I>
where
    I: Send + Sync,
    S: Stream<Item = Vec<u8>> + Send + Sync + 'static,
{
    type Item = I;

    type Ok = Iter<Empty<I>>;

    type Error = Infallible;

    type Fut = impl Future<Output = Result<Self::Ok, Self::Error>>;

    fn stream(self, _: S) -> Self::Fut {
        ready(Ok(iter(empty())))
    }
}

/// Generic speech recognition interface, that uses [`RecognitionDriver`] under the hood.
///
/// [`SpeechRecognition`] itself does nothing, but provides a way to upgrade it to [`DispatchedSpeechRecognition`],
/// that may be used to stop speech recognition tasks.
pub struct SpeechRecognition<S, R, D> {
    id: Uuid,
    database: Arc<HandlerDatabase>,
    sender: S,
    receiver: R,
    driver: D,
}

impl<S, R, D> SpeechRecognition<S, R, D>
where
    S: Sink<Vec<u8>> + Unpin + Send + Sync + 'static,
    R: Stream<Item = Vec<u8>> + Send + Sync + 'static,
    D: RecognitionDriver<R, Item = Result<Option<String>, <D as RecognitionDriver<R>>::Error>>
        + 'static,
    D::Ok: Unpin,
{
    pub fn new(
        id: Uuid,
        database: Arc<HandlerDatabase>,
        sender: S,
        receiver: R,
        driver: D,
    ) -> Self {
        Self {
            id,
            database,
            sender,
            receiver,
            driver,
        }
    }

    pub fn spawn(self) -> DispatchedSpeechRecognition<S> {
        spawn(Self::dispatch(
            self.id,
            self.database,
            self.receiver,
            self.driver,
        ));

        DispatchedSpeechRecognition::new(self.sender)
    }

    #[instrument(skip(database, receiver, driver), err)]
    async fn dispatch(
        id: Uuid,
        database: Arc<HandlerDatabase>,
        receiver: R,
        driver: D,
    ) -> Result<(), D::Error> {
        let mut stream = driver
            .stream(receiver)
            .await?
            .map_ok(Ok)
            .try_filter_map(ready);

        while let Some(recognition_result) = stream.try_next().await? {
            database.add_transcription(id, recognition_result);
        }

        Ok(())
    }
}

pub struct DispatchedSpeechRecognition<S> {
    sender: S,
}

impl<S> DispatchedSpeechRecognition<S>
where
    S: Sink<Vec<u8>> + Unpin + Send + Sync + 'static,
{
    fn new(sender: S) -> Self {
        Self { sender }
    }

    pub async fn send(&mut self, message: Vec<u8>) {
        self.sender.send(message).await.ok();
    }
}
