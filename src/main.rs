//! EMS (External Media Server) is an AudioSocket server, with support for
//! speech recognition, dynamic configuration exchange, audio playback and call management.

// Return Future from traits
#![feature(type_alias_impl_trait)]
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
use shutdown::Shutdown;
use tokio::{select, signal::ctrl_c};
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

/// Google-specific generated structs and keys (for both TTS and STT)
#[cfg(any(feature = "gcs", feature = "gctts"))]
pub mod google;

/// Google Cloud Speech-to-Text module
#[cfg(feature = "gcs")]
mod gcs;

/// Google Cloud Text-to-Speech module
#[cfg(feature = "gctts")]
mod gctts;

/// TCP server, that listens to incoming AudioSocket messages.
mod server;

/// AudioSocket message handler.
mod handler;

/// Speech-to-text interfaces.
mod recognition;

/// WebSocket server, that works as a bridge between EMS and clients.
mod ws;

/// Graceful shutdown for calls
mod shutdown;

/// Various services, including speech recognition and voice synthesis.
mod service;

/// Text-to-speech interfaces.
mod synthesis;

/// `AsyncRead` wrapper for receiving messages.
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

    select! {
        audiosocket_res = AudioSocketServer::new(config, database.clone()).listen() => audiosocket_res?,
        ws_res = WsServer::new(config, database.clone()).listen() => ws_res?,
        signal_res = ctrl_c() => signal_res?
    };

    Shutdown::from(database.as_ref()).await;

    Ok(())
}
