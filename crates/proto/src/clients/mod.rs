//! gRPC client builder utilities for Miden node.
//!
//! This module provides a unified [`Builder`] pattern for creating various gRPC clients
//! with consistent configuration options including TLS, OTEL interceptors, and connection types.
//!
//! # Examples
//!
//! ```rust,no_run
//! use miden_node_proto::clients::{Builder, GrpcClientBuilder, StoreNtxBuilderClient};
//! use miden_node_utils::tracing::grpc::OtelInterceptor;
//! use tonic::service::interceptor::InterceptedService;
//! use tonic::transport::Channel;
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Create a store client with OTEL and TLS
//! let client: StoreNtxBuilderClient<InterceptedService<Channel, OtelInterceptor>> = Builder::new()
//!     .with_address("https://store.example.com".to_string())
//!     .with_tls()
//!     .build()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use anyhow::{Context, Result};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::transport::{Channel, Endpoint};

// RE-EXPORTS FOR CONVENIENCE
// ================================================================================================
pub use crate::generated::{
    block_producer::api_client::ApiClient as BlockProducerApiClient,
    rpc::{api_client::ApiClient as RpcApiClient, api_server::Api},
    store::{
        block_producer_client::BlockProducerClient as StoreBlockProducerClient,
        ntx_builder_client::NtxBuilderClient as StoreNtxBuilderClient,
        rpc_client::RpcClient as StoreRpcClient,
    },
};

// BUILDER CONFIGURATION
// ================================================================================================

#[derive(Default, Clone)]
pub struct Builder {
    pub with_tls: bool,
    pub with_lazy_connection: bool,
    pub address: String,
    pub with_timeout: Option<Duration>,
    pub metadata_version: Option<String>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_tls(mut self) -> Self {
        self.with_tls = true;
        self
    }

    #[must_use]
    pub fn with_lazy_connection(mut self) -> Self {
        self.with_lazy_connection = true;
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.with_timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn with_metadata_version(mut self, version: String) -> Self {
        self.metadata_version = Some(version);
        self
    }

    #[must_use]
    pub fn with_address(mut self, address: String) -> Self {
        self.address = address;
        self
    }

    pub async fn build<T>(self) -> Result<T>
    where
        T: GrpcClientBuilder,
    {
        T::from_builder(self).await
    }
}

// GRPC CLIENT BUILDER TRAIT
// ================================================================================================

/// Trait for building gRPC clients from a common [`Builder`] configuration.
///
/// This trait provides a standardized way to create different gRPC clients with
/// consistent configuration options like TLS, OTEL interceptors, and connection types.
pub trait GrpcClientBuilder: Sized {
    /// Creates a new client instance from the builder configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the client cannot be created due to invalid configuration,
    /// network issues, or other initialization problems.
    #[allow(async_fn_in_trait)]
    async fn from_builder(builder: Builder) -> Result<Self>;
}

// IMPLEMENTATIONS
// ================================================================================================

// Note: This implementation always uses OtelInterceptor, matching existing patterns
impl GrpcClientBuilder
    for RpcApiClient<tonic::service::interceptor::InterceptedService<Channel, OtelInterceptor>>
{
    async fn from_builder(builder: Builder) -> Result<Self> {
        let mut endpoint = Endpoint::from_shared(builder.address)
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

        let client = RpcApiClient::with_interceptor(channel, OtelInterceptor);
        Ok(client)
    }
}

// Note: All implementations use OtelInterceptor by default, matching existing patterns
impl GrpcClientBuilder
    for BlockProducerApiClient<
        tonic::service::interceptor::InterceptedService<Channel, OtelInterceptor>,
    >
{
    async fn from_builder(builder: Builder) -> Result<Self> {
        let mut endpoint = Endpoint::from_shared(builder.address)
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

        let client = BlockProducerApiClient::with_interceptor(channel, OtelInterceptor);
        Ok(client)
    }
}

impl GrpcClientBuilder
    for StoreNtxBuilderClient<
        tonic::service::interceptor::InterceptedService<Channel, OtelInterceptor>,
    >
{
    async fn from_builder(builder: Builder) -> Result<Self> {
        let mut endpoint = Endpoint::from_shared(builder.address)
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

        let client = StoreNtxBuilderClient::with_interceptor(channel, OtelInterceptor);
        Ok(client)
    }
}

impl GrpcClientBuilder
    for StoreBlockProducerClient<
        tonic::service::interceptor::InterceptedService<Channel, OtelInterceptor>,
    >
{
    async fn from_builder(builder: Builder) -> Result<Self> {
        let mut endpoint = Endpoint::from_shared(builder.address)
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

        let client = StoreBlockProducerClient::with_interceptor(channel, OtelInterceptor);
        Ok(client)
    }
}

impl GrpcClientBuilder
    for StoreRpcClient<tonic::service::interceptor::InterceptedService<Channel, OtelInterceptor>>
{
    async fn from_builder(builder: Builder) -> Result<Self> {
        let mut endpoint = Endpoint::from_shared(builder.address)
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

        let client = StoreRpcClient::with_interceptor(channel, OtelInterceptor);
        Ok(client)
    }
}
