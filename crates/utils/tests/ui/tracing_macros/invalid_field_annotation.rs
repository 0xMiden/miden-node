use miden_node_utils::tracing::{miden_instrument, miden_span_record};

#[miden_instrument]
fn records_invalid_annotation() {
    miden_span_record!(custom.attribute = 1 #[unchecked]);
}

fn main() {}
