use std::net::SocketAddr;

use miden_node_proto::{
    errors::{ConversionError, MissingFieldHelper},
    generated::{
        requests::{
            GetAccountDetailsRequest, GetBlockHeaderByNumberRequest,
            GetUnconsumedNetworkNotesRequest,
        },
        store::api_client as store_client,
    },
};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::{
    account::{Account, AccountId},
    block::BlockHeader,
    note::Note,
};
use miden_tx::utils::Deserializable;
use thiserror::Error;
use tonic::{service::interceptor::InterceptedService, transport::Channel};
use tracing::{info, instrument};

use crate::COMPONENT;

// STORE CLIENT
// ================================================================================================

type InnerClient = store_client::ApiClient<InterceptedService<Channel, OtelInterceptor>>;

/// Interface to the store's gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct StoreClient {
    inner: InnerClient,
}

impl StoreClient {
    /// Creates a new store client with a lazy connection.
    pub fn new(store_address: SocketAddr) -> Self {
        let store_url = format!("http://{store_address}");
        // SAFETY: The store_url is always valid as it is created from a `SocketAddr`.
        let channel = tonic::transport::Endpoint::try_from(store_url).unwrap().connect_lazy();
        let store = store_client::ApiClient::with_interceptor(channel, OtelInterceptor);
        info!(target: COMPONENT, store_endpoint = %store_address, "Store client initialized");

        Self { inner: store }
    }

    /// Returns the latest block's header from the store.
    #[instrument(target = COMPONENT, name = "store.client.latest_header", skip_all, err)]
    pub async fn latest_header(&self) -> Result<BlockHeader, StoreError> {
        let response = self
            .inner
            .clone()
            .get_block_header_by_number(tonic::Request::new(
                GetBlockHeaderByNumberRequest::default(),
            ))
            .await?
            .into_inner()
            .block_header
            .ok_or(miden_node_proto::generated::block::BlockHeader::missing_field(
                "block_header",
            ))?;

        BlockHeader::try_from(response).map_err(Into::into)
    }

    /// Returns the latest block's header from the store.
    #[instrument(target = COMPONENT, name = "store.client.get_unconsumed_network_notes", skip_all, err)]
    pub async fn get_unconsumed_network_notes(&self) -> Result<Vec<Note>, StoreError> {
        let mut all_notes = Vec::new();
        let mut page_token: Option<u64> = None;

        loop {
            let req = GetUnconsumedNetworkNotesRequest { page_token, page_size: 128 };
            let resp = self.inner.clone().get_unconsumed_network_notes(req).await?.into_inner();

            let page: Vec<Note> =
                resp.notes.into_iter().map(Note::try_from).collect::<Result<Vec<_>, _>>()?;

            all_notes.extend(page);

            match resp.next_token {
                Some(tok) => page_token = Some(tok),
                None => break,
            }
        }

        Ok(all_notes)
    }

    #[instrument(target = COMPONENT, name = "store.client.get_network_account", skip_all, err)]
    pub async fn get_network_account(&self, account_id: AccountId) -> Result<Account, StoreError> {
        let request = GetAccountDetailsRequest { account_id: Some(account_id.into()) };

        let store_response = self
            .inner
            .clone()
            .get_account_details(request)
            .await?
            .into_inner()
            .details
            .unwrap(); // TODO

        let faucet_account_state_bytes = store_response.details.unwrap();

        Ok(Account::read_from_bytes(&faucet_account_state_bytes).unwrap())
    }
}

// Store errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("gRPC client error")]
    GrpcClientError(#[from] tonic::Status),
    #[error("failed to parse response")]
    DeserializationError(#[from] ConversionError),
}
