use std::net::SocketAddr;

use http::uri::Scheme;
use miden_node_proto::generated::block_producer::api_client::ApiClient as GeneratedApi;
use miden_node_utils::tracing::grpc::OtelInterceptor;
use tonic::{
    service::interceptor::InterceptedService,
    transport::{Channel, Endpoint},
};

type GeneratedClient = GeneratedApi<InterceptedService<Channel, OtelInterceptor>>;

pub(crate) struct Client {
    inner: GeneratedClient,
}

impl Client {
    pub(crate) fn connect_lazy(addr: SocketAddr) -> Self {
        // SAFETY: http://{addr} will always form a valid Uri.
        let uri = http::Uri::builder()
            .scheme(Scheme::HTTP)
            .authority(addr.to_string())
            .build()
            .unwrap();

        let client = Endpoint::from(uri).connect_lazy();
        let client = GeneratedApi::with_interceptor(client, OtelInterceptor);

        Self { inner: client }
    }
}
