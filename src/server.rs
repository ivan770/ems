use std::{io::Error as IoError, sync::Arc, time::Duration};

use audiosocket::AudioSocketError;

use flume::r#async::RecvStream;
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
    time::error::Elapsed,
};
use tracing::{error, info, instrument};

use crate::{
    config::Config,
    db::HandlerDatabase,
    handler::AnonymousMessageHandler,
    recognition::{DummyDriver, RecognitionDriver},
    stream::MessageStream,
};

#[cfg(feature = "gcs")]
use crate::{
    config::SpeechRecognitionDriver,
    gcs::driver::{await_time::AwaitTime, GoogleCloudSpeech},
};

#[cfg(feature = "gcs")]
use yup_oauth2::{read_service_account_key, Error as AuthError, ServiceAccountAuthenticator};

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("IO error: {0}")]
    IoError(#[from] IoError),

    #[error("AudioSocket error: {0}")]
    AudioSocketError(#[from] AudioSocketError),

    #[error("Exceeded max await time for next AudioSocket message: {0}")]
    TimeoutError(#[from] Elapsed),

    #[cfg(feature = "gcs")]
    #[error("Authentication error: {0}")]
    AuthError(#[from] AuthError),
}

pub struct AudioSocketServer<'c> {
    config: &'c Config,
    database: Arc<HandlerDatabase>,
}

impl<'c> AudioSocketServer<'c> {
    pub fn new(config: &'c Config, database: Arc<HandlerDatabase>) -> Self {
        AudioSocketServer { config, database }
    }
}

impl AudioSocketServer<'static> {
    #[instrument(skip(self), err)]
    pub async fn listen(self) -> Result<(), IoError> {
        let listener = TcpListener::bind(self.config.audiosocket_addr()).await?;

        info!("Started listening...");

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    spawn(handle_tcp(self.config, self.database.clone(), stream));
                }
                Err(e) => error!(inner = %e, "Unable to accept incoming TCP connection."),
            }
        }
    }
}

#[instrument(skip(config, database, stream), err)]
async fn handle_tcp(
    config: &'static Config,
    database: Arc<HandlerDatabase>,
    stream: TcpStream,
) -> Result<(), ServerError> {
    match config.recognition_driver() {
        #[cfg(feature = "gcs")]
        Some(SpeechRecognitionDriver::GoogleCloudSpeech) => match config.gcs_config() {
            Some(gcs_config) => {
                let authenticator = ServiceAccountAuthenticator::builder(
                    read_service_account_key(&gcs_config.service_account_path).await?,
                )
                .build()
                .await?;

                let driver = Some(GoogleCloudSpeech::new(
                    gcs_config
                        .max_await_time
                        .map(AwaitTime::new)
                        .flatten()
                        .unwrap_or_default(),
                    authenticator
                        .token(&["https://www.googleapis.com/auth/cloud-platform"])
                        .await?,
                ));

                handle_with_driver(database, config.message_timeout(), stream, driver).await?;
            }
            None => {
                error!("Server started with GCS driver but without GCS config. Disabling speech recognition for this session");
                handle_with_driver(
                    database,
                    config.message_timeout(),
                    stream,
                    DummyDriver::none(),
                )
                .await?;
            }
        },
        _ => {
            handle_with_driver(
                database,
                config.message_timeout(),
                stream,
                DummyDriver::none(),
            )
            .await?;
        }
    }

    Ok(())
}

async fn handle_with_driver<D>(
    database: Arc<HandlerDatabase>,
    max_time: Duration,
    mut stream: TcpStream,
    driver: Option<D>,
) -> Result<(), ServerError>
where
    D: RecognitionDriver<
            RecvStream<'static, Vec<u8>>,
            Item = Result<
                Option<String>,
                <D as RecognitionDriver<RecvStream<'static, Vec<u8>>>>::Error,
            >,
        > + 'static,
{
    let (mut read, mut write) = stream.split();

    AnonymousMessageHandler::new(database, MessageStream::new(&mut read), &mut write, driver)
        .listen(max_time)
        .await?;

    Ok(())
}
