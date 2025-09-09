use std::pin::Pin;
use std::task::{Context as StdContext, Poll};

use tonic::body::Body;
use tower::{Layer, Service};

/// Simple health check layer that intercepts requests to root path.
///
/// The root path is used by load-balancers and options requests to check the health
/// of the server. Since our gRPC server doesn't serve anything on the root
/// these get logged as errors. This layer instead intercepts these requests
/// and returns `Ok(200)`, preventing the errors.
#[derive(Clone)]
pub struct HealthCheckLayer;

impl<S> Layer<S> for HealthCheckLayer {
    type Service = HealthCheckService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HealthCheckService { inner: service }
    }
}

#[derive(Clone)]
pub struct HealthCheckService<S> {
    inner: S,
}

impl<S> Service<http::Request<Body>> for HealthCheckService<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut StdContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        if req.uri().path() == "/" {
            let response = http::Response::builder()
                .status(http::StatusCode::OK)
                .body(Body::empty())
                .expect("valid empty 200 response");
            Box::pin(async move { Ok(response) })
        } else {
            Box::pin(self.inner.call(req))
        }
    }
}
