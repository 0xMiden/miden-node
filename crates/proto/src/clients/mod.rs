//! gRPC client builder utilities for Miden node.
//!
//! This module provides a unified [`Builder`] pattern for creating various gRPC clients
//! with consistent configuration options including TLS, OTEL interceptors, and connection types.
//!
//! # Examples
//!
//! ```rust,no_run
//! use miden_node_proto::clients::{Builder, StoreNtxBuilderClient, StoreNtxBuilder};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Create a store client with OTEL and TLS
//! let client: StoreNtxBuilderClient = Builder::new()
//!     .with_address("https://store.example.com".to_string())
//!     .with_tls()
//!     .connect::<StoreNtxBuilder>()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::{
    Request, Status,
    metadata::AsciiMetadataValue,
    service::{Interceptor, interceptor::InterceptedService},
    transport::{Channel, Endpoint},
};

use crate::generated;

// METADATA INTERCEPTOR
// ================================================================================================

/// Interceptor designed to inject required metadata into all RPC requests.
#[derive(Default, Clone)]
pub struct MetadataInterceptor {
    metadata: HashMap<&'static str, AsciiMetadataValue>,
}

impl MetadataInterceptor {
    /// Adds or overwrites HTTP ACCEPT metadata to the interceptor.
    ///
    /// Provided version string must be ASCII.
    pub fn with_accept_metadata(mut self, version: &str) -> Result<Self, anyhow::Error> {
        let accept_value = format!("application/vnd.miden; version={version}");
        self.metadata.insert("accept", AsciiMetadataValue::try_from(accept_value)?);
        Ok(self)
    }
}

impl Interceptor for MetadataInterceptor {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        let mut request = request;
        for (key, value) in &self.metadata {
            request.metadata_mut().insert(*key, value.clone());
        }
        Ok(request)
    }
}

// TYPE ALIASES FOR INSTRUMENTED CLIENTS
// ================================================================================================

pub type RpcApiClient =
    generated::rpc::api_client::ApiClient<InterceptedService<Channel, MetadataInterceptor>>;
pub type BlockProducerApiClient =
    generated::block_producer::api_client::ApiClient<InterceptedService<Channel, OtelInterceptor>>;
pub type StoreNtxBuilderClient = generated::ntx_builder_store::ntx_builder_client::NtxBuilderClient<
    InterceptedService<Channel, OtelInterceptor>,
>;
pub type StoreBlockProducerClient =
    generated::block_producer_store::block_producer_client::BlockProducerClient<
        InterceptedService<Channel, OtelInterceptor>,
    >;
pub type StoreRpcClient =
    generated::rpc_store::rpc_client::RpcClient<InterceptedService<Channel, OtelInterceptor>>;

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
        Ok(T::with_interceptor(channel, &self))
    }

    pub fn connect_lazy<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let channel = self.build_endpoint()?.connect_lazy();
        Ok(T::with_interceptor(channel, &self))
    }

    fn build_endpoint(&self) -> Result<Endpoint> {
        let mut endpoint = Endpoint::from_shared(self.address.clone())
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

    fn with_interceptor(channel: Channel, builder: &Builder) -> Self::Service;
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
    type Service = RpcApiClient;

    fn with_interceptor(channel: Channel, builder: &Builder) -> Self::Service {
        // Use version from builder or default
        let version = builder.metadata_version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"));
        let interceptor = MetadataInterceptor::default()
            .with_accept_metadata(version)
            .expect("Failed to create metadata interceptor");

        generated::rpc::api_client::ApiClient::with_interceptor(channel, interceptor)
    }
}

impl GrpcClientBuilder for BlockProducer {
    type Service = BlockProducerApiClient;

    fn with_interceptor(channel: Channel, _builder: &Builder) -> Self::Service {
        generated::block_producer::api_client::ApiClient::with_interceptor(channel, OtelInterceptor)
    }
}

impl GrpcClientBuilder for StoreNtxBuilder {
    type Service = StoreNtxBuilderClient;

    fn with_interceptor(channel: Channel, _builder: &Builder) -> Self::Service {
        generated::ntx_builder_store::ntx_builder_client::NtxBuilderClient::with_interceptor(
            channel,
            OtelInterceptor,
        )
    }
}

impl GrpcClientBuilder for StoreBlockProducer {
    type Service = StoreBlockProducerClient;

    fn with_interceptor(channel: Channel, _builder: &Builder) -> Self::Service {
        generated::block_producer_store::block_producer_client::BlockProducerClient::with_interceptor(
            channel,
            OtelInterceptor,
        )
    }
}

impl GrpcClientBuilder for StoreRpc {
    type Service = StoreRpcClient;

    fn with_interceptor(channel: Channel, _builder: &Builder) -> Self::Service {
        generated::rpc_store::rpc_client::RpcClient::with_interceptor(channel, OtelInterceptor)
    }
}
