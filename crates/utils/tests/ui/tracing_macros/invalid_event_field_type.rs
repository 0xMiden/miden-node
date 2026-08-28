use miden_node_utils::tracing::debug;

fn main() {
    debug!("invalid.field.type", account.id = 42_u32);
}
