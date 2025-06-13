use std::{fs, io::Write};

use miden_node_proto_build::proving_service_api_descriptor;
use miette::IntoDiagnostic;

/// Defines whether the build script should generate files in `/src`.
///
/// The docs.rs build pipeline has a read-only filesystem, so we have to avoid writing to `src`,
/// otherwise the docs will fail to build there. Note that writing to `OUT_DIR` is fine.
const BUILD_GENERATED_FILES_IN_SRC: bool = option_env!("BUILD_PROTO").is_some();

const GENERATED_OUT_DIR: &str = "src/proving_service/generated";

/// Generates Rust protobuf bindings.
fn main() -> miette::Result<()> {
    println!("cargo::rerun-if-env-changed=BUILD_PROTO");
    if !BUILD_GENERATED_FILES_IN_SRC {
        return Ok(());
    }

    let proving_service_descriptor = proving_service_api_descriptor();

    let std_path = format!("{GENERATED_OUT_DIR}/std");

    tonic_build::configure()
        .out_dir(std_path)
        .build_server(false)
        .build_transport(true)
        .compile_fds_with_config(prost_build::Config::new(), proving_service_descriptor.clone())
        .into_diagnostic()?;

    let nostd_path = format!("{GENERATED_OUT_DIR}/nostd");

    tonic_build::configure()
        .out_dir(nostd_path.clone())
        .build_server(false)
        .build_transport(false) // No transport for nostd
        .compile_fds_with_config(prost_build::Config::new(), proving_service_descriptor)
        .into_diagnostic()?;

    // Replace `std` references with `core` and `alloc` in `proving_service.rs`.
    // (Only for nostd version)
    let nostd_file_path = format!("{nostd_path}/proving_service.rs");
    let file_content = fs::read_to_string(&nostd_file_path).into_diagnostic()?;
    let updated_content = file_content
        .replace("std::result", "core::result")
        .replace("std::marker", "core::marker")
        .replace("format!", "alloc::format!");

    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&nostd_file_path)
        .into_diagnostic()?;

    file.write_all(updated_content.as_bytes()).into_diagnostic()?;
    Ok(())
}
