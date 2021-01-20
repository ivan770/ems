use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GoogleCloudSpeechConfig {
    /// Path to service credentials file.
    pub(crate) service_account_path: PathBuf,

    /// Max await time in seconds between AudioSocket audio messages.
    ///
    /// Valid values are `0..=10`.
    pub(crate) max_await_time: Option<u64>,
}
