use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GoogleCloudTextToSpeechConfig {
    /// Path to service credentials file.
    pub(crate) service_account_path: PathBuf,
}
