use std::time::Duration;

use crate::generated::rpc::api_client::ApiClient;
use anyhow::Context;
use tonic::{
    metadata::AsciiMetadataValue,
    service::{Interceptor, interceptor::InterceptedService},
    transport::Channel,
};
use url::Url;

/// Connects to the Miden node API using the provided URL and timeout.
///
/// The client is configured with an interceptor that sets the HTTP ACCEPT header
/// to the version of the Miden node.
pub async fn connect(
    url: Url,
    timeout_ms: u64,
) -> anyhow::Result<ApiClient<InterceptedService<Channel, AcceptHeaderInterceptor>>> {
    // Setup connection channel.
    let endpoint = tonic::transport::Endpoint::try_from(url.to_string())
        .context("Failed to parse node URL")?
        .timeout(Duration::from_millis(timeout_ms));
    let channel = endpoint.connect().await?;

    // Set up ACCEPT header interceptor.
    let version = env!("CARGO_PKG_VERSION");
    let accept_value = format!("application/vnd.miden.{version}+grpc");
    let interceptor = AcceptHeaderInterceptor::new(accept_value);

    // Return the connected client.
    Ok(ApiClient::with_interceptor(channel, interceptor))
}

/// Interceptor designed to inject the HTTP ACCEPT header
/// into all [`ApiClient`] requests.
pub struct AcceptHeaderInterceptor {
    value: String,
}

impl AcceptHeaderInterceptor {
    fn new(value: String) -> Self {
        Self { value }
    }
}

impl Interceptor for AcceptHeaderInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        let mut request = request;
        request
            .metadata_mut()
            .insert("accept", AsciiMetadataValue::try_from(self.value.clone()).unwrap());
        Ok(request)
    }
}
