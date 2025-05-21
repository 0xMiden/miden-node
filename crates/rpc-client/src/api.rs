use super::MetadataInterceptor;
use crate::RpcError;
use alloc::string::ToString;
use core::{
    ops::{Deref, DerefMut},
    time::Duration,
};
use miden_node_proto::generated::rpc::api_client::ApiClient as ProtoClient;
use tonic::service::interceptor::InterceptedService;
use url::Url;

type InnerClient = ProtoClient<InterceptedService<tonic::transport::Channel, MetadataInterceptor>>;

/// Client for the Miden RPC API which is fully configured to communicate with a Miden node.
#[derive(Clone)]
pub struct RpcClient(InnerClient);

impl RpcClient {
    /// Connects to the Miden node API using the provided URL and timeout.
    ///
    /// The client is configured with an interceptor that sets all requisite request metadata.
    ///
    /// If a version is not specified, the version found in the `Cargo.toml` of the workspace is
    /// used.
    pub async fn connect(
        url: &Url,
        timeout: Duration,
        version: Option<&'static str>,
    ) -> Result<RpcClient, RpcError> {
        let endpoint = tonic::transport::Endpoint::try_from(url.to_string())
            .expect("valid url produces valid tonic endpoint")
            .timeout(timeout);
        let channel = endpoint.connect().await?;
        let interceptor = accept_header_interceptor(version);
        Ok(RpcClient(ProtoClient::with_interceptor(channel, interceptor)))
    }
}

impl Deref for RpcClient {
    type Target = InnerClient;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RpcClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Returns the HTTP ACCEPT header [`MetadataInterceptor`] that is required for the Miden RPC.
fn accept_header_interceptor(version: Option<&'static str>) -> MetadataInterceptor {
    let version = version.unwrap_or(env!("CARGO_PKG_VERSION"));
    let accept_value = format!("application/vnd.miden.{version}+grpc");
    MetadataInterceptor::default()
        .with_metadata("accept", accept_value)
        .expect("valid key/value metadata for interceptor")
}
