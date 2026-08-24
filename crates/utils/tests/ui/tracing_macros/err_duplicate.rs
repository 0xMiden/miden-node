use miden_node_utils::tracing::miden_instrument;

#[miden_instrument(target = "test", name = "duplicate_err", err, err(fault_only))]
async fn duplicate_err() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
