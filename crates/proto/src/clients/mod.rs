use std::{collections::HashMap, net::SocketAddr, time::Duration};

use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::{
    metadata::AsciiMetadataValue,
    service::{Interceptor, interceptor::InterceptedService},
    transport::{Channel, ClientTlsConfig, Endpoint},
};
use url::Url;

// RE-EXPORTS
// ================================================================================================
pub use self::error::*;

// MODULES
// ================================================================================================

mod error;

// CLIENT BUILDER
// ================================================================================================

/// Generic client builder for configuring and creating gRPC clients.
///
/// This builder provides a consistent interface for creating any type of gRPC client
/// with common configuration options like interceptors, TLS, timeouts, etc.
///
/// # Examples
///
/// ```no_run
/// use miden_node_proto::clients::ClientBuilder;
/// use std::net::SocketAddr;
/// use url::Url;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Create an RPC store client with OpenTelemetry tracing
/// let store_addr: SocketAddr = "127.0.0.1:8080".parse()?;
/// let rpc_client = ClientBuilder::new()
///     .with_otel()
///     .build_rpc_store_client(store_addr)
///     .await?;
///
/// // Create an NtxBuilder store client with TLS and lazy connection
/// let store_url: Url = "https://store.example.com".parse()?;
/// let ntx_client = ClientBuilder::new()
///     .with_otel()
///     .with_tls()
///     .with_lazy_connection(true)
///     .build_ntx_builder_store_client(&store_url)
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Default, Clone)]
pub struct ClientBuilder {
    with_otel: bool,
    tls: Option<ClientTlsConfig>,
    timeout: Option<Duration>,
    connect_lazy: bool,
    rpc_version: Option<String>,
}

impl ClientBuilder {
    /// Creates a new client builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable OpenTelemetry tracing interceptor.
    #[must_use]
    pub fn with_otel(mut self) -> Self {
        self.with_otel = true;
        self
    }

    /// Enable TLS with default configuration (native roots).
    #[must_use]
    pub fn with_tls(mut self) -> Self {
        self.tls = Some(ClientTlsConfig::new().with_native_roots());
        self
    }

    /// Set a custom TLS configuration.
    #[must_use]
    pub fn with_tls_config(mut self, tls_config: ClientTlsConfig) -> Self {
        self.tls = Some(tls_config);
        self
    }

    /// Set a timeout for requests.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Configure whether to use lazy connection (default: false).
    ///
    /// When enabled, connection is established on first request rather than immediately.
    #[must_use]
    pub fn with_lazy_connection(mut self, lazy: bool) -> Self {
        self.connect_lazy = lazy;
        self
    }

    /// Set the version for RPC metadata (required for RPC API clients).
    ///
    /// If not specified, defaults to the current package version.
    #[must_use]
    pub fn with_rpc_version(mut self, version: impl Into<String>) -> Self {
        self.rpc_version = Some(version.into());
        self
    }

