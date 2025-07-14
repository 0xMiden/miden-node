//! gRPC client builder utilities for Miden node.
//!
//! This module provides a unified [`Builder`] pattern for creating various gRPC clients
//! with consistent configuration options including TLS, OTEL interceptors, and connection types.
//!
//! # Examples
//!
//! ```rust,no_run
//! use miden_node_proto::clients::{Builder, InstrumentedStoreNtxBuilderClient, StoreNtxBuilder};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Create a store client with OTEL and TLS
//! let client: InstrumentedStoreNtxBuilderClient = Builder::new()
//!     .with_address("https://store.example.com".to_string())
//!     .with_tls()
//!     .connect::<StoreNtxBuilder>()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use anyhow::{Context, Result};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::{
    service::interceptor::InterceptedService,
    transport::{Channel, Endpoint},
};

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

// TYPE ALIASES FOR INSTRUMENTED CLIENTS
// ================================================================================================

pub type InstrumentedRpcApiClient = RpcApiClient<InterceptedService<Channel, OtelInterceptor>>;
pub type InstrumentedBlockProducerApiClient =
    BlockProducerApiClient<InterceptedService<Channel, OtelInterceptor>>;
pub type InstrumentedStoreNtxBuilderClient =
    StoreNtxBuilderClient<InterceptedService<Channel, OtelInterceptor>>;
pub type InstrumentedStoreBlockProducerClient =
    StoreBlockProducerClient<InterceptedService<Channel, OtelInterceptor>>;
pub type InstrumentedStoreRpcClient = StoreRpcClient<InterceptedService<Channel, OtelInterceptor>>;

// BUILDER CONFIGURATION
// ================================================================================================

#[derive(Default, Clone)]
pub struct Builder {
    pub with_tls: bool,
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

    pub async fn connect<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let channel = self.build_endpoint()?.connect().await?;
        Ok(T::with_interceptor(channel))
    }

    pub fn connect_lazy<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let channel = self.build_endpoint()?.connect_lazy();
        Ok(T::with_interceptor(channel))
    }

    fn build_endpoint(self) -> Result<Endpoint> {
        let mut endpoint = Endpoint::from_shared(self.address)
            .context("Failed to create endpoint from address")?;

        if let Some(timeout) = self.with_timeout {
            endpoint = endpoint.timeout(timeout);
        }

        if self.with_tls {
            endpoint = endpoint
                .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
                .context("Failed to configure TLS")?;
        }

        Ok(endpoint)
    }
}

// GRPC CLIENT BUILDER TRAIT
// ================================================================================================

/// Trait for building gRPC clients from a common [`Builder`] configuration.
///
/// This trait provides a standardized way to create different gRPC clients with
/// consistent configuration options like TLS, OTEL interceptors, and connection types.
pub trait GrpcClientBuilder {
    type Service;

    fn with_interceptor(channel: Channel) -> Self::Service;
}

// CLIENT BUILDER MARKERS
// ================================================================================================

#[derive(Copy, Clone, Debug)]
pub struct Rpc;

#[derive(Copy, Clone, Debug)]
pub struct BlockProducer;

#[derive(Copy, Clone, Debug)]
pub struct StoreNtxBuilder;

#[derive(Copy, Clone, Debug)]
pub struct StoreBlockProducer;

#[derive(Copy, Clone, Debug)]
pub struct StoreRpc;

// CLIENT BUILDER IMPLEMENTATIONS
// ================================================================================================

impl GrpcClientBuilder for Rpc {
    type Service = InstrumentedRpcApiClient;

    fn with_interceptor(channel: Channel) -> Self::Service {
        RpcApiClient::with_interceptor(channel, OtelInterceptor)
    }
}

impl GrpcClientBuilder for BlockProducer {
    type Service = InstrumentedBlockProducerApiClient;

    fn with_interceptor(channel: Channel) -> Self::Service {
        BlockProducerApiClient::with_interceptor(channel, OtelInterceptor)
    }
}

impl GrpcClientBuilder for StoreNtxBuilder {
    type Service = InstrumentedStoreNtxBuilderClient;

    fn with_interceptor(channel: Channel) -> Self::Service {
        StoreNtxBuilderClient::with_interceptor(channel, OtelInterceptor)
    }
}

impl GrpcClientBuilder for StoreBlockProducer {
    type Service = InstrumentedStoreBlockProducerClient;

    fn with_interceptor(channel: Channel) -> Self::Service {
        StoreBlockProducerClient::with_interceptor(channel, OtelInterceptor)
    }
}

impl GrpcClientBuilder for StoreRpc {
    type Service = InstrumentedStoreRpcClient;

    fn with_interceptor(channel: Channel) -> Self::Service {
        StoreRpcClient::with_interceptor(channel, OtelInterceptor)
    }
}
