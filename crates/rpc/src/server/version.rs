use std::task::{Context as StdContext, Poll};

use futures::{FutureExt, future::BoxFuture};
use http::header::ACCEPT;
use tower::{Layer, Service};
use tracing::debug;

use crate::COMPONENT;

#[derive(Clone)]
pub struct VersionLayer {}

impl VersionLayer {
    pub fn new() -> Self {
        VersionLayer {}
    }
}

impl<S> Layer<S> for VersionLayer {
    type Service = VersionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        VersionService { inner }
    }
}

#[derive(Clone)]
pub struct VersionService<S> {
    inner: S,
}

impl<S, B> Service<http::Request<B>> for VersionService<S>
where
    S: Service<http::Request<B>, Response = http::Response<B>> + Clone + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
    B: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut StdContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        // Check if ACCEPT header exists.
        if !request.headers().contains_key(ACCEPT) {
            debug!(target: COMPONENT, "Request missing ACCEPT header");

            // Create a response without calling the inner service
            let response = http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .header("content-type", "application/grpc")
                .header("grpc-status", "3") // INVALID_ARGUMENT
                .header("grpc-message", "Missing required ACCEPT header")
                .body(B::default())
                .unwrap();

            // Return a future that resolves immediately to this response
            return futures::future::ready(Ok(response)).boxed();
        }

        // TODO: check version

        self.inner.call(request).boxed()
    }
}
