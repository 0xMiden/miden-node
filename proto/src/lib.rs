use protox::prost::Message;
use tonic_build::FileDescriptorSet;

/// Returns the Protobuf file descriptor for the RPC API.
pub fn rpc_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "rpc_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the proving service API.
pub fn proving_service_api_descriptor() -> FileDescriptorSet {
    let bytes =
        include_bytes!(concat!(env!("OUT_DIR"), "/", "proving_service_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the store API.
#[cfg(feature = "internal")]
pub fn store_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "store_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the block-producer API.
#[cfg(feature = "internal")]
pub fn block_producer_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "block_producer_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the ntx-builder API.
#[cfg(feature = "internal")]
pub fn ntx_builder_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "ntx_builder_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the worker status API.
#[cfg(feature = "internal")]
pub fn worker_status_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "worker_status_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}
