use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use backoff::{Error as BackoffError, ExponentialBackoff, future::retry_notify};
use tokio::sync::RwLock;
use tonic::transport::{Channel, Endpoint, Error};
use tracing::{error, info};

/// A sealed extension trait for [`url::Url`] that adds convenience functions for binding and
/// connecting to the url.
pub trait UrlExt: private::Sealed {
    fn to_socket(&self) -> anyhow::Result<SocketAddr>;
}

impl UrlExt for url::Url {
    fn to_socket(&self) -> anyhow::Result<SocketAddr> {
        self.socket_addrs(|| None)?
            .into_iter()
            .next()
            .with_context(|| format!("failed to convert url {self} to socket address"))
    }
}

mod private {
    pub trait Sealed {}
    impl Sealed for url::Url {}
}

/// A lazily connecting gRPC client with retry logic.
///
/// This struct allows to defer the connection to a gRPC service until it's actually needed,
/// and will retry the connection using exponential backoff if the initial attempt fails.
#[derive(Clone)]
pub struct LazyClient<T> {
    /// Shared gRPC client.
    inner: Arc<RwLock<Option<T>>>,
    /// The address of the service to connect to.
    address: SocketAddr,
    /// A constructor function to create the gRPC client from a connected channel.
    constructor: fn(Channel) -> T,
}

impl<T: Clone> LazyClient<T> {
    /// Creates a new [`LazyClient`] for the given service address and client constructor.
    ///
    /// # Arguments
    ///
    /// * `address` - The address of the service to connect to.
    /// * `constructor` - A function that takes a [`tonic::transport::Channel`] and returns a new
    ///   client instance.
    pub fn new(address: SocketAddr, constructor: fn(Channel) -> T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            address,
            constructor,
        }
    }

    /// Returns a clone of the inner client, connecting if needed.
    ///
    /// This method checks if a client is already available, and if not, attempts to establish
    /// a connection using exponential backoff.
    pub async fn inner(&self) -> Result<T, Error> {
        if let Some(client) = self.inner.read().await.clone() {
            return Ok(client);
        }
        self.connect().await
    }

    /// Establishes a connection to the service using exponential backoff retries.
    ///
    /// If a connection already exists, it is returned immediately. Otherwise, it attempts to
    /// connect the client.
    async fn connect(&self) -> Result<T, Error> {
        let mut lock = self.inner.write().await;

        if let Some(client) = &*lock {
            return Ok(client.clone());
        }

        let url = format!("http://{}", self.address);
        let endpoint = Endpoint::try_from(url.clone())?;

        let connect = || async {
            match endpoint.connect().await {
                Ok(channel) => Ok((self.constructor)(channel)),
                Err(e) => Err(BackoffError::transient(e)),
            }
        };
        let notify = |err, dur| error!(%url, %err, ?dur, "gRPC connection failed, retrying");
        let client = retry_notify(ExponentialBackoff::default(), connect, notify).await?;

        *lock = Some(client.clone());
        drop(lock);

        info!(%url, "gRPC connection established");

        Ok(client)
    }
}
