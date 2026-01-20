use std::ops::RangeInclusive;
use std::time::Duration;

use miden_node_proto::clients::{Builder, StoreNtxBuilderClient};
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::note::NetworkNote;
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::rpc::BlockRange;
use miden_node_proto::generated::{self as proto};
use miden_node_proto::try_convert;
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::{Forest, MmrPeaks, PartialMmr};
use miden_protocol::note::NoteScript;
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
            .with_otel_context_injection()
            .connect_lazy::<StoreNtxBuilderClient>();

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
                        .saturating_mul(1 << retry_counter.min(6))
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
        let request = proto::store::AccountIdPrefix { account_id_prefix: prefix.inner() };

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
    #[instrument(target = COMPONENT, name = "store.client.get_unconsumed_network_notes", skip_all, err)]
    pub async fn get_unconsumed_network_notes(
        &self,
        network_account_prefix: NetworkAccountPrefix,
        block_num: u32,
    ) -> Result<Vec<NetworkNote>, StoreError> {
        // Upper bound of each note is ~10KB. Limit page size to ~10MB.
        const PAGE_SIZE: u64 = 1024;

        let mut all_notes = Vec::new();
        let mut page_token: Option<u64> = None;

        let mut store_client = self.inner.clone();
        loop {
            let req = proto::store::UnconsumedNetworkNotesRequest {
                page_token,
                page_size: PAGE_SIZE,
                network_account_id_prefix: network_account_prefix.inner(),
                block_num,
            };
            let resp = store_client.get_unconsumed_network_notes(req).await?.into_inner();

            all_notes.reserve(resp.notes.len());
            for note in resp.notes {
                all_notes.push(NetworkNote::try_from(note)?);
            }

            match resp.next_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }

        Ok(all_notes)
    }

    /// Streams network account IDs to the provided sender.
    ///
    /// This method is designed to be run in a background task, sending accounts to the main event
    /// loop as they are loaded. This allows the ntx-builder to start processing mempool events
    /// without waiting for all accounts to be preloaded.
    pub async fn stream_network_account_ids(
        &self,
        sender: tokio::sync::mpsc::Sender<NetworkAccountPrefix>,
    ) -> Result<(), StoreError> {
        let mut block_range = BlockNumber::from(0)..=BlockNumber::from(u32::MAX);

        while let Some(next_start) = self.load_page(block_range, &sender).await? {
            block_range = next_start..=BlockNumber::from(u32::MAX);
        }

        Ok(())
    }

    /// Loads a single page of network accounts and submits them to the sender.
    ///
    /// Returns the next block number to fetch from, or `None` if the chain tip has been reached.
    #[instrument(target = COMPONENT, name = "store.client.load_accounts_page", skip_all, err)]
    async fn load_page(
        &self,
        block_range: RangeInclusive<BlockNumber>,
        sender: &tokio::sync::mpsc::Sender<NetworkAccountPrefix>,
    ) -> Result<Option<BlockNumber>, StoreError> {
        let (accounts, pagination_info) = self.fetch_page(block_range).await?;

        let chain_tip = pagination_info.chain_tip;
        let current_height = pagination_info.block_num;

        self.submit_page(accounts, sender).await?;

        if current_height >= chain_tip {
            Ok(None)
        } else {
            Ok(Some(BlockNumber::from(current_height)))
        }
    }

    #[instrument(target = COMPONENT, name = "store.client.fetch_page", skip_all, err)]
    async fn fetch_page(
        &self,
        block_range: std::ops::RangeInclusive<BlockNumber>,
    ) -> Result<(Vec<NetworkAccountPrefix>, proto::rpc::PaginationInfo), StoreError> {
        let mut retry_counter = 0u32;

        let response = loop {
            match self
                .inner
                .clone()
                .get_network_account_ids(Into::<BlockRange>::into(block_range.clone()))
                .await
            {
                Ok(response) => break response.into_inner(),
                Err(err) => {
                    // Exponential backoff with base 500ms and max 30s.
                    let backoff = Duration::from_millis(500)
                        .saturating_mul(1 << retry_counter.min(6))
                        .min(Duration::from_secs(30));

                    tracing::warn!(
                        ?backoff,
                        %retry_counter,
                        %err,
                        "store connection failed while fetching committed accounts page, retrying"
                    );

                    retry_counter += 1;
                    tokio::time::sleep(backoff).await;
                },
            }
        };

        let accounts = response
            .account_ids
            .into_iter()
            .map(|account_id| {
                let account_id = AccountId::read_from_bytes(&account_id.id).map_err(|err| {
                    StoreError::DeserializationError(ConversionError::deserialization_error(
                        "account_id",
                        err,
                    ))
                })?;
                NetworkAccountPrefix::try_from(account_id).map_err(|_| {
                    StoreError::MalformedResponse(
                        "account id is not a valid network account".into(),
                    )
                })
            })
            .collect::<Result<Vec<NetworkAccountPrefix>, StoreError>>()?;

        let pagination_info = response.pagination_info.ok_or(
            ConversionError::MissingFieldInProtobufRepresentation {
                entity: "NetworkAccountIdList",
                field_name: "pagination_info",
            },
        )?;

        Ok((accounts, pagination_info))
    }

    #[instrument(
        target = COMPONENT,
        name = "store.client.submit_page",
        skip_all
    )]
    async fn submit_page(
        &self,
        accounts: Vec<NetworkAccountPrefix>,
        sender: &tokio::sync::mpsc::Sender<NetworkAccountPrefix>,
    ) -> Result<(), StoreError> {
        for account in accounts {
            // If the receiver is dropped, stop loading.
            if sender.send(account).await.is_err() {
                tracing::warn!("Account receiver dropped");
                return Ok(());
            }
        }

        Ok(())
    }

    #[instrument(target = COMPONENT, name = "store.client.get_note_script_by_root", skip_all, err)]
    pub async fn get_note_script_by_root(
        &self,
        root: Word,
    ) -> Result<Option<NoteScript>, StoreError> {
        let request = proto::note::NoteRoot { root: Some(root.into()) };

        // Make the request to the store.
        let script = self.inner.clone().get_note_script_by_root(request).await?.into_inner().script;

        // Handle result.
        if let Some(script) = script {
            // Deserialize the script.
            let script = NoteScript::read_from_bytes(&script.mast).map_err(|err| {
                StoreError::DeserializationError(ConversionError::deserialization_error(
                    "note script",
                    err,
                ))
            })?;
            Ok(Some(script))
        } else {
            Ok(None)
        }
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
