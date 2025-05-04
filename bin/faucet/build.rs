use std::str::FromStr;

/// Embeds static faucet website files and generates build metadata for --version.
fn main() {
    // The location of our static faucet website files.
    let static_dir = std::path::PathBuf::from_str(std::env!("CARGO_MANIFEST_DIR"))
        .unwrap()
        .join("src")
        .join("server")
        .join("resources");
    println!("cargo::rerun-if-changed={}", static_dir.to_str().expect("Valid utf-8"));
    // This makes the static files available as an embedded resource.
    static_files::resource_dir(static_dir).build().expect("Resources should build");

    // Configures environment variables for build metadata intended for extended version
    // information.
    if let Err(e) = miden_node_utils::version::vergen() {
        // Don't let an error here bring down the build. Build metadata will be empty which isn't a
        // critical failure.
        println!("cargo:warning=Failed to embed build metadata: {e:?}");
    }
}
