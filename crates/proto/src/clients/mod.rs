//! gRPC client builder utilities for Miden node.
//!
//! This module provides a unified type-state [`Builder`] for creating various gRPC clients with
//! explicit configuration decisions for TLS, timeout, and metadata.
//!
//! # Examples
//!
//! ```rust,no_run
//! use miden_node_proto::clients::{Builder, WantsTls, StoreNtxBuilderClient, StoreNtxBuilder};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Create a store client with OTEL and TLS
//! let client: StoreNtxBuilderClient = Builder::<WantsTls>::new("https://store.example.com")?
//!     .with_tls()?                 // or `.without_tls()`
//!     .without_timeout()           // or `.with_timeout(Duration::from_secs(10))`
//!     .without_metadata_version()  // or `.with_metadata_version("1.0".into())`
//!     .without_metadata_genesis()  // or `.with_metadata_genesis(genesis)`
//!     .connect::<StoreNtxBuilder>()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt::Write;
use std::marker::PhantomData;
use std::time::Duration;

use anyhow::{Context, Result};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::metadata::AsciiMetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
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

// GRPC CLIENT BUILDER TRAIT
// ================================================================================================

/// Configuration for gRPC clients.
///
/// This struct contains the configuration for gRPC clients, including the metadata version and
/// genesis commitment.
pub struct ClientConfig {
    pub metadata_version: Option<String>,
    pub metadata_genesis: Option<String>,
}

/// Trait for building gRPC clients from a common [`Builder`] configuration.
///
/// This trait provides a standardized way to create different gRPC clients with consistent
/// configuration options like TLS, OTEL interceptors, and connection types.
pub trait GrpcClientBuilder {
    type Service;

    fn with_interceptor(channel: Channel, config: &ClientConfig) -> Self::Service;
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

    fn with_interceptor(channel: Channel, config: &ClientConfig) -> Self::Service {
        // For RPC, include Accept header only if version was explicitly provided.
        let mut interceptor = MetadataInterceptor::default();
        if let Some(version) = config.metadata_version.as_deref() {
            interceptor = interceptor
                .with_accept_metadata(version, config.metadata_genesis.as_deref())
                .expect("Failed to create metadata interceptor");
        }
        generated::rpc::api_client::ApiClient::with_interceptor(channel, interceptor)
    }
}

impl GrpcClientBuilder for BlockProducer {
    type Service = BlockProducerClient;

    fn with_interceptor(channel: Channel, _config: &ClientConfig) -> Self::Service {
        generated::block_producer::api_client::ApiClient::with_interceptor(channel, OtelInterceptor)
    }
}

impl GrpcClientBuilder for StoreNtxBuilder {
    type Service = StoreNtxBuilderClient;

    fn with_interceptor(channel: Channel, _config: &ClientConfig) -> Self::Service {
        generated::ntx_builder_store::ntx_builder_client::NtxBuilderClient::with_interceptor(
            channel,
            OtelInterceptor,
        )
    }
}

impl GrpcClientBuilder for StoreBlockProducer {
    type Service = StoreBlockProducerClient;

    fn with_interceptor(channel: Channel, _config: &ClientConfig) -> Self::Service {
        generated::block_producer_store::block_producer_client::BlockProducerClient::with_interceptor(
            channel,
            OtelInterceptor,
        )
    }
}

impl GrpcClientBuilder for StoreRpc {
    type Service = StoreRpcClient;

    fn with_interceptor(channel: Channel, _config: &ClientConfig) -> Self::Service {
        generated::rpc_store::rpc_client::RpcClient::with_interceptor(channel, OtelInterceptor)
    }
}

// STRICT TYPE-STATE BUILDER (NO DEFAULTS)
// ================================================================================================

/// A type-state builder that forces the caller to make an explicit decision for each
/// configuration item (TLS, timeout, metadata version, metadata genesis) before connecting.
///
/// This builder replaces the previous defaulted builder. Callers must explicitly choose TLS,
/// timeout, and metadata options before connecting.
///
/// Usage example:
///
/// ```rust,no_run
/// use miden_node_proto::clients::{Builder, WantsTls, Rpc, RpcClient};
/// use std::time::Duration;
///
/// # async fn example() -> anyhow::Result<()> {
/// let client: RpcClient = Builder::<WantsTls>::new("https://rpc.example.com:8080")?
///     .with_tls()?                        // or `.without_tls()`
///     .with_timeout(Duration::from_secs(5)) // or `.without_timeout()`
///     .with_metadata_version("1.0".into()) // or `.without_metadata_version()`
///     .without_metadata_genesis()           // or `.with_metadata_genesis(genesis)`
///     .connect::<Rpc>()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Builder<State> {
    address: String,
    tls_enabled: Option<bool>,
    timeout: Option<Duration>,
    metadata_version: Option<String>,
    metadata_genesis: Option<String>,
    _state: PhantomData<State>,
}

