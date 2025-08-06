use std::ops::{Deref, DerefMut};
use std::time::Duration;

use anyhow::Context;
use miden_node_proto::generated::rpc::api_client::ApiClient as ProtoClient;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use url::Url;

use super::MetadataInterceptor;

/// Alias for gRPC client that wraps the underlying client for the purposes of metadata
/// configuration.
type InnerClient = ProtoClient<InterceptedService<Channel, MetadataInterceptor>>;

/// Client for the Miden RPC API which is fully configured to communicate with a Miden node.
pub struct ApiClient(InnerClient);

impl ApiClient {
    /// Connects to the Miden node API using the provided URL and timeout.
    ///
    /// The client is configured with an interceptor that sets all requisite request metadata.
    ///
    /// If a version is not specified, the version found in the `Cargo.toml` of the workspace is
    /// used.
    pub async fn connect(
        url: &Url,
        timeout: Duration,
        version: Option<&'static str>,
    ) -> anyhow::Result<ApiClient> {
        // Setup connection channel.
        let endpoint = tonic::transport::Endpoint::try_from(url.to_string())
            .context("Failed to parse node URL")?
            .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
            .context("Failed to enable TLS")?
            .timeout(timeout);
        let channel = endpoint.connect().await?;

        // Set up the accept metadata interceptor.
        let version = version.unwrap_or(env!("CARGO_PKG_VERSION"));
        let interceptor = MetadataInterceptor::default().with_accept_metadata(version)?;

        // Return the connected client.
        Ok(ApiClient(ProtoClient::with_interceptor(channel, interceptor)))
    }

    /// Connects to the Miden node API using the provided URL and timeout.
    ///
    /// The connection is lazy and will re-establish in the background on disconnection.
    ///
    /// The client is configured with an interceptor that sets all requisite request metadata.
    ///
    /// If a version is not specified, the version found in the `Cargo.toml` of the workspace is
    /// used.
    pub fn connect_lazy(
        url: &Url,
        timeout: Duration,
        version: Option<&'static str>,
    ) -> anyhow::Result<ApiClient> {
        let endpoint = tonic::transport::Endpoint::try_from(url.to_string())
            .context("Failed to parse node URL")?
            .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
            .context("Failed to enable TLS")?
            .timeout(timeout);

        let channel = endpoint.connect_lazy();

        // Set up the accept metadata interceptor.
        let version = version.unwrap_or(env!("CARGO_PKG_VERSION"));
        let interceptor = MetadataInterceptor::default().with_accept_metadata(version)?;

        Ok(ApiClient(ProtoClient::with_interceptor(channel, interceptor)))
    }
}

impl Deref for ApiClient {
    type Target = InnerClient;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ApiClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
