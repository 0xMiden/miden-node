use miden_node_utils::tracing::error;

fn main() {
    error!("not an error", "test.exception");
}
