use std::{io::Error as IoError, sync::Arc};

use audiosocket::AudioSocketError;
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
    time::error::Elapsed,
};
use tracing::{error, info, instrument};

use crate::{
    config::Config, db::HandlerDatabase, handler::AnonymousMessageHandler, stream::MessageStream,
};

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("IO error: {0}")]
    IoError(#[from] IoError),

    #[error("AudioSocket error: {0}")]
    AudioSocketError(#[from] AudioSocketError),

    #[error("Exceeded max await time for next AudioSocket message: {0}")]
    TimeoutError(#[from] Elapsed),

    #[error("Connection was closed by client")]
    ClientDisconnected(Option<IoError>),
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
    mut stream: TcpStream,
) -> Result<(), ServerError> {
    let (mut read, mut write) = stream.split();

    AnonymousMessageHandler::new(config, database, MessageStream::new(&mut read), &mut write)
        .listen(config.message_timeout())
        .await
}
