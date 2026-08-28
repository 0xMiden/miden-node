use miden_node_utils::tracing::miden_instrument;

#[miden_instrument(fields(transaction.id = ?"0x1234"))]
fn selects_instrument_formatter() {}

fn main() {}
