use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_util::sink::Sink;
use pin_project::pin_project;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::Config, db::HandlerDatabase, ws::WsNotification};

/// Speech recognition request, that contains audio to be recognized.
///
/// Audio inside may be any byte sequence, thus validation of audio
/// is left for user of this struct.
pub struct SpeechRecognitionRequest {
    /// Unknown audio byte sequence.
    pub audio: Vec<u8>,
}

/// Result of speech recognition. Contains audio transcription.
pub struct SpeechRecognitionResponse {
    pub transcription: String,
}

/// Speech recognition service config.
///
/// Unlike speech synthesis service, speech recognition is created for each
/// new handler, thus all configuration is done once per initialization,
/// unlike once per each request as with speech synthesis.
///
/// This struct contains both [`application config`] and speech recognition
/// specific options via [`SpeechRecognitionConfig`]
///
/// [`application config`]: Config
#[derive(Clone)]
pub struct SpeechRecognitionServiceConfig<'c> {
    /// Application configuration.
    pub application_config: &'c Config,

    /// Speech recognition specific options
    pub recognition_config: SpeechRecognitionConfig,
}

/// Speech recognition specific options
///
/// For more details see [`SpeechRecognitionServiceConfig`].
#[derive(Serialize, Deserialize, Clone)]
#[cfg_attr(test, derive(Debug))]
pub struct SpeechRecognitionConfig {
    /// Language that is being recognized.
    pub language: String,

    /// Enable profanity filter (if provider supports it)?
    #[serde(default)]
    pub profanity_filter: bool,

    /// Enable punctuation guessing (if provider supports it)?
    #[serde(default)]
    pub punctuation: bool,
}

impl Default for SpeechRecognitionConfig {
    fn default() -> Self {
        SpeechRecognitionConfig {
            language: String::from("en-US"),
            profanity_filter: false,
            punctuation: false,
        }
    }
}

#[pin_project]
pub struct SpeechRecognitionSink<E> {
    id: Uuid,
    #[pin]
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

impl<E> Sink<SpeechRecognitionResponse> for SpeechRecognitionSink<E> {
    type Error = E;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(
        self: Pin<&mut Self>,
        response: SpeechRecognitionResponse,
    ) -> Result<(), Self::Error> {
        let this = self.project();

        this.database
            .add_notification(WsNotification::Transcription(
                *this.id,
                response.transcription,
            ));

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
