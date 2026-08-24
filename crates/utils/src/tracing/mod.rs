mod attribute;
pub mod grpc;
mod span_ext;

#[doc(hidden)]
pub use attribute::field_name_allowed;
pub use attribute::{RecordAttribute, record_attribute};
pub use miden_node_tracing_macro::{miden_instrument, miden_span_record};
pub use span_ext::ErrorSpanExt;
