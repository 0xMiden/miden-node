use miden_node_utils::tracing::miden_instrument;

struct UnapprovedAttribute;

#[miden_instrument(fields(transaction.id = UnapprovedAttribute))]
fn records_unapproved_attribute() {}

fn main() {}
