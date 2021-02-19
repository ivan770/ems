use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_util::sink::Sink;
use pin_project::pin_project;
use uuid::Uuid;

use crate::{db::HandlerDatabase, handler::MessageHandlerAction};

/// Generic speech synthesis request, containing UUID of handler and desirable text.
pub struct SpeechSynthesisRequest {
    /// Handler UUID, that will play audio after speech synthesis process.
    pub id: Uuid,

    /// Text to synthesize.
    pub text: String,

    /// Language and region of of the voice expressed as a BCP-47 language tag.
    pub language_code: String,

    /// Preferred voice gender.
    pub gender: Option<SynthesisVoiceGender>,
}

/// Gender of speech synthesis voice.
///
/// Consult <https://www.w3.org/TR/speech-synthesis11/#edef_voice> for additional info.
/// If used in [`Option`] context, [`SynthesisVoiceGender::Any`] should be used as a fallback.
pub enum SynthesisVoiceGender {
    Any,
    Male,
    Female,
    Neutral,
}

impl From<&str> for SynthesisVoiceGender {
    fn from(gender: &str) -> Self {
        match gender {
            "male" => SynthesisVoiceGender::Male,
            "female" => SynthesisVoiceGender::Female,
            "neutral" => SynthesisVoiceGender::Neutral,
            _ => SynthesisVoiceGender::Any,
        }
    }
}

/// Result of speech synthesis. Contains UUID of handler and synthesized audio.
///
/// [`SpeechSynthesisResponse`] does not validate audio in any way,
/// so checking provided data validity is up to user of this struct.
#[derive(Debug)]
pub struct SpeechSynthesisResponse {
    /// Handler UUID, that will play synthesized audio.
    pub id: Uuid,

    /// Result of speech synthesis process.
    pub audio: Vec<u8>,
}

#[pin_project]
pub struct SpeechSynthesisSink<E> {
    #[pin]
    database: Arc<HandlerDatabase>,
    _error: PhantomData<E>,
}

impl<E> SpeechSynthesisSink<E> {
    pub fn new(database: Arc<HandlerDatabase>) -> Self {
        Self {
            database,
            _error: PhantomData::default(),
        }
    }
}

impl<E> Sink<SpeechSynthesisResponse> for SpeechSynthesisSink<E> {
    type Error = E;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(
        self: Pin<&mut Self>,
        response: SpeechSynthesisResponse,
    ) -> Result<(), Self::Error> {
        let this = self.project();

        this.database
            .send(&response.id, MessageHandlerAction::Play(response.audio));

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
