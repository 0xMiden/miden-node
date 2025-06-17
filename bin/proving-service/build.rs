use miden_node_proto_build::{proving_service_api_descriptor, worker_status_api_descriptor};
use miette::IntoDiagnostic;
use tonic_build::FileDescriptorSet;

/// Defines whether the build script should generate files in `/src`.
///
/// The docs.rs build pipeline has a read-only filesystem, so we have to avoid writing to `src`,
/// otherwise the docs will fail to build there. Note that writing to `OUT_DIR` is fine.
const BUILD_GENERATED_FILES_IN_SRC: bool = option_env!("BUILD_PROTO").is_some();

const GENERATED_OUT_DIR: &str = "src/generated";

/// Generates Rust protobuf bindings.
fn main() -> miette::Result<()> {
    println!("cargo::rerun-if-env-changed=BUILD_PROTO");
    if !BUILD_GENERATED_FILES_IN_SRC {
        return Ok(());
    }

    // Get both file descriptor sets
    let worker_status_descriptor = worker_status_api_descriptor();
    let proving_service_descriptor = proving_service_api_descriptor();

    // Build tonic code for each descriptor
    build_tonic_from_descriptor(worker_status_descriptor)?;
    build_tonic_from_descriptor(proving_service_descriptor)?;

    Ok(())
}

// HELPER FUNCTIONS
// ================================================================================================

/// Builds tonic code from a `FileDescriptorSet`
fn build_tonic_from_descriptor(descriptor: FileDescriptorSet) -> miette::Result<()> {
    tonic_build::configure()
        .out_dir(GENERATED_OUT_DIR)
        .build_server(true) // this setting generates only the client side of the rpc api
        .build_transport(true)
        .compile_fds_with_config(prost_build::Config::new(), descriptor)
        .into_diagnostic()
}
