pub mod grpc;
mod span_ext;

pub use grpc::{GrpcFault, is_server_fault_code, record_grpc_error};
pub use miden_node_tracing_macro::{miden_instrument, miden_span_record};
pub use span_ext::ErrorSpanExt;
