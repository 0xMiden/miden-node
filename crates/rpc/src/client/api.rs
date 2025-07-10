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
    async fn from_builder(builder: Builder) -> anyhow::Result<Self> {
        let mut endpoint = tonic::transport::Endpoint::from_shared(builder.address)
            .context("Failed to create endpoint from address")?;

        if let Some(timeout) = builder.with_timeout {
            endpoint = endpoint.timeout(timeout);
        }

        if builder.with_tls {
            endpoint = endpoint
                .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
                .context("Failed to configure TLS")?;
        }

        let channel = if builder.with_lazy_connection {
            endpoint.connect_lazy()
        } else {
            endpoint.connect().await.context("Failed to connect to endpoint")?
        };

        // Create MetadataInterceptor with version from builder or default
        let version = builder.metadata_version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"));
        let interceptor = MetadataInterceptor::default().with_accept_metadata(version)?;

        Ok(RpcInnerClient(ProtoClient::with_interceptor(channel, interceptor)))
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
            .build()
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
    pub async fn connect_lazy(
        url: &Url,
        timeout: Duration,
        version: Option<&'static str>,
    ) -> anyhow::Result<ApiClient> {
        let client: RpcInnerClient = Builder::new()
            .with_address(url.to_string())
            .with_tls()
            .with_timeout(timeout)
            .with_lazy_connection()
            .with_metadata_version(version.unwrap_or(env!("CARGO_PKG_VERSION")).to_string())
            .build()
            .await
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
