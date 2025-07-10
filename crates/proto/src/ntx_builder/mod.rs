use std::net::SocketAddr;

use http::uri::Scheme;
use miden_objects::{
    note::{Note, Nullifier},
    transaction::TransactionId,
};
use prost::bytes::Bytes;
use tonic::{
    service::{Interceptor, interceptor::InterceptedService},
    transport::{Body, Channel, Endpoint},
};

use crate::generated::ntx_builder as ntx_builder_proto;

type StdError = Box<dyn std::error::Error + Send + Sync + 'static>;
type GeneratedClient<T> = crate::generated::ntx_builder::api_client::ApiClient<T>;

#[derive(Clone)]
pub struct Client<T> {
    inner: GeneratedClient<T>,
}

impl<I: Interceptor> Client<InterceptedService<Channel, I>> {
    /// Creates a new [`Client`] which lazily connects with the given [`Interceptor`].
    pub fn connect_lazy(addr: SocketAddr, interceptor: I) -> Self {
        // SAFETY: http://{addr} will always form a valid Uri.
        let uri = http::Uri::builder()
            .scheme(Scheme::HTTP)
            .authority(addr.to_string())
            .path_and_query("/")
            .build()
            .unwrap();

        let client = Endpoint::from(uri).connect_lazy();
        let client = GeneratedClient::with_interceptor(client, interceptor);

        Client { inner: client }
    }
}

impl<T> Client<T>
where
    T: tonic::client::GrpcService<tonic::body::Body>,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + std::marker::Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + std::marker::Send,
{
    // TODO: this should probably be an entire block instead of just a single tx.
    pub async fn submit_network_notes(
        &mut self,
        tx_id: TransactionId,
        notes: impl Iterator<Item = Note>,
    ) -> Result<(), tonic::Status> {
        let request = ntx_builder_proto::TransactionNetworkNotes {
            transaction_id: Some(tx_id.into()),
            note: notes.map(Into::into).collect(),
        };
        self.inner.submit_network_notes(request).await.map(|_| ())
    }

    pub async fn update_transaction_status(
        &mut self,
        statuses: impl Iterator<
            Item = (TransactionId, ntx_builder_proto::transaction_status::TransactionStatus),
        >,
    ) -> Result<(), tonic::Status> {
        let request = ntx_builder_proto::TransactionStatus {
            updates: statuses
                .map(|(id, status)| ntx_builder_proto::transaction_status::TransactionUpdate {
                    transaction_id: Some(id.into()),
                    status: status.into(),
                })
                .collect(),
        };
        self.inner.update_transaction_status(request).await.map(|_| ())
    }

    pub async fn update_network_notes(
        &mut self,
        transaction_id: TransactionId,
        nullifiers: impl Iterator<Item = Nullifier>,
    ) -> Result<(), tonic::Status> {
        let request = ntx_builder_proto::NetworkNotes {
            transaction_id: Some(transaction_id.into()),
            nullifiers: nullifiers.map(Into::into).collect(),
        };
        self.inner.update_network_notes(request).await.map(|_| ())
    }
}
