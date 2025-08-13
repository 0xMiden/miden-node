use std::any::Any;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use http::{Response, StatusCode, header};
use http_body_util::Full;
pub use miden_node_panic_macro::main;
pub use tower_http::catch_panic::CatchPanicLayer;

/// Wraps a future and handles any panics that occur during its execution.
///
/// Returns a result containing the future's output or an error if a panic occurred. Should be used
/// through the associated procedural macro [`miden_node_panic_macro::main`].
pub async fn handle_panic<F, T, E>(future: F) -> anyhow::Result<Result<T, E>>
where
    F: Future<Output = Result<T, E>>,
{
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => Ok(result),
        Err(err) => {
            let err = stringify_panic_error(err);
            tracing::error!("panic: {err}");
            Err(anyhow::anyhow!("panic: {err}"))
        },
    }
}

/// Custom callback that is used by Tower to fulfill the
/// [`tower_http::catch_panic::ResponseForPanic`] trait.
///
/// This should be added to tonic server builder as a layer via [`CatchPanicLayer::custom()`].
pub fn catch_panic_layer_fn(err: Box<dyn Any + Send + 'static>) -> Response<Full<bytes::Bytes>> {
    // Log the panic error details.
    let err = stringify_panic_error(err);
    tracing::error!("panic occurred: {err}");

    // Return generic error response.
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/grpc")
        .body(Full::from(""))
        .unwrap()
}

/// Converts a dynamic panic-related error into a string.
fn stringify_panic_error(err: Box<dyn Any + Send + 'static>) -> String {
    if let Some(&msg) = err.downcast_ref::<&str>() {
        msg.to_string()
    } else if let Ok(msg) = err.downcast::<String>() {
        *msg
    } else {
        "unknown".to_string()
    }
}
