use miden_node_utils::tracing::miden_instrument;

#[miden_instrument(target = "test", name = "both_directives", err, grpc_err)]
async fn both_directives() -> Result<(), std::io::Error> {
    Ok(())
}

fn main() {}
