use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_util::sink::Sink;
use uuid::Uuid;

use crate::db::HandlerDatabase;

pub struct SpeechRecognitionSink<E> {
    id: Uuid,
    database: Arc<HandlerDatabase>,
    _error: PhantomData<E>,
}

impl<E> SpeechRecognitionSink<E> {
    // We'll allow dead_code here to pass clippy test without default features
    #[allow(dead_code)]
    pub fn new(id: Uuid, database: Arc<HandlerDatabase>) -> Self {
        Self {
            id,
            database,
            _error: PhantomData::default(),
        }
    }
}

impl<E> Sink<String> for SpeechRecognitionSink<E>
where
    E: Unpin,
{
    type Error = E;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: String) -> Result<(), Self::Error> {
        let sink = self.get_mut();

        sink.database.add_transcription(sink.id, item);

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
