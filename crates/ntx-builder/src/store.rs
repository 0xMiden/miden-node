use std::time::Duration;

use miden_node_proto::{
    clients::{Builder, StoreNtxBuilder, StoreNtxBuilderClient},
    domain::{account::NetworkAccountPrefix, note::NetworkNote},
    errors::{ConversionError, MissingFieldHelper},
    generated as proto, try_convert,
};
use miden_objects::{
    account::Account,
    block::{BlockHeader, BlockNumber},
    crypto::merkle::{Forest, MmrPeaks, PartialMmr},
};
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
    pub fn new(store_url: &Url) -> Self {
        let store = Builder::new()
            .with_address(store_url.to_string())
            .connect_lazy::<StoreNtxBuilder>()
            .expect("failed to connect to store"); // TODO: handle error

        info!(target: COMPONENT, store_endpoint = %store_url, "Store client initialized");

        Self { inner: store }
    }

    #[instrument(target = COMPONENT, name = "store.client.genesis_header", skip_all, err)]
    pub async fn genesis_header_with_retry(&self) -> Result<BlockHeader, StoreError> {
        let mut retry_counter = 0;
        loop {
            match self.genesis_header().await {
                Err(StoreError::GrpcClientError(err)) => {
                    // exponential backoff with base 500ms and max 30s
                    let backoff = Duration::from_millis(500)
                        .saturating_mul(1 << retry_counter)
                        .min(Duration::from_secs(30));

                    tracing::warn!(
                        ?backoff,
                        %retry_counter,
                        %err,
                        "store connection failed while fetching genesis header, retrying"
                    );

                    retry_counter += 1;
                    tokio::time::sleep(backoff).await;
                },
                result => return result,
            }
        }
    }

    async fn genesis_header(&self) -> Result<BlockHeader, StoreError> {
        let response = self
            .inner
            .clone()
            .get_block_header_by_number(tonic::Request::new(
                proto::shared::BlockHeaderByNumberRequest {
                    block_num: Some(BlockNumber::GENESIS.as_u32()),
                    include_mmr_proof: None,
                },
            ))
            .await?
            .into_inner()
            .block_header
            .ok_or(miden_node_proto::generated::blockchain::BlockHeader::missing_field(
                "block_header",
            ))?;

        BlockHeader::try_from(response).map_err(Into::into)
    }

    /// Returns the block header and MMR peaks at the chain tip if the input `block_num` is either
    /// `None` or an outdated block number. If the input `block_num` is equal to the current
    /// chain tip, this function returns `None`.
    #[instrument(target = COMPONENT, name = "store.client.get_current_blockchain_data", skip_all, err)]
    pub async fn get_current_blockchain_data(
        &self,
        block_num: Option<BlockNumber>,
    ) -> Result<Option<(BlockHeader, PartialMmr)>, StoreError> {
        let request = tonic::Request::new(proto::blockchain::MaybeBlockNumber {
            block_num: block_num.as_ref().map(BlockNumber::as_u32),
        });

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
