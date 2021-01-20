use await_time::AwaitTime;
use futures_util::stream::{once, Map, Stream, StreamExt};
use thiserror::Error;
use tonic::{
    metadata::{errors::InvalidMetadataValue, MetadataValue},
    transport::{Certificate, Channel, ClientTlsConfig, Error as TransportError},
    Request, Status, Streaming,
};
use tracing::warn;
use yup_oauth2::AccessToken;

use crate::{
    gcs::codegen::{
        recognition_config::AudioEncoding, speech_client::SpeechClient,
        streaming_recognize_request::StreamingRequest, RecognitionConfig,
        StreamingRecognitionConfig, StreamingRecognizeRequest, StreamingRecognizeResponse,
    },
    recognition::RecognitionDriver,
};

const DOMAIN_NAME: &str = "speech.googleapis.com";
const ENDPOINT: &str = "https://speech.googleapis.com";

const CERTS: &[u8] = include_bytes!("cert.pem");

#[derive(Error, Debug)]
pub enum CloudSpeechError {
    #[error("Transport error: {0}")]
    TransportError(#[from] TransportError),

    #[error("Invalid OAuth key provided: {0}")]
    InvalidOauthKey(#[from] InvalidMetadataValue),

    #[error("Unable to call gRPC API: {0}")]
    CallError(#[from] Status),
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

pub struct GoogleCloudSpeech {
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
    pub fn new(max_time: AwaitTime, token: AccessToken) -> Self {
        Self { max_time, token }
    }
}

#[async_trait::async_trait]
impl<S> RecognitionDriver<S> for GoogleCloudSpeech
where
    S: Stream<Item = Vec<u8>> + Send + Sync + 'static,
{
    type Item = Result<Option<String>, Self::Error>;

    #[allow(clippy::type_complexity)]
    type Ok = Map<
        Streaming<StreamingRecognizeResponse>,
        fn(Result<StreamingRecognizeResponse, Status>) -> Result<Option<String>, Self::Error>,
    >;

    type Error = CloudSpeechError;

    async fn stream(&self, stream: S) -> Result<Self::Ok, Self::Error> {
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
                language_code: String::from("ru-RU"),
                max_alternatives: 1,
                profanity_filter: false,
                speech_contexts: Vec::new(),
                enable_word_time_offsets: false,
                enable_automatic_punctuation: false,
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
                .chain(stream.map(|audio| StreamingRecognizeRequest {
                    streaming_request: Some(StreamingRequest::AudioContent(audio)),
                })), *self.max_time.inner())
                .map(|result| {
                    match result {
                        Ok(req) => req,
                        Err(elapsed) => {
                            warn!(error = %elapsed, "Max time elapsed while awaiting for next audio message for GCS");

                            StreamingRecognizeRequest {
                                streaming_request: Some(StreamingRequest::AudioContent(Vec::from([0; 320])))
                            }
                        }
                    }
                })
            )
            .await?;

        Ok(stream.into_inner().map(|response| {
            response
                .map(|mut response| {
                    response
                        .results
                        .iter_mut()
                        .filter(|result| result.is_final)
                        .map(|result| result.alternatives.pop())
                        .next()
                        .flatten()
                        .map(|alternative| alternative.transcript)
                })
                .map_err(CloudSpeechError::from)
        }))
    }
}