    /// Build an RPC store client with OpenTelemetry interceptor.
    pub async fn build_rpc_store_client(
        &self,
        addr: SocketAddr,
    ) -> Result<RpcStoreClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        Ok(RpcStoreClient::new(channel))
    }

    /// Build a `BlockProducer` store client with OpenTelemetry interceptor.
    pub async fn build_block_producer_store_client(
        &self,
        addr: SocketAddr,
    ) -> Result<BlockProducerStoreClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        Ok(BlockProducerStoreClient::new(channel))
    }

    /// Build an `NtxBuilder` store client with OpenTelemetry interceptor.
    pub async fn build_ntx_builder_store_client(
        &self,
        url: &Url,
    ) -> Result<NtxBuilderStoreClient, ClientError> {
        let channel = self.create_channel(url.to_string()).await?;
        Ok(NtxBuilderStoreClient::new(channel))
    }

    /// Build an RPC API client with metadata interceptor and TLS.
    pub async fn build_rpc_api_client(&self, url: &Url) -> Result<RpcApiClient, ClientError> {
        // RPC clients always use TLS with native roots
        let mut builder = self.clone();
        if builder.tls.is_none() {
            builder = builder.with_tls();
        }

        let channel = builder.create_channel(url.to_string()).await?;

        // Create metadata interceptor with version
        let version = self.rpc_version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"));
        let metadata_interceptor = Self::create_metadata_interceptor(version)?;

        Ok(crate::generated::rpc::api_client::ApiClient::with_interceptor(
            channel,
            metadata_interceptor,
        ))
    }

    /// Build a `BlockProducer` API client with OpenTelemetry interceptor.
    pub async fn build_block_producer_api_client(
        &self,
        addr: SocketAddr,
    ) -> Result<BlockProducerApiClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        Ok(crate::generated::block_producer::api_client::ApiClient::with_interceptor(
            channel,
            OtelInterceptor,
        ))
    }

    /// Internal helper to create a configured channel.
    async fn create_channel(&self, endpoint: String) -> Result<Channel, ClientError> {
        let mut ep = Endpoint::try_from(endpoint)
            .map_err(|e| ClientError::InvalidEndpoint(e.to_string()))?;

        if let Some(timeout) = self.timeout {
            ep = ep.timeout(timeout);
        }

        if let Some(tls) = &self.tls {
            ep = ep.tls_config(tls.clone())?;
        }

        if self.connect_lazy {
            Ok(ep.connect_lazy())
        } else {
            Ok(ep.connect().await?)
        }
    }

    /// Internal helper to create a metadata interceptor for RPC clients.
    fn create_metadata_interceptor(version: &str) -> Result<MetadataInterceptor, ClientError> {
        let accept_value = format!("application/vnd.miden.{version}+grpc");
        let metadata_value = AsciiMetadataValue::try_from(accept_value)
            .map_err(|e| ClientError::InvalidMetadata(e.to_string()))?;

        let mut metadata = HashMap::new();
        metadata.insert("accept", metadata_value);

        Ok(MetadataInterceptor { metadata })
    }
}

// METADATA INTERCEPTOR
// ================================================================================================

/// Interceptor for adding metadata to RPC requests.
#[derive(Clone)]
pub struct MetadataInterceptor {
    metadata: HashMap<&'static str, AsciiMetadataValue>,
}

impl Interceptor for MetadataInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        for (key, value) in &self.metadata {
            request.metadata_mut().insert(*key, value.clone());
        }
        Ok(request)
    }
}

// TYPE ALIASES
// ================================================================================================

/// Type alias for RPC API client with metadata interceptor.
pub type RpcApiClient =
    crate::generated::rpc::api_client::ApiClient<InterceptedService<Channel, MetadataInterceptor>>;

/// Type alias for Block Producer API client with OpenTelemetry interceptor.
pub type BlockProducerApiClient = crate::generated::block_producer::api_client::ApiClient<
    InterceptedService<Channel, OtelInterceptor>,
>;

// CLIENT WRAPPERS
// ================================================================================================

/// RPC store client wrapper with OpenTelemetry tracing.
///
/// This wrapper provides access to all RPC methods like `check_nullifiers`,
/// `get_account_details`, `sync_notes`, etc.
#[derive(Debug)]
pub struct RpcStoreClient {
    inner: crate::generated::store::rpc_client::RpcClient<
        InterceptedService<Channel, OtelInterceptor>,
    >,
}

impl RpcStoreClient {
    fn new(channel: Channel) -> Self {
        let inner = crate::generated::store::rpc_client::RpcClient::with_interceptor(
            channel,
            OtelInterceptor,
        );
        Self { inner }
    }

    /// Get a mutable reference to the underlying client.
    pub fn get_mut(
        &mut self,
    ) -> &mut crate::generated::store::rpc_client::RpcClient<
        InterceptedService<Channel, OtelInterceptor>,
    > {
        &mut self.inner
    }

