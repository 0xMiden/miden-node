use std::any::Any;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use http::{Response, StatusCode, header};
use http_body_util::Full;
pub use miden_node_panic_macro::handle_panic_fn;
pub use tower_http::catch_panic::CatchPanicLayer;

/// ...
pub async fn handle_panic<F, T, E>(future: F) -> anyhow::Result<Result<T, E>>
where
    F: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send,
{
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => Ok(result),
        Err(err) => {
            let err = stringify_panic_error(err);
            tracing::error!("panic occurred: {err}");
            Err(anyhow::anyhow!("panic occurred: {}", err))
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

pub fn stringify_panic_error(err: Box<dyn Any + Send + 'static>) -> String {
    if let Some(&msg) = err.downcast_ref::<&str>() {
        msg.to_string()
    } else if let Ok(msg) = err.downcast::<String>() {
        *msg
    } else {
        "unknown".to_string()
    }
}
