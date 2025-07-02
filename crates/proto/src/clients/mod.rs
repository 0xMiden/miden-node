use std::net::SocketAddr;
use std::time::Duration;

use tonic::{
    transport::{Channel, ClientTlsConfig, Endpoint},
    service::interceptor::InterceptedService,
};
use miden_node_utils::tracing::grpc::OtelInterceptor;
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
#[derive(Default)]
pub struct ClientBuilder {
    with_otel: bool,
    tls: Option<ClientTlsConfig>,
    timeout: Option<Duration>,
    connect_lazy: bool,
}

impl ClientBuilder {
    /// Creates a new client builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Enable OpenTelemetry tracing interceptor.
    pub fn with_otel(mut self) -> Self {
        self.with_otel = true;
        self
    }
    
    /// Enable TLS with default configuration (native roots).
    pub fn with_tls(mut self) -> Self {
        self.tls = Some(ClientTlsConfig::new().with_native_roots());
        self
    }
    
    /// Set a custom TLS configuration.
    pub fn with_tls_config(mut self, tls_config: ClientTlsConfig) -> Self {
        self.tls = Some(tls_config);
        self
    }
    
    /// Set a timeout for requests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
    
    /// Configure whether to use lazy connection (default: false).
    ///
    /// When enabled, connection is established on first request rather than immediately.
    pub fn with_lazy_connection(mut self, lazy: bool) -> Self {
        self.connect_lazy = lazy;
        self
    }
    
    /// Build an RPC store client with OpenTelemetry interceptor.
    pub async fn build_rpc_store_client(&self, addr: SocketAddr) -> Result<RpcStoreClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        Ok(RpcStoreClient::new(channel))
    }
    
    /// Build a BlockProducer store client with OpenTelemetry interceptor.
    pub async fn build_block_producer_store_client(&self, addr: SocketAddr) -> Result<BlockProducerStoreClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        Ok(BlockProducerStoreClient::new(channel))
    }
    
    /// Build an NtxBuilder store client with OpenTelemetry interceptor.
    pub async fn build_ntx_builder_store_client(&self, url: &Url) -> Result<NtxBuilderStoreClient, ClientError> {
        let channel = self.create_channel(url.to_string()).await?;
        Ok(NtxBuilderStoreClient::new(channel))
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
}

// CLIENT WRAPPERS
// ================================================================================================

/// RPC store client wrapper with OpenTelemetry tracing.
///
/// This wrapper provides access to all RPC methods like `check_nullifiers`,
/// `get_account_details`, `sync_notes`, etc.
pub struct RpcStoreClient {
    inner: crate::generated::store::rpc_client::RpcClient<InterceptedService<Channel, OtelInterceptor>>,
}

impl RpcStoreClient {
    fn new(channel: Channel) -> Self {
        let inner = crate::generated::store::rpc_client::RpcClient::with_interceptor(channel, OtelInterceptor);
        Self { inner }
    }
    
    /// Get a mutable reference to the underlying client.
    pub fn get_mut(&mut self) -> &mut crate::generated::store::rpc_client::RpcClient<InterceptedService<Channel, OtelInterceptor>> {
        &mut self.inner
    }
    
    /// Get the underlying client.
    pub fn into_inner(self) -> crate::generated::store::rpc_client::RpcClient<InterceptedService<Channel, OtelInterceptor>> {
        self.inner
    }
}

impl Clone for RpcStoreClient {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// BlockProducer store client wrapper with OpenTelemetry tracing.
///
/// This wrapper provides access to all BlockProducer methods like `get_transaction_inputs`,
/// `get_block_inputs`, `apply_block`, etc.
pub struct BlockProducerStoreClient {
    inner: crate::generated::store::block_producer_client::BlockProducerClient<InterceptedService<Channel, OtelInterceptor>>,
}

impl BlockProducerStoreClient {
    fn new(channel: Channel) -> Self {
        let inner = crate::generated::store::block_producer_client::BlockProducerClient::with_interceptor(channel, OtelInterceptor);
        Self { inner }
    }
    
    /// Get a mutable reference to the underlying client.
    pub fn get_mut(&mut self) -> &mut crate::generated::store::block_producer_client::BlockProducerClient<InterceptedService<Channel, OtelInterceptor>> {
        &mut self.inner
    }
    
    /// Get the underlying client.
    pub fn into_inner(self) -> crate::generated::store::block_producer_client::BlockProducerClient<InterceptedService<Channel, OtelInterceptor>> {
        self.inner
    }
}

impl Clone for BlockProducerStoreClient {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// NtxBuilder store client wrapper with OpenTelemetry tracing.
///
/// This wrapper provides access to all NtxBuilder methods like `get_current_blockchain_data`,
/// `get_unconsumed_network_notes`, etc.
pub struct NtxBuilderStoreClient {
    inner: crate::generated::store::ntx_builder_client::NtxBuilderClient<InterceptedService<Channel, OtelInterceptor>>,
}

impl NtxBuilderStoreClient {
    fn new(channel: Channel) -> Self {
        let inner = crate::generated::store::ntx_builder_client::NtxBuilderClient::with_interceptor(channel, OtelInterceptor);
        Self { inner }
    }
    
    /// Get a mutable reference to the underlying client.
    pub fn get_mut(&mut self) -> &mut crate::generated::store::ntx_builder_client::NtxBuilderClient<InterceptedService<Channel, OtelInterceptor>> {
        &mut self.inner
    }
    
    /// Get the underlying client.
    pub fn into_inner(self) -> crate::generated::store::ntx_builder_client::NtxBuilderClient<InterceptedService<Channel, OtelInterceptor>> {
        self.inner
    }
}

impl Clone for NtxBuilderStoreClient {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
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
        Self {
            inner: self.inner.clone(),
        }
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
