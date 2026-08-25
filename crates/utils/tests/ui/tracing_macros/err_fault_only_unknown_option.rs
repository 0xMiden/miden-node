use miden_node_utils::tracing::miden_instrument;

#[miden_instrument(target = "test", name = "unknown_option", err(fault_only, Debug))]
async fn unknown_option() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
