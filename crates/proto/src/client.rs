use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use anyhow::Context;
use tonic::{
    metadata::AsciiMetadataValue,
    service::{Interceptor, interceptor::InterceptedService},
    transport::Channel,
};
use url::Url;

use crate::generated::rpc::api_client::ApiClient;

// RPC CLIENT
// ================================================================================================

/// Alias for gRPC client that wraps the underlying gRPC client for the purposes of configuration.
type InnerClient = ApiClient<InterceptedService<Channel, AcceptHeaderInterceptor>>;

/// Client for the Miden RPC API which is fully configured to communicate with a Miden node.
pub struct RpcClient(InnerClient);

impl Deref for RpcClient {
    type Target = InnerClient;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RpcClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl RpcClient {
    /// Connects to the Miden node API using the provided URL and timeout.
    ///
    /// The client is configured with an interceptor that sets the HTTP ACCEPT header to the version of
    /// the Miden node.
    pub async fn connect(url: &Url, timeout_ms: u64) -> anyhow::Result<RpcClient> {
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
        Ok(RpcClient(ApiClient::with_interceptor(channel, interceptor)))
    }
}

// ACCEPT INTERCEPTOR
// ================================================================================================

/// Interceptor designed to inject the HTTP ACCEPT header into all [`ApiClient`] requests.
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
