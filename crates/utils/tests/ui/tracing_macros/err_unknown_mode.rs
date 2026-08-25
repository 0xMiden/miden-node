use miden_node_utils::tracing::miden_instrument;

#[miden_instrument(target = "test", name = "unknown_mode", err(faultonly))]
async fn unknown_mode() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
