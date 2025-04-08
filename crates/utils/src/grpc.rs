use std::net::SocketAddr;

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
            .ok_or_else(|| anyhow::anyhow!("address resolution failed for {}", self.to_string()))
    }
}

mod private {
    pub trait Sealed {}
    impl Sealed for url::Url {}
}