#[derive(Copy, Clone, Debug)]
pub struct WantsTls;
#[derive(Copy, Clone, Debug)]
pub struct WantsTimeout;
#[derive(Copy, Clone, Debug)]
pub struct WantsVersion;
#[derive(Copy, Clone, Debug)]
pub struct WantsGenesis;
#[derive(Copy, Clone, Debug)]
pub struct WantsConnection;

impl<State> Builder<State> {
    /// Convenience function to cast the state type and carry internal configuration forward.
    fn next_state<Next>(self) -> Builder<Next> {
        Builder {
            address: self.address,
            tls_enabled: self.tls_enabled,
            timeout: self.timeout,
            metadata_version: self.metadata_version,
            metadata_genesis: self.metadata_genesis,
            _state: PhantomData::<Next>,
        }
    }
}

impl Builder<WantsTls> {
    /// Create a new strict builder from a gRPC endpoint URL such as
    /// `http://localhost:8080` or `https://api.example.com:443`.
    pub fn new(url: impl Into<String>) -> Result<Builder<WantsTls>> {
        let address = url.into();
        // Basic upfront validation using `Endpoint::from_shared` path; let `Builder` do full checks
        // later to keep logic in one place.
        let _ = Endpoint::from_shared(address.clone())
            .context("Failed to create endpoint from address")?;

        Ok(Builder {
            address,
            tls_enabled: None,
            timeout: None,
            metadata_version: None,
            metadata_genesis: None,
            _state: PhantomData,
        })
    }

    /// Explicitly disable TLS.
    pub fn without_tls(mut self) -> Builder<WantsTimeout> {
        self.tls_enabled = Some(false);
        self.next_state()
    }

    /// Explicitly enable TLS.
    pub fn with_tls(mut self) -> Result<Builder<WantsTimeout>> {
        // We only record the decision here; the actual TLS wiring is performed by the underlying
        // `Builder` during `connect()` to keep the logic centralized.
        self.tls_enabled = Some(true);
        Ok(self.next_state())
    }
}

impl Builder<WantsTimeout> {
    /// Explicitly disable request timeout.
    pub fn without_timeout(mut self) -> Builder<WantsVersion> {
        self.timeout = None;
        self.next_state()
    }

    /// Explicitly configure a request timeout.
    pub fn with_timeout(mut self, duration: Duration) -> Builder<WantsVersion> {
        self.timeout = Some(duration);
        self.next_state()
    }
}

impl Builder<WantsVersion> {
    /// Do not include version in request metadata.
    pub fn without_metadata_version(mut self) -> Builder<WantsGenesis> {
        self.metadata_version = None;
        self.next_state()
    }

    /// Include a specific version string in request metadata.
    pub fn with_metadata_version(mut self, version: String) -> Builder<WantsGenesis> {
        self.metadata_version = Some(version);
        self.next_state()
    }
}

impl Builder<WantsGenesis> {
    /// Do not include genesis commitment in request metadata.
    pub fn without_metadata_genesis(mut self) -> Builder<WantsConnection> {
        self.metadata_genesis = None;
        self.next_state()
    }

    /// Include a specific genesis commitment string in request metadata.
    pub fn with_metadata_genesis(mut self, genesis: String) -> Builder<WantsConnection> {
        self.metadata_genesis = Some(genesis);
        self.next_state()
    }
}

impl Builder<WantsConnection> {
    /// Establish an eager connection and return a fully configured client.
    pub async fn connect<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let mut endpoint = Endpoint::from_shared(self.address)
            .context("Failed to create endpoint from address")?;
        if let Some(timeout) = self.timeout {
            endpoint = endpoint.timeout(timeout);
        }
        if self.tls_enabled.expect("TLS decision must be made before connect") {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_native_roots())
                .context("Failed to configure TLS")?;
        }
        let channel = endpoint.connect().await?;
        let cfg = ClientConfig {
            metadata_version: self.metadata_version,
            metadata_genesis: self.metadata_genesis,
        };
        Ok(T::with_interceptor(channel, &cfg))
    }

    /// Establish a lazy connection and return a client that will connect on first use.
    pub fn connect_lazy<T>(self) -> Result<T::Service>
    where
        T: GrpcClientBuilder,
    {
        let mut endpoint = Endpoint::from_shared(self.address)
            .context("Failed to create endpoint from address")?;
        if let Some(timeout) = self.timeout {
            endpoint = endpoint.timeout(timeout);
        }
        if self.tls_enabled.expect("TLS decision must be made before connect") {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_native_roots())
                .context("Failed to configure TLS")?;
        }
        let channel = endpoint.connect_lazy();
        let cfg = ClientConfig {
            metadata_version: self.metadata_version,
            metadata_genesis: self.metadata_genesis,
        };
        Ok(T::with_interceptor(channel, &cfg))
    }
}
