use std::{
    collections::HashMap,
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

/// Alias for gRPC client that wraps the underlying client for the purposes of metadata
/// configuration.
type InnerClient = ApiClient<InterceptedService<Channel, MetadataInterceptor>>;

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
    /// The client is configured with an interceptor that sets all requisite request metadata.
    pub async fn connect(url: &Url, timeout: Duration) -> anyhow::Result<RpcClient> {
        // Setup connection channel.
        let endpoint = tonic::transport::Endpoint::try_from(url.to_string())
            .context("Failed to parse node URL")?
            .timeout(timeout);
        let channel = endpoint.connect().await?;

        // Set up the accept metadata interceptor.
        let version = env!("CARGO_PKG_VERSION");
        let accept_value = format!("application/vnd.miden.{version}+grpc");
        let interceptor = MetadataInterceptor::default().with_metadata("accept", accept_value)?;

        // Return the connected client.
        Ok(RpcClient(ApiClient::with_interceptor(channel, interceptor)))
    }
}

// INTERCEPTOR
// ================================================================================================

/// Interceptor designed to inject required metadata into all [`ApiClient`] requests.
#[derive(Default)]
pub struct MetadataInterceptor {
    metadata: HashMap<&'static str, AsciiMetadataValue>,
}

impl MetadataInterceptor {
    /// Adds or overwrites metadata to the interceptor.
    pub fn with_metadata(
        mut self,
        key: &'static str,
        value: String,
    ) -> Result<Self, anyhow::Error> {
        self.metadata.insert(key, AsciiMetadataValue::try_from(value)?);
        Ok(self)
    }
}

impl Interceptor for MetadataInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        let mut request = request;
        for (key, value) in &self.metadata {
            request.metadata_mut().insert(*key, value.clone());
        }
        Ok(request)
    }
}
