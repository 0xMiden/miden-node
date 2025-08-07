//! gRPC client builder utilities for Miden node.
//!
//! This module provides a unified [`Builder`] pattern for creating various gRPC clients with
//! consistent configuration options including TLS, OTEL interceptors, and connection types.
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

use std::collections::HashMap;
use std::fmt::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::metadata::AsciiMetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

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
    pub fn with_accept_metadata(
        mut self,
        version: &str,
        genesis: Option<&str>,
    ) -> Result<Self, anyhow::Error> {
        let mut accept_value = format!("application/vnd.miden; version={version}");
        if let Some(genesis) = genesis {
            write!(accept_value, "; genesis={genesis}")?;
        }
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

pub type RpcClient =
    generated::rpc::api_client::ApiClient<InterceptedService<Channel, MetadataInterceptor>>;
pub type BlockProducerClient =
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

/// Builder for configuring and creating gRPC clients with consistent options.
///
/// This builder provides a fluent interface for configuring various aspects of gRPC client
/// connections including TLS, timeouts, metadata, and connection types. It supports both eager and
/// lazy connection establishment.
///
/// # Examples
///
/// ```rust,no_run
/// use miden_node_proto::clients::{Builder, Rpc, RpcClient};
/// use std::time::Duration;
///
/// # async fn example() -> anyhow::Result<()> {
/// // Create a client with TLS and timeout
/// let client: RpcClient = Builder::new()
///     .with_address("https://rpc.example.com:8080".to_string())
///     .with_tls()
///     .with_timeout(Duration::from_secs(30))
///     .connect::<Rpc>()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Default, Clone)]
pub struct Builder {
    /// Whether to enable TLS encryption for the connection.
    pub with_tls: bool,
    /// The gRPC server address in the format `{protocol}://{hostname}:{port}`.
    pub address: String,
    /// Optional timeout for gRPC operations.
    pub with_timeout: Option<Duration>,
    /// Optional version string to include in request metadata.
    pub metadata_version: Option<String>,
    /// Optional genesis commitment string to include in request metadata.
    pub metadata_genesis: Option<String>,
}

impl Builder {
    /// Creates a new builder with default configuration.
    ///
    /// The default configuration has TLS disabled, no timeout, and no metadata version.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables TLS encryption for the gRPC connection.
    ///
    /// When enabled, the client will use native system certificate roots for TLS verification.
    /// This is required for connecting to secure endpoints.
    #[must_use]
    pub fn with_tls(mut self) -> Self {
        self.with_tls = true;
        self
    }

    /// Sets a timeout for gRPC operations.
    ///
    /// This timeout applies to all gRPC calls made through the client. If not set,
    /// operations may hang indefinitely.
    ///
    /// # Arguments
    ///
    /// * `timeout` - The duration after which gRPC operations will timeout
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.with_timeout = Some(timeout);
        self
    }

    /// Sets the version string to include in request metadata.
    ///
    /// This version is used by the [`MetadataInterceptor`] to set the `Accept` header
    /// in gRPC requests. If not set, the current crate version will be used.
    ///
    /// # Arguments
    ///
    /// * `version` - The version string to include in request metadata
    #[must_use]
    pub fn with_metadata_version(mut self, version: String) -> Self {
        self.metadata_version = Some(version);
        self
    }

    /// Sets the genesis commitment string to include in request metadata.
    ///
    /// This genesis is used by the [`MetadataInterceptor`] to set the `Accept` header
    /// in gRPC requests. If not set, no genesis will be included.
    ///
    /// # Arguments
    ///
    /// * `genesis` - The genesis commitment string to include in request metadata
    #[must_use]
    pub fn with_metadata_genesis(mut self, genesis: String) -> Self {
        self.metadata_genesis = Some(genesis);
        self
    }

    /// Sets the gRPC server address.
    ///
    /// The address should be in the format `{protocol}://{hostname}:{port}`.
    /// Examples: `http://localhost:8080`, `https://api.example.com:443`
    ///
    /// # Arguments
    ///
    /// * `address` - The gRPC server address
    #[must_use]
    pub fn with_address(mut self, address: String) -> Self {
        self.address = address;
        self
    }

    /// Establishes an eager connection to the gRPC server.
    ///
    /// This method attempts to connect to the server immediately and returns a client only after
    /// the connection is established. If the connection fails, an error is returned.
    ///
    /// # Arguments
    ///
    /// * `T` - The type implementing [`GrpcClientBuilder`] that determines which client type to
    ///   create (e.g., [`Rpc`], [`BlockProducer`], [`StoreRpc`])
    ///
    /// # Returns
    ///
    /// A configured gRPC client of type `T::Service` if the connection succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established or if the endpoint configuration
    /// is invalid.
    pub async fn connect<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let channel = self.build_endpoint()?.connect().await?;
        Ok(T::with_interceptor(channel, &self))
    }

    /// Establishes a lazy connection to the gRPC server.
    ///
    /// This method returns a client immediately without attempting to connect. The actual
    /// connection is established when the first gRPC call is made. This is useful for creating
    /// clients that may not be used immediately.
    ///
    /// # Arguments
    ///
    /// * `T` - The type implementing [`GrpcClientBuilder`] that determines which client type to
    ///   create (e.g., [`Rpc`], [`BlockProducer`], [`StoreRpc`])
    ///
    /// # Returns
    ///
    /// A configured gRPC client of type `T::Service`. Connection errors will only be encountered
    /// when making actual gRPC calls.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint configuration is invalid.
    pub fn connect_lazy<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let channel = self.build_endpoint()?.connect_lazy();
        Ok(T::with_interceptor(channel, &self))
    }

    /// Builds a tonic [`Endpoint`] from the builder configuration.
    ///
    /// This method creates and configures a gRPC endpoint with the specified address, timeout, and
    /// TLS settings.
    ///
    /// # Returns
    ///
    /// A configured [`Endpoint`] ready for connection establishment.
    ///
    /// # Errors
    ///
    /// Returns an error if the address is invalid or TLS configuration fails.
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
/// This trait provides a standardized way to create different gRPC clients with consistent
/// configuration options like TLS, OTEL interceptors, and connection types.
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
    type Service = RpcClient;

    fn with_interceptor(channel: Channel, builder: &Builder) -> Self::Service {
        // Use version from builder or default
        let version = builder.metadata_version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"));
        let interceptor = MetadataInterceptor::default()
            .with_accept_metadata(version, builder.metadata_genesis.as_deref())
            .expect("Failed to create metadata interceptor");

        generated::rpc::api_client::ApiClient::with_interceptor(channel, interceptor)
    }
}

impl GrpcClientBuilder for BlockProducer {
    type Service = BlockProducerClient;

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
