/// GCS config
pub mod config;

/// Implementation of recognition driver for GCS
pub mod driver;

/// Recognition request
pub mod codegen {
    include!("./codegen/google.api.rs");
    include!("./codegen/google.cloud.speech.v1.rs");
    include!("./codegen/google.protobuf.rs");
}

pub mod longrunning {
    include!("./codegen/google.longrunning.rs");
}

pub mod rpc {
    include!("./codegen/google.rpc.rs");
}
