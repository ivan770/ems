use std::{
    future::{ready, Future},
    io::Error as IoError,
    path::PathBuf,
};

use await_time::AwaitTime;
use flume::SendError;
use futures_util::stream::{once, Stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tonic::{
    metadata::{errors::InvalidMetadataValue, MetadataValue},
    transport::{Certificate, Channel, ClientTlsConfig, Error as TransportError},
    Request, Status,
};
use tracing::warn;
use yup_oauth2::{
    read_service_account_key, AccessToken, Error as OauthError, ServiceAccountAuthenticator,
};

use crate::{
    google::{
        codegen::{
            recognition_config::AudioEncoding, speech_client::SpeechClient,
            streaming_recognize_request::StreamingRequest, RecognitionConfig,
            StreamingRecognitionConfig, StreamingRecognizeRequest,
        },
        AUTH_SCOPE, CERTS,
    },
    handler::CHUNK_SIZE,
    recognition::{SpeechRecognitionConfig, SpeechRecognitionRequest, SpeechRecognitionResponse},
    service::{FromConfig, Service},
};

const DOMAIN_NAME: &str = "speech.googleapis.com";
const ENDPOINT: &str = "https://speech.googleapis.com";

#[derive(Error, Debug)]
pub enum CloudSpeechError {
    #[error("Application config doesn't contain GCS settings")]
    NoSuitableConfigFound,

    #[error("Transport error: {0}")]
    TransportError(#[from] TransportError),

    #[error("IO error: {0}")]
    IoError(#[from] IoError),

    #[error("Invalid OAuth key provided: {0}")]
    InvalidOauthKey(#[from] InvalidMetadataValue),

    #[error("Authentication error: {0}")]
    AuthenticationError(#[from] OauthError),

    #[error("Unable to call gRPC API: {0}")]
    CallError(#[from] Status),

    #[error("Unable to send transcription to background service: {0}")]
    FlumeError(#[from] SendError<String>),
}

pub mod await_time {
    use std::time::Duration;

    /// Max await time between each audio package.
    ///
    /// While usually all AudioSocket clients should provide data
    /// near real-time, sometimes you might be stuck in a situation
    /// when there are no audio packages incoming.
    ///
    /// For this reason, GCS client checks for time between each package
    /// and automatically sends empty audio bytes is we don't have any actual payload to send.
    pub struct AwaitTime(Duration);

    impl AwaitTime {
        /// Create new AwaitTime from provided seconds.
        ///
        /// Returns [`None`] if `secs > 10`.
        pub fn new(secs: u64) -> Option<Self> {
            if secs <= 10 {
                Some(Self(Duration::from_secs(secs)))
            } else {
                None
            }
        }

        /// Unwrap [`Duration`] from [`AwaitTime`].
        pub fn inner(&self) -> &Duration {
            &self.0
        }
    }

    impl Default for AwaitTime {
        fn default() -> Self {
            // We can give ourselves some time to send empty audio package to GCS by using 5 instead of 10.
            Self(Duration::from_secs(5))
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct GoogleCloudSpeechConfig {
    /// Path to service credentials file.
    pub(crate) service_account_path: PathBuf,

    /// Max await time in seconds between AudioSocket audio messages.
    ///
    /// Valid values are `0..=10`.
    pub(crate) max_await_time: Option<u64>,
}

pub struct GoogleCloudSpeech {
    /// Language, that is being recognized.
    ///
    /// Consult <https://cloud.google.com/speech-to-text/docs/languages> for more info
    language: String,

    /// Text punctuation guessing toggle
    punctuation: bool,

    /// Profanity filter toggle
    profanity_filter: bool,

    /// Max await time between each audio message.
    ///
    /// Check [`AwaitTime`] for details.
    max_time: AwaitTime,

    /// OAuth2 access token.
    ///
    /// Usually generated from service credentials file.
    token: AccessToken,
}

impl GoogleCloudSpeech {
    pub fn new(
        language: String,
        punctuation: bool,
        profanity_filter: bool,
        max_time: AwaitTime,
        token: AccessToken,
    ) -> Self {
        Self {
            language,
            punctuation,
            profanity_filter,
            max_time,
            token,
        }
    }
}

impl<S> Service<S> for GoogleCloudSpeech
where
    S: Stream<Item = SpeechRecognitionRequest> + Send + Sync + 'static,
{
    type Input = SpeechRecognitionRequest;

    type Output = Result<SpeechRecognitionResponse, Self::Error>;

    type Ok = impl Stream<Item = Self::Output>;

    type Error = CloudSpeechError;

    type Fut = impl Future<Output = Result<Self::Ok, Self::Error>>;

    fn stream(self, stream: S) -> Self::Fut {
        async move {
            let tls_config = ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(CERTS))
                .domain_name(DOMAIN_NAME);

            let channel = Channel::from_static(ENDPOINT)
                .tls_config(tls_config)?
                .connect()
                .await?;

            let oauth_key = MetadataValue::from_str(&format!("Bearer {}", self.token.as_str()))?;

            let mut speech = SpeechClient::with_interceptor(channel, move |mut req: Request<()>| {
                req.metadata_mut()
                    .insert("authorization", oauth_key.clone());

                Ok(req)
            });

            let streaming_config = StreamingRequest::StreamingConfig(StreamingRecognitionConfig {
                config: Some(RecognitionConfig {
                    encoding: AudioEncoding::Linear16.into(),
                    sample_rate_hertz: 8000,
                    audio_channel_count: 1,
                    enable_separate_recognition_per_channel: false,
                    language_code: self.language,
                    max_alternatives: 1,
                    profanity_filter: self.profanity_filter,
                    speech_contexts: Vec::new(),
                    enable_word_time_offsets: false,
                    enable_automatic_punctuation: self.punctuation,
                    diarization_config: None,
                    metadata: None,
                    model: String::from("phone_call"),
                    use_enhanced: false,
                }),
                single_utterance: false,
                interim_results: true,
            });

            let stream = speech
                .streaming_recognize(
                    tokio_stream::StreamExt::timeout(once(async move {
                        StreamingRecognizeRequest {
                            streaming_request: Some(streaming_config),
                        }
                    })
                    .chain(stream.map(|recognition_request| StreamingRecognizeRequest {
                        streaming_request: Some(StreamingRequest::AudioContent(recognition_request.audio)),
                    })), *self.max_time.inner())
                    .map(|result| {
                        match result {
                            Ok(req) => req,
                            Err(elapsed) => {
                                warn!(error = %elapsed, "Max time elapsed while awaiting for next audio message for GCS");

                                StreamingRecognizeRequest {
                                    streaming_request: Some(StreamingRequest::AudioContent(Vec::from([0; CHUNK_SIZE])))
                                }
                            }
                        }
                    })
                )
                .await?;

            Ok(stream
                .into_inner()
                .try_filter_map(|mut response| {
                    ready(Ok(response
                        .results
                        .iter_mut()
                        .filter(|result| result.is_final)
                        .map(|result| result.alternatives.pop())
                        .next()
                        .flatten()
                        .map(|alternative| alternative.transcript)))
                })
                .map_ok(move |transcription| SpeechRecognitionResponse { transcription })
                .map_err(CloudSpeechError::from))
        }
    }
}

impl<'c> FromConfig<'c> for GoogleCloudSpeech
where
    Self: Sized,
{
    type Config = SpeechRecognitionConfig<'c>;

    type Error = CloudSpeechError;

    type Fut = impl Future<Output = Result<Self, Self::Error>>;

    fn from_config(config: Self::Config) -> Self::Fut {
        async move {
            let authenticator = ServiceAccountAuthenticator::builder(
                read_service_account_key(
                    &config
                        .application_config
                        .gcs_config()
                        .as_ref()
                        .ok_or(CloudSpeechError::NoSuitableConfigFound)?
                        .service_account_path,
                )
                .await?,
            )
            .build()
            .await?;

            let access_token = authenticator.token(&[AUTH_SCOPE]).await?;

            Ok(GoogleCloudSpeech::new(
                config.language,
                config.punctuation,
                config.profanity_filter,
                config
                    .application_config
                    .gcs_config()
                    .as_ref()
                    .ok_or(CloudSpeechError::NoSuitableConfigFound)?
                    .max_await_time
                    .map(AwaitTime::new)
                    .flatten()
                    .unwrap_or_default(),
                access_token,
            ))
        }
    }
}
