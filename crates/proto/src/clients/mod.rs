use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

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
/// // Create an RPC store client with OpenTelemetry tracing enabled
/// let store_addr: SocketAddr = "127.0.0.1:8080".parse()?;
/// let rpc_client = ClientBuilder::new()
///     .with_otel()
///     .build_rpc_store_client(store_addr)
///     .await?;
///
/// // Create an NtxBuilder store client with TLS and lazy connection
/// let store_url: Url = "https://store.example.com".parse()?;
/// let ntx_client = ClientBuilder::new()
///     .with_tls()
///     .with_lazy_connection(true)
///     .build_ntx_builder_store_client(&store_url)
///     .await?;
/// # Ok(())
/// # }
/// ```


#[derive(Clone)]
pub struct ClientBuilder {
    with_otel: bool,
    tls: Option<ClientTlsConfig>,
    timeout: Option<Duration>,
    connect_lazy: bool,
    rpc_version: Option<String>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            with_otel: false,
            tls: None,
            timeout: None,
            connect_lazy: false,
            rpc_version: None,
        }
    }
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

    /// Set a custom timeout for requests.
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

    /// Build an RPC store client.
    pub async fn build_rpc_store_client(
        &self,
        addr: SocketAddr,
    ) -> Result<RpcStoreClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        let interceptor = ConditionalOtelInterceptor::new(self.with_otel);
        let inner =
            crate::generated::store::rpc_client::RpcClient::with_interceptor(channel, interceptor);
        Ok(Client::new(inner))
    }

    /// Build a `BlockProducer` store client.
    pub async fn build_block_producer_store_client(
        &self,
        addr: SocketAddr,
    ) -> Result<BlockProducerStoreClient, ClientError> {
        let endpoint = format!("http://{addr}");
        let channel = self.create_channel(endpoint).await?;
        let interceptor = ConditionalOtelInterceptor::new(self.with_otel);
        let inner =
            crate::generated::store::block_producer_client::BlockProducerClient::with_interceptor(
                channel,
                interceptor,
            );
        Ok(Client::new(inner))
    }

    /// Build an `NtxBuilder` store client.
    pub async fn build_ntx_builder_store_client(
        &self,
        url: &Url,
    ) -> Result<NtxBuilderStoreClient, ClientError> {
        let channel = self.create_channel(url.to_string()).await?;
        let interceptor = ConditionalOtelInterceptor::new(self.with_otel);
        let inner = crate::generated::store::ntx_builder_client::NtxBuilderClient::with_interceptor(
            channel,
            interceptor,
        );
        Ok(Client::new(inner))
    }

    /// Build an RPC API client with a metadata interceptor.
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

    /// Build a `BlockProducer` API client.
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

    /// Create a configured channel with the builder's settings.
    ///
    /// This method creates a gRPC channel with the configured TLS, timeout, and connection
    /// settings. It can be used by external clients that need custom channel creation logic.
    pub async fn create_channel(&self, endpoint: String) -> Result<Channel, ClientError> {
        let mut ep = Endpoint::try_from(endpoint)
            .map_err(|e| ClientError::InvalidEndpoint(e.to_string()))?;

        // Apply timeout if configured
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

/// Interceptor that conditionally applies OpenTelemetry tracing.
#[derive(Clone)]
pub struct ConditionalOtelInterceptor {
    enabled: bool,
}

impl ConditionalOtelInterceptor {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl Interceptor for ConditionalOtelInterceptor {
    fn call(&mut self, request: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        if self.enabled {
            // Apply OTel interceptor logic
            let mut otel_interceptor = OtelInterceptor;
            otel_interceptor.call(request)
        } else {
            // Pass through unchanged
            Ok(request)
        }
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

/// Type alias for RPC store client with conditional OpenTelemetry interceptor.
pub type RpcStoreClient = Client<
    crate::generated::store::rpc_client::RpcClient<
        InterceptedService<Channel, ConditionalOtelInterceptor>,
    >,
>;

/// Type alias for Block Producer store client with conditional OpenTelemetry interceptor.
pub type BlockProducerStoreClient = Client<
    crate::generated::store::block_producer_client::BlockProducerClient<
        InterceptedService<Channel, ConditionalOtelInterceptor>,
    >,
>;

/// Type alias for `NtxBuilder` store client with conditional OpenTelemetry interceptor.
pub type NtxBuilderStoreClient = Client<
    crate::generated::store::ntx_builder_client::NtxBuilderClient<
        InterceptedService<Channel, ConditionalOtelInterceptor>,
    >,
>;

// CLIENT WRAPPERS
// ================================================================================================

/// Generic client wrapper for cases where specific wrappers are not available.
#[derive(Debug)]
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

// REMOTE CLIENT MANAGER
// ================================================================================================

/// Generic manager for remote gRPC clients with lazy connection and timeout support.
///
/// This abstraction eliminates code duplication across remote prover clients by providing
/// common connection management, endpoint handling, and timeout configuration.
///
/// # Type Parameters
/// * `C` - The gRPC client type (e.g., `ApiClient<Channel>` or
///   `ApiClient<tonic_web_wasm_client::Client>`)
///
/// # Example
/// ```rust,ignore
/// use miden_node_proto::clients::{RemoteClientManager, ClientBuilder};
/// use std::time::Duration;
///
/// // Create a manager with default timeout
/// let manager: RemoteClientManager<ApiClient<Channel>> = RemoteClientManager::new("https://prover.example.com");
///
/// // Create a manager with custom timeout
/// let manager = RemoteClientManager::with_timeout("https://prover.example.com", Some(Duration::from_secs(30)));
///
/// // Connect and get the client (native example)
/// #[cfg(not(target_arch = "wasm32"))]
/// let client = manager.connect_with(|endpoint| async move {
///     let mut builder = ClientBuilder::new().with_tls();
///     // Access timeout from manager if needed: manager.timeout()
///     let channel = builder.create_channel(endpoint).await?;
///     Ok(ApiClient::new(channel))
/// }).await?;
///
/// // Connect and get the client (WASM example)
/// #[cfg(target_arch = "wasm32")]
/// let client = manager.connect_with(|endpoint| async move {
///     let web_client = tonic_web_wasm_client::Client::new(endpoint);
///     Ok(ApiClient::new(web_client))
/// }).await?;
/// ```
#[derive(Clone)]
pub struct RemoteClientManager<C> {
    client: Arc<tokio::sync::Mutex<Option<C>>>,
    endpoint: String,
    timeout: Option<Duration>,
}

impl<C> RemoteClientManager<C>
where
    C: Clone,
{
    /// Creates a new remote client manager with the specified endpoint.
    ///
    /// Uses no timeout by default.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            client: Arc::new(tokio::sync::Mutex::new(None)),
            endpoint: endpoint.into(),
            timeout: None,
        }
    }

    /// Creates a new remote client manager with an optional custom timeout.
    ///
    /// If `timeout` is `None`, no timeout is used.
    /// If `timeout` is `Some(duration)`, uses the specified timeout.
    pub fn with_timeout(endpoint: impl Into<String>, timeout: Option<Duration>) -> Self {
        Self {
            client: Arc::new(tokio::sync::Mutex::new(None)),
            endpoint: endpoint.into(),
            timeout,
        }
    }

    /// Connects to the remote service using a provided factory function.
    ///
    /// This method handles the connection logic, including:
    /// - Checking if already connected (returns existing client)
    /// - Calling the factory function to create the specific client type
    /// - Storing the client for reuse
    ///
    /// The factory function receives only the endpoint and is responsible for creating the
    /// appropriate transport with any desired configuration (e.g., `tonic::transport::Channel`
    /// for native, `tonic_web_wasm_client::Client` for WASM).
    ///
    /// # Arguments
    /// * `factory` - Async function that creates the client given an endpoint
    ///
    /// # Returns
    /// A clone of the connected client
    pub async fn connect_with<F, Fut, E>(&self, factory: F) -> Result<C, E>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<C, E>>,
    {
        let mut client_guard = self.client.lock().await;

        // Return existing client if already connected
        if let Some(client) = client_guard.as_ref() {
            return Ok(client.clone());
        }

        // Create the client using the factory
        let new_client = factory(self.endpoint.clone()).await?;

        // Store and return the client
        *client_guard = Some(new_client.clone());
        Ok(new_client)
    }

    /// Gets a clone of the current client if connected.
    ///
    /// Returns `None` if no connection has been established yet.
    pub async fn get_client(&self) -> Option<C> {
        self.client.lock().await.clone()
    }

    /// Gets the endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Gets the configured timeout.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

// TESTS
// ================================================================================================

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
