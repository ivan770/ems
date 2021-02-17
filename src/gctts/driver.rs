use std::{future::Future, io::Error as IoError};

use async_stream::stream;
use futures_util::{pin_mut, stream::Stream};
use thiserror::Error;
use tonic::{
    metadata::{errors::InvalidMetadataValue, MetadataValue},
    transport::{Certificate, Channel, ClientTlsConfig, Error as TransportError},
    Request, Status,
};
use yup_oauth2::{
    read_service_account_key, AccessToken, Error as OauthError, ServiceAccountAuthenticator,
};

use crate::{
    config::Config,
    google::{
        codegen::{
            synthesis_input::InputSource, text_to_speech_client::TextToSpeechClient, AudioConfig,
            AudioEncoding, SsmlVoiceGender, SynthesisInput, SynthesizeSpeechRequest,
            VoiceSelectionParams,
        },
        AUTH_SCOPE, CERTS,
    },
    service::{from_config::FromConfig, Service},
    synthesis::{SpeechSynthesisRequest, SpeechSynthesisResponse},
};

const DOMAIN_NAME: &str = "texttospeech.googleapis.com";
const ENDPOINT: &str = "https://texttospeech.googleapis.com";

#[derive(Error, Debug)]
pub enum CloudTextToSpeechError {
    #[error("Application config doesn't contain GCTTS settings")]
    NoSuitableConfigFound,

    #[error("Transport error: {0}")]
    TransportError(#[from] TransportError),

    #[error("Invalid OAuth key provided: {0}")]
    InvalidOauthKey(#[from] InvalidMetadataValue),

    #[error("IO error: {0}")]
    IoError(#[from] IoError),

    #[error("Authentication error: {0}")]
    AuthenticationError(#[from] OauthError),

    #[error("Unable to call gRPC API: {0}")]
    CallError(#[from] Status),
}

pub struct GoogleCloudTextToSpeech {
    /// OAuth2 access token.
    ///
    /// Usually generated from service credentials file.
    token: AccessToken,
}

impl GoogleCloudTextToSpeech {
    pub fn new(token: AccessToken) -> Self {
        Self { token }
    }
}

impl<S> Service<S> for GoogleCloudTextToSpeech
where
    S: Stream<Item = SpeechSynthesisRequest> + Send + Sync + 'static,
{
    type Input = SpeechSynthesisRequest;

    type Output = Result<SpeechSynthesisResponse, Self::Error>;

    type Ok = impl Stream<Item = Self::Output>;

    type Error = CloudTextToSpeechError;

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

            let speech =
                TextToSpeechClient::with_interceptor(channel, move |mut req: Request<()>| {
                    req.metadata_mut()
                        .insert("authorization", oauth_key.clone());

                    Ok(req)
                });

            Ok(prepare_response_stream(speech, stream))
        }
    }
}

fn prepare_response_stream<S>(
    client: TextToSpeechClient<Channel>,
    incoming: S,
) -> impl Stream<Item = Result<SpeechSynthesisResponse, CloudTextToSpeechError>> + Send
where
    S: Stream<Item = SpeechSynthesisRequest> + Send + Sync + 'static,
{
    stream! {
        pin_mut!(client);

        for await request in incoming {
            let response = client.synthesize_speech(SynthesizeSpeechRequest {
                input: Some(SynthesisInput {
                    input_source: Some(InputSource::Text(request.text)),
                }),
                voice: Some(VoiceSelectionParams {
                    language_code: String::from("en-US"),
                    name: String::from(""),
                    ssml_gender: SsmlVoiceGender::Male.into(),
                }),
                audio_config: Some(AudioConfig {
                    audio_encoding: AudioEncoding::Linear16.into(),
                    speaking_rate: 1.,
                    pitch: 0.,
                    volume_gain_db: 0.,
                    sample_rate_hertz: 8000,
                    effects_profile_id: vec![String::from("telephony-class-application")],
                }),
            }).await.map_err(CloudTextToSpeechError::from)?;

            let audio = response.into_inner().audio_content;

            yield Ok(SpeechSynthesisResponse {
                id: request.id,
                audio,
            })
        }
    }
}

impl<'c> FromConfig<'c> for GoogleCloudTextToSpeech
where
    Self: Sized,
{
    type Config = &'c Config;

    type Error = CloudTextToSpeechError;

    type Fut = impl Future<Output = Result<Self, Self::Error>>;

    fn from_config(config: Self::Config) -> Self::Fut {
        async move {
            let authenticator = ServiceAccountAuthenticator::builder(
                read_service_account_key(
                    &config
                        .gctts_config()
                        .as_ref()
                        .ok_or(CloudTextToSpeechError::NoSuitableConfigFound)?
                        .service_account_path,
                )
                .await?,
            )
            .build()
            .await?;

            let access_token = authenticator.token(&[AUTH_SCOPE]).await?;

            Ok(GoogleCloudTextToSpeech::new(access_token))
        }
    }
}
