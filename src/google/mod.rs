// Hide missing documentation warning for codegen structs
#![allow(missing_docs)]

/// OAuth authentication scope for GCS and GCTTS.
pub const AUTH_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// TLS certificates.
pub const CERTS: &[u8] = include_bytes!("../google/cert.pem");

/// Generated APIs for GCS and GCTTS.
pub mod codegen {
    include!("./codegen/google.api.rs");
    include!("./codegen/google.cloud.speech.v1.rs");
    include!("./codegen/google.cloud.texttospeech.v1.rs");
    include!("./codegen/google.protobuf.rs");
}

/// Generated longrunning requests.
pub mod longrunning {
    include!("./codegen/google.longrunning.rs");
}

/// Generated RPC structs.
pub mod rpc {
    include!("./codegen/google.rpc.rs");
}
