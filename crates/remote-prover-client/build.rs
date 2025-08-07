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

const GENERATED_OUT_DIR: &str = "src/remote_prover/generated";

/// Generates Rust protobuf bindings.
fn main() -> miette::Result<()> {
    println!("cargo::rerun-if-env-changed=BUILD_PROTO");
    if !BUILD_GENERATED_FILES_IN_SRC {
        return Ok(());
    }

    let remote_prover_descriptor = remote_prover_api_descriptor();

    // Build std version
    let std_path = format!("{GENERATED_OUT_DIR}/std");
    build_tonic_from_descriptor(remote_prover_descriptor.clone(), std_path, true)?;

    // Build nostd version
    let nostd_path = format!("{GENERATED_OUT_DIR}/nostd");
    build_tonic_from_descriptor(remote_prover_descriptor, nostd_path.clone(), false)?;

    // Convert nostd version to use core/alloc instead of std
    let nostd_file_path = format!("{nostd_path}/remote_prover.rs");
    convert_to_nostd(&nostd_file_path)?;

    Ok(())
}

// HELPER FUNCTIONS
// ================================================================================================

/// Builds tonic code from a `FileDescriptorSet` with specified configuration
fn build_tonic_from_descriptor(
    descriptor: FileDescriptorSet,
    out_dir: String,
    build_transport: bool,
) -> miette::Result<()> {
    tonic_build::configure()
        .out_dir(out_dir)
        .build_server(false)
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