    /// Get the underlying client.
    pub fn into_inner(
        self,
    ) -> crate::generated::store::rpc_client::RpcClient<InterceptedService<Channel, OtelInterceptor>>
    {
        self.inner
    }
}

impl Clone for RpcStoreClient {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

/// `BlockProducer` store client wrapper with OpenTelemetry tracing.
///
/// This wrapper provides access to all `BlockProducer` methods like `get_transaction_inputs`,
/// `get_block_inputs`, `apply_block`, etc.
#[derive(Debug)]
pub struct BlockProducerStoreClient {
    inner: crate::generated::store::block_producer_client::BlockProducerClient<
        InterceptedService<Channel, OtelInterceptor>,
    >,
}

impl BlockProducerStoreClient {
    fn new(channel: Channel) -> Self {
        let inner =
            crate::generated::store::block_producer_client::BlockProducerClient::with_interceptor(
                channel,
                OtelInterceptor,
            );
        Self { inner }
    }

    /// Get a mutable reference to the underlying client.
    pub fn get_mut(
        &mut self,
    ) -> &mut crate::generated::store::block_producer_client::BlockProducerClient<
        InterceptedService<Channel, OtelInterceptor>,
    > {
        &mut self.inner
    }

    /// Get the underlying client.
    pub fn into_inner(
        self,
    ) -> crate::generated::store::block_producer_client::BlockProducerClient<
        InterceptedService<Channel, OtelInterceptor>,
    > {
        self.inner
    }
}

impl Clone for BlockProducerStoreClient {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

/// `NtxBuilder` store client wrapper with OpenTelemetry tracing.
///
/// This wrapper provides access to all `NtxBuilder` methods like `get_current_blockchain_data`,
/// `get_unconsumed_network_notes`, etc.
#[derive(Debug)]
pub struct NtxBuilderStoreClient {
    inner: crate::generated::store::ntx_builder_client::NtxBuilderClient<
        InterceptedService<Channel, OtelInterceptor>,
    >,
}

impl NtxBuilderStoreClient {
    fn new(channel: Channel) -> Self {
        let inner = crate::generated::store::ntx_builder_client::NtxBuilderClient::with_interceptor(
            channel,
            OtelInterceptor,
        );
        Self { inner }
    }

    /// Get a mutable reference to the underlying client.
    pub fn get_mut(
        &mut self,
    ) -> &mut crate::generated::store::ntx_builder_client::NtxBuilderClient<
        InterceptedService<Channel, OtelInterceptor>,
    > {
        &mut self.inner
    }

    /// Get the underlying client.
    pub fn into_inner(
        self,
    ) -> crate::generated::store::ntx_builder_client::NtxBuilderClient<
        InterceptedService<Channel, OtelInterceptor>,
    > {
        self.inner
    }
}

impl Clone for NtxBuilderStoreClient {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

/// Generic client wrapper for cases where specific wrappers are not available.
pub struct Client<T> {
    /// The underlying gRPC client.
    pub inner: T,
}

impl<T> Client<T> {
    /// Create a new client wrapper.
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Consume the wrapper and return the underlying client.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Get a reference to the underlying client.
    pub fn get_ref(&self) -> &T {
        &self.inner
    }

    /// Get a mutable reference to the underlying client.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: Clone> Clone for Client<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_builder_creation() {
        let builder = ClientBuilder::new();
        assert!(!builder.with_otel);
        assert!(builder.tls.is_none());
        assert!(builder.timeout.is_none());
        assert!(!builder.connect_lazy);
    }

    #[test]
    fn test_client_builder_configuration() {
        let builder = ClientBuilder::new()
            .with_otel()
            .with_tls()
            .with_timeout(Duration::from_secs(30))
            .with_lazy_connection(true);

        assert!(builder.with_otel);
        assert!(builder.tls.is_some());
        assert_eq!(builder.timeout, Some(Duration::from_secs(30)));
        assert!(builder.connect_lazy);
    }
}
