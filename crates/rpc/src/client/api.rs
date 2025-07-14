use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use anyhow::Context;
use miden_node_proto::{
    clients::{Builder, GrpcClientBuilder},
    generated::rpc::api_client::ApiClient as ProtoClient,
};
use tonic::{service::interceptor::InterceptedService, transport::Channel};
use url::Url;

use super::MetadataInterceptor;

/// Wrapper for gRPC client that wraps the underlying client for the purposes of metadata
/// configuration and builder pattern support.
#[derive(Clone, Debug)]
pub struct RpcInnerClient(ProtoClient<InterceptedService<Channel, MetadataInterceptor>>);

impl Deref for RpcInnerClient {
    type Target = ProtoClient<InterceptedService<Channel, MetadataInterceptor>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RpcInnerClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Client for the Miden RPC API which is fully configured to communicate with a Miden node.
pub struct ApiClient(RpcInnerClient);

// Implement GrpcClientBuilder for the RpcInnerClient type
impl GrpcClientBuilder for RpcInnerClient {
    type Service = Self;

    fn with_interceptor(channel: Channel, builder: &Builder) -> Self::Service {
        // Use version from builder or default
        let version = builder.metadata_version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"));
        let interceptor = MetadataInterceptor::default()
            .with_accept_metadata(version)
            .expect("Failed to create metadata interceptor");

        Self(ProtoClient::with_interceptor(channel, interceptor))
    }
}

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
        let client: RpcInnerClient = Builder::new()
            .with_address(url.to_string())
            .with_tls()
            .with_timeout(timeout)
            .with_metadata_version(version.unwrap_or(env!("CARGO_PKG_VERSION")).to_string())
            .connect::<RpcInnerClient>()
            .await
            .context("Failed to create gRPC client")?;

        Ok(ApiClient(client))
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
        let client: RpcInnerClient = Builder::new()
            .with_address(url.to_string())
            .with_tls()
            .with_timeout(timeout)
            .with_metadata_version(version.unwrap_or(env!("CARGO_PKG_VERSION")).to_string())
            .connect_lazy::<RpcInnerClient>()
            .context("Failed to create gRPC client")?;

        Ok(ApiClient(client))
    }
}

impl Deref for ApiClient {
    type Target = RpcInnerClient;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ApiClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
