use std::time::Duration;

use miden_node_proto::{
    domain::{account::NetworkAccountPrefix, note::NetworkNote},
    errors::ConversionError,
    generated::{self as proto, ntx_builder_store::ntx_builder_client as store_client},
    try_convert,
};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::{
    account::Account,
    block::BlockHeader,
    crypto::merkle::{Forest, MmrPeaks, PartialMmr},
};
use miden_tx::utils::Deserializable;
use thiserror::Error;
use tonic::{service::interceptor::InterceptedService, transport::Channel};
use tracing::{info, instrument};
use url::Url;

use crate::COMPONENT;

// STORE CLIENT
// ================================================================================================

type InnerClient = store_client::NtxBuilderClient<InterceptedService<Channel, OtelInterceptor>>;

/// Interface to the store's ntx-builder gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct StoreClient {
    inner: InnerClient,
}

impl StoreClient {
    /// Creates a new store client with a lazy connection.
    pub fn new(store_url: &Url) -> Self {
        let channel = tonic::transport::Endpoint::try_from(store_url.to_string())
            .expect("valid gRPC endpoint URL")
            .connect_lazy();
        let store = store_client::NtxBuilderClient::with_interceptor(channel, OtelInterceptor);
        info!(target: COMPONENT, store_endpoint = %store_url, "Store client initialized");

        Self { inner: store }
    }

    /// Returns the block header and MMR peaks at the current chain tip.
    #[instrument(target = COMPONENT, name = "store.client.get_latest_blockchain_data_with_retry", skip_all, err)]
    pub async fn get_latest_blockchain_data_with_retry(
        &self,
    ) -> Result<Option<(BlockHeader, PartialMmr)>, StoreError> {
        let mut retry_counter = 0;
        loop {
            match self.get_latest_blockchain_data().await {
                Err(StoreError::GrpcClientError(err)) => {
                    // Exponential backoff with base 500ms and max 30s.
                    let backoff = Duration::from_millis(500)
                        .saturating_mul(1 << retry_counter)
                        .min(Duration::from_secs(30));

                    tracing::warn!(
                        ?backoff,
                        %retry_counter,
                        %err,
                        "store connection failed while fetching latest blockchain data, retrying"
                    );

                    retry_counter += 1;
                    tokio::time::sleep(backoff).await;
                },
                result => return result,
            }
        }
    }

    #[instrument(target = COMPONENT, name = "store.client.get_latest_blockchain_data", skip_all, err)]
    async fn get_latest_blockchain_data(
        &self,
    ) -> Result<Option<(BlockHeader, PartialMmr)>, StoreError> {
        let request = tonic::Request::new(proto::blockchain::MaybeBlockNumber::default());

        let response = self.inner.clone().get_current_blockchain_data(request).await?.into_inner();

        match response.current_block_header {
            // There are new blocks compared to the builder's latest state
            Some(block) => {
                let peaks = try_convert(response.current_peaks).collect::<Result<_, _>>()?;
                let header =
                    BlockHeader::try_from(block).map_err(StoreError::DeserializationError)?;

                let peaks = MmrPeaks::new(Forest::new(header.block_num().as_usize()), peaks)
                    .map_err(|_| {
                        StoreError::MalformedResponse(
                            "returned peaks are not valid for the sent request".into(),
                        )
                    })?;

                let partial_mmr = PartialMmr::from_peaks(peaks);

                Ok(Some((header, partial_mmr)))
            },
            // No new blocks were created, return
            None => Ok(None),
        }
    }

    /// Returns the list of unconsumed network notes.
    #[instrument(target = COMPONENT, name = "store.client.get_unconsumed_network_notes", skip_all, err)]
    pub async fn get_unconsumed_network_notes(&self) -> Result<Vec<NetworkNote>, StoreError> {
        let mut all_notes = Vec::new();
        let mut page_token: Option<u64> = None;

        loop {
            let req = proto::ntx_builder_store::UnconsumedNetworkNotesRequest {
                page_token,
                page_size: 128,
            };
            let resp = self.inner.clone().get_unconsumed_network_notes(req).await?.into_inner();

            let page: Vec<NetworkNote> = resp
                .notes
                .into_iter()
                .map(NetworkNote::try_from)
                .collect::<Result<Vec<_>, _>>()?;

            all_notes.extend(page);

            match resp.next_token {
                Some(tok) => page_token = Some(tok),
                None => break,
            }
        }

        Ok(all_notes)
    }

    #[instrument(target = COMPONENT, name = "store.client.get_network_account", skip_all, err)]
    pub async fn get_network_account(
        &self,
        prefix: NetworkAccountPrefix,
    ) -> Result<Option<Account>, StoreError> {
        let request =
            proto::ntx_builder_store::AccountIdPrefix { account_id_prefix: prefix.inner() };

        let store_response = self
            .inner
            .clone()
            .get_network_account_details_by_prefix(request)
            .await?
            .into_inner()
            .details;

        // we only care about the case where the account returns and is actually a network account,
        // which implies details being public, so OK to error otherwise
        let account = match store_response.map(|acc| acc.details) {
            Some(Some(details)) => Some(Account::read_from_bytes(&details).map_err(|err| {
                StoreError::DeserializationError(ConversionError::deserialization_error(
                    "account", err,
                ))
            })?),
            _ => None,
        };

        Ok(account)
    }
}

// Store errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("gRPC client error")]
    GrpcClientError(#[from] tonic::Status),
    #[error("malformed response from store: {0}")]
    MalformedResponse(String),
    #[error("failed to parse response")]
    DeserializationError(#[from] ConversionError),
}
