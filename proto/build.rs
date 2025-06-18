use std::{env, fs, path::PathBuf};

use anyhow::Context;
use prost::Message;

const RPC_PROTO: &str = "rpc.proto";
const STORE_PROTO: &str = "store.proto";
const BLOCK_PRODUCER_PROTO: &str = "block_producer.proto";

const RPC_DESCRIPTOR: &str = "rpc_file_descriptor.bin";
const STORE_DESCRIPTOR: &str = "store_file_descriptor.bin";
const BLOCK_PRODUCER_DESCRIPTOR: &str = "block_producer_file_descriptor.bin";

/// Generates Rust protobuf bindings from .proto files.
///
/// This is done only if `BUILD_PROTO` environment variable is set to `1` to avoid running the
/// script on crates.io where repo-level .proto files are not available.
fn main() -> anyhow::Result<()> {
    println!("cargo::rerun-if-changed=../proto");
    println!("cargo::rerun-if-env-changed=BUILD_PROTO");

    let out = env::var("OUT_DIR").context("env::OUT_DIR not set")?;

    let crate_root: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    let proto_dir = crate_root.join("proto");
    let includes = &[proto_dir];

    let rpc_file_descriptor = protox::compile([RPC_PROTO], includes)?;
    let rpc_path = PathBuf::from(&out).join(RPC_DESCRIPTOR);
    fs::write(&rpc_path, rpc_file_descriptor.encode_to_vec())
        .context("writing rpc file descriptor")?;

    let store_file_descriptor = protox::compile([STORE_PROTO], includes)?;
    let store_path = PathBuf::from(&out).join(STORE_DESCRIPTOR);
    fs::write(&store_path, store_file_descriptor.encode_to_vec())
        .context("writing store file descriptor")?;

    let block_producer_file_descriptor = protox::compile([BLOCK_PRODUCER_PROTO], includes)?;
    let block_producer_path = PathBuf::from(&out).join(BLOCK_PRODUCER_DESCRIPTOR);
    fs::write(&block_producer_path, block_producer_file_descriptor.encode_to_vec())
        .context("writing block producer file descriptor")?;

    Ok(())
}
