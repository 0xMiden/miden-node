fn main() {
    // Configures environment variables for build metadata intended for extended version
    // information.
    if let Err(e) = miden_node_utils::version::vergen() {
        // Don't let an error here bring down the build. Build metadata will be empty which isn't a
        // critical failure.
        println!("cargo:warning=Failed to embed build metadata: {e:?}");
    }
}
