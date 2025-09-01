use std::fs;
use std::io::Write;

use miden_node_proto_build::remote_prover_api_descriptor;
use miette::IntoDiagnostic;
use tonic_build::FileDescriptorSet;

/// Defines whether the build script should generate files in `/src`.
///
/// The docs.rs build pipeline has a read-only filesystem, so we have to avoid writing to `src`,
/// otherwise the docs will fail to build there. Note that writing to `OUT_DIR` is fine.
const BUILD_GENERATED_FILES_IN_SRC: bool = option_env!("BUILD_PROTO").is_some();

const CLIENT_STD_OUT_DIR: &str = "src/generated/client/std";
const CLIENT_NOSTD_OUT_DIR: &str = "src/generated/client/nostd";
const SERVER_OUT_DIR: &str = "src/generated/server";

/// Generates Rust protobuf bindings for both client and server.
fn main() -> miette::Result<()> {
    println!("cargo::rerun-if-env-changed=BUILD_PROTO");
    if !BUILD_GENERATED_FILES_IN_SRC {
        return Ok(());
    }

    let remote_prover_descriptor = remote_prover_api_descriptor();

    // Generate client code (both std and nostd variants) when client features are enabled
    if should_generate_client() {
        generate_client_code(remote_prover_descriptor.clone())?;
    }

    // Generate server code when server features are enabled
    if should_generate_server() {
        generate_server_code(remote_prover_descriptor)?;
    }

    Ok(())
}

// HELPER FUNCTIONS
// ================================================================================================

/// Determines if client code should be generated based on enabled features
fn should_generate_client() -> bool {
    cfg!(feature = "client")
}

/// Determines if server code should be generated based on enabled features
fn should_generate_server() -> bool {
    cfg!(feature = "server")
}

/// Generates client-side protobuf code with std and nostd variants
fn generate_client_code(descriptor: FileDescriptorSet) -> miette::Result<()> {
    // Create client directory structure
    fs::create_dir_all(CLIENT_STD_OUT_DIR).into_diagnostic()?;
    fs::create_dir_all(CLIENT_NOSTD_OUT_DIR).into_diagnostic()?;

    // Generate std version for client
    build_tonic_from_descriptor(
        descriptor.clone(),
        CLIENT_STD_OUT_DIR.to_string(),
        false, // build_server = false (client only)
        true,  // build_transport = true
    )?;

    // Generate nostd version for client
    build_tonic_from_descriptor(
        descriptor,
        CLIENT_NOSTD_OUT_DIR.to_string(),
        false, // build_server = false (client only)
        false, // build_transport = false (no transport for nostd)
    )?;

    // Convert nostd version to use core/alloc
    let nostd_file_path = format!("{CLIENT_NOSTD_OUT_DIR}/remote_prover.rs");
    convert_to_nostd(&nostd_file_path)?;

    Ok(())
}

/// Generates server-side protobuf code
fn generate_server_code(descriptor: FileDescriptorSet) -> miette::Result<()> {
    // Create server directory structure
    fs::create_dir_all(SERVER_OUT_DIR).into_diagnostic()?;

    build_tonic_from_descriptor(
        descriptor,
        SERVER_OUT_DIR.to_string(),
        true, // build_server = true
        true, // build_transport = true
    )?;

    Ok(())
}

/// Builds tonic code from a `FileDescriptorSet` with specified configuration
fn build_tonic_from_descriptor(
    descriptor: FileDescriptorSet,
    out_dir: String,
    build_server: bool,
    build_transport: bool,
) -> miette::Result<()> {
    tonic_build::configure()
        .out_dir(out_dir)
        .build_server(build_server)
        .build_client(true) // Always build client code
        .build_transport(build_transport)
        .compile_fds_with_config(prost_build::Config::new(), descriptor)
        .into_diagnostic()
}

/// Replaces std references with core and alloc for nostd compatibility
fn convert_to_nostd(file_path: &str) -> miette::Result<()> {
    let file_content = fs::read_to_string(file_path).into_diagnostic()?;
    let updated_content = file_content
        .replace("std::result", "core::result")
        .replace("std::marker", "core::marker")
        .replace("format!", "alloc::format!");

    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(file_path)
        .into_diagnostic()?;

    file.write_all(updated_content.as_bytes()).into_diagnostic()
}
