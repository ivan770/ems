//! EMS (External Media Server) is an AudioSocket server, with support for
//! speech recognition, dynamic configuration exchange, audio playback and call management.

// We'll silence Clippy on this one because of tracing macro
#![allow(clippy::unit_arg)]
#![warn(missing_docs)]

use std::{convert::TryInto, sync::Arc};

use anyhow::Result;
use argh::from_env;
use config::{Cli, Config};
use db::HandlerDatabase;
use once_cell::sync::OnceCell;
use server::AudioSocketServer;
use tokio::join;
use tracing::subscriber::set_global_default;
use tracing_subscriber::{EnvFilter, FmtSubscriber};
use ws::WsServer;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

static CONFIG: OnceCell<Config> = OnceCell::new();

/// EMS configuration.
pub mod config;

/// Message handler database.
pub mod db;

/// Google Cloud Speech-to-Text module
#[cfg(feature = "gcs")]
mod gcs;

/// TCP server, that listens to incoming AudioSocket messages.
mod server;

/// AudioSocket message handler.
mod handler;

/// Speech-to-text interfaces.
mod recognition;

/// WebSocket server, that works as a bridge between EMS and clients.
mod ws;

/// AsyncRead wrapper for receiving messages.
pub mod stream;

#[tokio::main]
async fn main() -> Result<()> {
    let cli: Cli = from_env();

    CONFIG.set(cli.try_into()?).ok();

    set_global_default(
        FmtSubscriber::builder()
            .with_env_filter(EnvFilter::from_env("LOG_LEVEL"))
            .finish(),
    )?;

    let database = Arc::new(HandlerDatabase::default());

    let config = CONFIG.get().expect("Config was not set previously");

    join!(
        WsServer::new(config, database.clone()).listen(),
        AudioSocketServer::new(config, database).listen()
    )
    .1?;

    Ok(())
}
