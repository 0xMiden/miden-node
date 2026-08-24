use miden_node_utils::tracing::miden_instrument;

#[miden_instrument(target = "test", name = "invalid_level", err(fault_only, level = "loud"))]
async fn invalid_level() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
