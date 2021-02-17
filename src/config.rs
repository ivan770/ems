use std::{
    convert::TryFrom, fs::read, io::Error as IoError, net::SocketAddr, path::PathBuf,
    time::Duration,
};

use argh::FromArgs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml::{de::Error as TomlError, from_slice};

#[cfg(feature = "gcs")]
use crate::gcs::config::GoogleCloudSpeechConfig;
#[cfg(feature = "gctts")]
use crate::gctts::config::GoogleCloudTextToSpeechConfig;

const fn default_message_timeout() -> u64 {
    3
}

fn default_config_path() -> PathBuf {
    PathBuf::from("./ems.toml")
}

/// Preferred speech recognition driver.
#[derive(Serialize, Deserialize)]
pub enum SpeechRecognitionDriver {
    /// Google Cloud Speech driver
    #[cfg(feature = "gcs")]
    #[serde(rename = "google")]
    GoogleCloudSpeech,
}

/// Preferred speech synthesis driver.
#[derive(Serialize, Deserialize)]
pub enum SpeechSynthesisDriver {
    /// Google Cloud Speech driver
    #[cfg(feature = "gctts")]
    #[serde(rename = "google")]
    GoogleCloudTextToSpeech,
}

/// Errors, that may happen during configuration loading.
#[derive(Error, Debug)]
pub enum ConfigError {
    /// Config was not found using specified path (or using a default one).
    #[error("Config file ({0}) cannot be loaded: {1}")]
    ConfigNotFound(PathBuf, IoError),

    /// File by provided path does not contain a valid configuration.
    #[error("Malformed config file: {0}")]
    MalformedConfig(#[from] TomlError),
}

/// EMS (External Media Server) is an AudioSocket server created for speech recognition and processing.
#[derive(FromArgs)]
pub struct Cli {
    /// path to EMS configuration file
    #[argh(default = "default_config_path()", option)]
    config: PathBuf,
}

/// TOML server configuration.
#[derive(Serialize, Deserialize)]
pub struct Config {
    /// AudioSocket server address.
    audiosocket_addr: SocketAddr,

    /// WebSocket server address.
    websocket_addr: SocketAddr,

    /// Max amount of seconds to wait for another AudioSocket message.
    #[serde(default = "default_message_timeout")]
    message_timeout: u64,

    /// Recognition driver to be used.
    ///
    /// [`None`] by default.
    recognition_driver: Option<SpeechRecognitionDriver>,

    /// Speech synthesis driver to be used.
    ///
    /// [`None`] by default.
    synthesis_driver: Option<SpeechSynthesisDriver>,

    /// Google Cloud Speech configuration.
    #[cfg(feature = "gcs")]
    #[serde(rename = "gcs")]
    gcs_config: Option<GoogleCloudSpeechConfig>,

    #[cfg(feature = "gctts")]
    #[serde(rename = "gctts")]
    gctts_config: Option<GoogleCloudTextToSpeechConfig>,
}

// We can use sync FS API to load config, as there are no other tasks
// to block from executing.
impl TryFrom<Cli> for Config {
    type Error = ConfigError;

    fn try_from(value: Cli) -> Result<Self, Self::Error> {
        Ok(from_slice(&read(&value.config).map_err(|e| {
            ConfigError::ConfigNotFound(value.config, e)
        })?)?)
    }
}

impl Config {
    /// Get configured AudioSocket server address.
    pub fn audiosocket_addr(&self) -> &SocketAddr {
        &self.audiosocket_addr
    }

    /// Get configured WebSocket server address
    pub fn websocket_addr(&self) -> &SocketAddr {
        &self.websocket_addr
    }

    /// Get max await time for next AudioSocket message.
    pub fn message_timeout(&self) -> Duration {
        Duration::from_secs(self.message_timeout)
    }

    /// Get configured speech recognition driver.
    ///
    /// [`None`], if speech recognition is disabled.
    pub fn recognition_driver(&self) -> &Option<SpeechRecognitionDriver> {
        &self.recognition_driver
    }

    /// Get configured speech synthesis driver.
    ///
    /// [`None`], if speech synthesis is disabled.
    pub fn synthesis_driver(&self) -> &Option<SpeechSynthesisDriver> {
        &self.synthesis_driver
    }

    /// Get Google Cloud Speech configuration.
    ///
    /// Note that driver selection config, and the driver config itself are separate entities.
    /// It is a normal situation, where `google` driver was selected, but no `google` config was found.
    #[cfg(feature = "gcs")]
    pub fn gcs_config(&self) -> &Option<GoogleCloudSpeechConfig> {
        &self.gcs_config
    }

    /// Get Google Cloud Text-to-Speech configuration.
    #[cfg(feature = "gctts")]
    pub fn gctts_config(&self) -> &Option<GoogleCloudTextToSpeechConfig> {
        &self.gctts_config
    }
}
