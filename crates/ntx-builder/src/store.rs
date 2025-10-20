use std::time::Duration;

use miden_node_proto::clients::{Builder, StoreNtxBuilder, StoreNtxBuilderClient};
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::note::NetworkNote;
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::{self as proto};
use miden_node_proto::try_convert;
use miden_objects::account::{Account, AccountId};
use miden_objects::block::BlockHeader;
use miden_objects::crypto::merkle::{Forest, MmrPeaks, PartialMmr};
use miden_tx::utils::Deserializable;
use thiserror::Error;
use tracing::{info, instrument};
use url::Url;

use crate::COMPONENT;

// STORE CLIENT
// ================================================================================================

/// Interface to the store's ntx-builder gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct StoreClient {
    inner: StoreNtxBuilderClient,
}

impl StoreClient {
    /// Creates a new store client with a lazy connection.
    pub fn new(store_url: Url) -> Self {
        info!(target: COMPONENT, store_endpoint = %store_url, "Initializing store client");

        let store = Builder::new(store_url)
            .without_tls()
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .connect_lazy::<StoreNtxBuilder>();

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

    /// Returns the list of unconsumed network notes for a specific network account up to a
    /// specified block.
    #[instrument(target = COMPONENT, name = "store.client.get_unconsumed_network_notes_for_account", skip_all, err)]
    pub async fn get_unconsumed_network_notes_for_account(
        &self,
        network_account_prefix: NetworkAccountPrefix,
        block_num: u32,
    ) -> Result<Vec<NetworkNote>, StoreError> {
        let mut all_notes = Vec::new();
        let mut page_token: Option<u64> = None;

        let mut store_client = self.inner.clone();
        loop {
            let req = proto::ntx_builder_store::UnconsumedNetworkNotesForAccountRequest {
                page_token,
                page_size: 128,
                network_account_id_prefix: network_account_prefix.inner(),
                block_num,
            };
            let resp =
                store_client.get_unconsumed_network_notes_for_account(req).await?.into_inner();

            let page: Vec<NetworkNote> = resp
                .notes
                .into_iter()
                .map(NetworkNote::try_from)
                .collect::<Result<Vec<_>, _>>()?;

            all_notes.extend(page);

            match resp.next_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }

        Ok(all_notes)
    }

    // TODO: add pagination.
    #[instrument(target = COMPONENT, name = "store.client.get_network_accounts", skip_all, err)]
    pub async fn get_network_account_ids(&self) -> Result<Vec<AccountId>, StoreError> {
        let response = self.inner.clone().get_network_account_ids(()).await?.into_inner();

        let accounts: Result<Vec<AccountId>, ConversionError> = response
            .account_ids
            .into_iter()
            .map(|account_id| {
                AccountId::read_from_bytes(&account_id.id)
                    .map_err(|err| ConversionError::deserialization_error("account_id", err))
            })
            .collect();

        Ok(accounts?)
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
