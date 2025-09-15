use miden_node_proto::domain::account::AccountInfo;
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::rpc_store::rpc_server;
use miden_node_proto::generated::{self as proto};
use miden_node_proto::{convert, try_convert};
use miden_node_utils::ErrorReport as _;
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::note::NoteId;
use tonic::{Request, Response, Status};
use tracing::{debug, info, instrument};

use crate::COMPONENT;
use crate::server::api::{
    StoreApi,
    internal_error,
    invalid_argument,
    read_account_id,
    read_account_ids,
    validate_nullifiers,
};

// CLIENT ENDPOINTS
// ================================================================================================

#[tonic::async_trait]
impl rpc_server::Rpc for StoreApi {
    /// Returns block header for the specified block number.
    ///
    /// If the block number is not provided, block header for the latest block is returned.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_block_header_by_number",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_block_header_by_number(
        &self,
        request: Request<proto::shared::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::shared::BlockHeaderByNumberResponse>, Status> {
        self.get_block_header_by_number_inner(request).await
    }

    /// Returns info on whether the specified nullifiers have been consumed.
    ///
    /// This endpoint also returns Merkle authentication path for each requested nullifier which can
    /// be verified against the latest root of the nullifier database.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.check_nullifiers",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn check_nullifiers(
        &self,
        request: Request<proto::rpc_store::NullifierList>,
    ) -> Result<Response<proto::rpc_store::CheckNullifiersResponse>, Status> {
        // Validate the nullifiers and convert them to Word values. Stop on first error.
        let request = request.into_inner();

        let nullifiers = validate_nullifiers(&request.nullifiers)?;

        // Query the state for the request's nullifiers
        let proofs = self.state.check_nullifiers(&nullifiers).await;

        Ok(Response::new(proto::rpc_store::CheckNullifiersResponse {
            proofs: convert(proofs).collect(),
        }))
    }

    /// Returns nullifiers that match the specified prefixes and have been consumed.
    ///
    /// Currently the only supported prefix length is 16 bits.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_nullifiers",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_nullifiers(
        &self,
        request: Request<proto::rpc_store::SyncNullifiersRequest>,
    ) -> Result<Response<proto::rpc_store::SyncNullifiersResponse>, Status> {
        let request = request.into_inner();

        if request.prefix_len != 16 {
            return Err(Status::invalid_argument("Only 16-bit prefixes are supported"));
        }

        let chain_tip = self.state.latest_block_num().await;
        let block_range = request
            .block_range
            .ok_or(invalid_argument("block_range is required"))?
            .into_inclusive_range(chain_tip);

        let (nullifiers, block_num) = self
            .state
            .sync_nullifiers(request.prefix_len, request.nullifiers, block_range)
            .await?;
        let nullifiers = nullifiers
            .into_iter()
            .map(|nullifier_info| proto::rpc_store::sync_nullifiers_response::NullifierUpdate {
                nullifier: Some(nullifier_info.nullifier.into()),
                block_num: nullifier_info.block_num.as_u32(),
            })
            .collect();

        Ok(Response::new(proto::rpc_store::SyncNullifiersResponse {
            pagination_info: Some(proto::rpc_store::PaginationInfo {
                chain_tip: chain_tip.as_u32(),
                block_num: block_num.as_u32(),
            }),
            nullifiers,
        }))
    }

    /// Returns info which can be used by the client to sync up to the latest state of the chain
    /// for the objects the client is interested in.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_state",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_state(
        &self,
        request: Request<proto::rpc_store::SyncStateRequest>,
    ) -> Result<Response<proto::rpc_store::SyncStateResponse>, Status> {
        let request = request.into_inner();

        let account_ids: Vec<AccountId> = read_account_ids(&request.account_ids)?;

        let (state, delta) = self
            .state
            .sync_state(request.block_num.into(), account_ids, request.note_tags)
            .await
            .map_err(internal_error)?;

        let accounts = state
            .account_updates
            .into_iter()
            .map(|account_info| proto::account::AccountSummary {
                account_id: Some(account_info.account_id.into()),
                account_commitment: Some(account_info.account_commitment.into()),
                block_num: account_info.block_num.as_u32(),
            })
            .collect();

        let transactions = state
            .transactions
            .into_iter()
            .map(|transaction_summary| proto::transaction::TransactionSummary {
                account_id: Some(transaction_summary.account_id.into()),
                block_num: transaction_summary.block_num.as_u32(),
                transaction_id: Some(transaction_summary.transaction_id.into()),
            })
            .collect();

        let notes = state.notes.into_iter().map(Into::into).collect();

        Ok(Response::new(proto::rpc_store::SyncStateResponse {
            chain_tip: self.state.latest_block_num().await.as_u32(),
            block_header: Some(state.block_header.into()),
            mmr_delta: Some(delta.into()),
            accounts,
            transactions,
            notes,
        }))
    }

    /// Returns info which can be used by the client to sync note state.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_notes",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_notes(
        &self,
        request: Request<proto::rpc_store::SyncNotesRequest>,
    ) -> Result<Response<proto::rpc_store::SyncNotesResponse>, Status> {
        let request = request.into_inner();

        let (state, mmr_proof) = self
            .state
            .sync_notes(request.block_num.into(), request.note_tags)
            .await
            .map_err(internal_error)?;

        let notes = state.notes.into_iter().map(Into::into).collect();

        Ok(Response::new(proto::rpc_store::SyncNotesResponse {
            chain_tip: self.state.latest_block_num().await.as_u32(),
            block_header: Some(state.block_header.into()),
            mmr_path: Some(mmr_proof.merkle_path.into()),
            notes,
        }))
    }

    /// Returns a list of [`Note`]s for the specified [`NoteId`]s.
    ///
    /// If the list is empty or no [`Note`] matched the requested [`NoteId`] and empty list is
    /// returned.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_notes_by_id",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_notes_by_id(
        &self,
        request: Request<proto::note::NoteIdList>,
    ) -> Result<Response<proto::note::CommittedNoteList>, Status> {
        info!(target: COMPONENT, ?request);

        let note_ids = request.into_inner().ids;

        let note_ids: Vec<Word> = try_convert(note_ids)
            .collect::<Result<_, _>>()
            .map_err(|err| Status::invalid_argument(format!("Invalid NoteId: {err}")))?;

        let note_ids: Vec<NoteId> = note_ids.into_iter().map(From::from).collect();

        let notes = self
            .state
            .get_notes_by_id(note_ids)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        Ok(Response::new(proto::note::CommittedNoteList { notes }))
    }

    /// Returns details for public (public) account by id.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_account_details",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_account_details(
        &self,
        request: Request<proto::account::AccountId>,
    ) -> Result<Response<proto::account::AccountDetails>, Status> {
        let request = request.into_inner();
        let account_id = read_account_id(Some(request)).map_err(|err| *err)?;
        let account_info: AccountInfo = self.state.get_account_details(account_id).await?;

        // TODO: revisit this, previous implementation was just returning only the summary, but it
        // is weird since the details are not empty.
        Ok(Response::new((&account_info).into()))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_block_by_number",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_block_by_number(
        &self,
        request: Request<proto::blockchain::BlockNumber>,
    ) -> Result<Response<proto::blockchain::MaybeBlock>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        let block = self.state.load_block(request.block_num.into()).await?;

        Ok(Response::new(proto::blockchain::MaybeBlock { block }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_account_proof",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_account_proof(
        &self,
        request: Request<proto::rpc_store::AccountProofRequest>,
    ) -> Result<Response<proto::rpc_store::AccountProof>, Status> {
        debug!(target: COMPONENT, ?request);
        let request = request.into_inner();
        let account_request = request.try_into()?;

        let proof = self.state.get_account_proof(account_request).await?;

        Ok(Response::new(proto::rpc_store::AccountProof::from(proof)))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_account_vault",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_account_vault(
        &self,
        request: Request<proto::rpc_store::SyncAccountVaultRequest>,
    ) -> Result<Response<proto::rpc_store::SyncAccountVaultResponse>, Status> {
        let request = request.into_inner();
        let chain_tip = self.state.latest_block_num().await;

        let account_id: AccountId = read_account_id(request.account_id).map_err(|e| *e)?;
        if !account_id.is_public() {
            return Err(Status::invalid_argument(format!(
                "account with ID {account_id} is not public",
            )));
        }

        let block_range = request.block_range.ok_or(invalid_argument("block_range is required"))?;

        let block_range = block_range.into_inclusive_range(chain_tip);

        if block_range.end() > &chain_tip {
            return Err(Status::invalid_argument("block_to cannot be higher than the chain tip"));
        }

        let (last_included_block, updates) = self
            .state
            .sync_account_vault(account_id, block_range)
            .await
            .map_err(internal_error)?;

        let updates = updates
            .into_iter()
            .map(|update| proto::rpc_store::AccountVaultUpdate {
                vault_key: Some(update.vault_key.into()),
                asset: update.asset.map(Into::into),
                block_num: update.block_num.as_u32(),
            })
            .collect();

        Ok(Response::new(proto::rpc_store::SyncAccountVaultResponse {
            pagination_info: Some(proto::rpc_store::PaginationInfo {
                chain_tip: chain_tip.as_u32(),
                block_num: last_included_block.as_u32(),
            }),
            updates,
        }))
    }

    /// Returns storage map updates for the specified account within a block range.
    ///
    /// Supports cursor-based pagination for large storage maps.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_storage_maps",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_storage_maps(
        &self,
        request: Request<proto::rpc_store::SyncStorageMapsRequest>,
    ) -> Result<Response<proto::rpc_store::SyncStorageMapsResponse>, Status> {
        let request = request.into_inner();

        let account_id = read_account_id(request.account_id).map_err(|e| *e)?;

        if !account_id.is_public() {
            return Err(Status::invalid_argument(format!(
                "account with ID {account_id} is not public"
            )));
        }

        let chain_tip = self.state.latest_block_num().await;
        let block_range = request
            .block_range
            .ok_or(invalid_argument("block_range is required"))?
            .into_inclusive_range(chain_tip);

        if block_range.start() > block_range.end() {
            return Err(Status::invalid_argument("block_from cannot be greater than block_to"));
        }

        let storage_maps_page = self
            .state
            .get_storage_map_sync_values(account_id, block_range)
            .await
            .map_err(internal_error)?;

        let updates = storage_maps_page
            .values
            .into_iter()
            .map(|map_value| proto::rpc_store::StorageMapUpdate {
                slot_index: u32::from(map_value.slot_index),
                key: Some(map_value.key.into()),
                value: Some(map_value.value.into()),
                block_num: map_value.block_num.as_u32(),
            })
            .collect();

        Ok(Response::new(proto::rpc_store::SyncStorageMapsResponse {
            pagination_info: Some(proto::rpc_store::PaginationInfo {
                chain_tip: chain_tip.as_u32(),
                block_num: storage_maps_page.last_block_included.as_u32(),
            }),
            updates,
        }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.status",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn status(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::rpc_store::StoreStatus>, Status> {
        Ok(Response::new(proto::rpc_store::StoreStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            status: "connected".to_string(),
            chain_tip: self.state.latest_block_num().await.as_u32(),
        }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_note_script_by_root",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_note_script_by_root(
        &self,
        request: Request<proto::note::NoteRoot>,
    ) -> Result<Response<proto::rpc_store::MaybeNoteScript>, Status> {
        debug!(target: COMPONENT, request = ?request);

        let root = request
            .into_inner()
            .root
            .ok_or(invalid_argument("missing root"))?
            .try_into()
            .map_err(|err: ConversionError| {
                invalid_argument(err.as_report_context("invalid root"))
            })?;

        let note_script = self.state.get_note_script_by_root(root).await.map_err(internal_error)?;

        Ok(Response::new(proto::rpc_store::MaybeNoteScript {
            script: note_script.map(Into::into),
        }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_transactions",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_transactions(
        &self,
        request: Request<proto::rpc_store::SyncTransactionsRequest>,
    ) -> Result<Response<proto::rpc_store::SyncTransactionsResponse>, Status> {
        debug!(target: COMPONENT, request = ?request);

        let request = request.into_inner();

        let chain_tip = self.state.latest_block_num().await;
        let block_range = request.block_range.ok_or(invalid_argument("block_range is required"))?;
        let block_range = block_range.into_inclusive_range(chain_tip);

        let account_ids: Vec<AccountId> = read_account_ids(&request.account_ids)?;

        let (last_block_included, transaction_headers_db) = self
            .state
            .sync_transactions(account_ids, block_range.clone())
            .await
            .map_err(internal_error)?;

        // Convert database TransactionHeader to proto TransactionHeader
        let mut transaction_headers = Vec::new();

        for tx_header in transaction_headers_db {
            // Retrieve full note data for output notes from the database
            let note_records = self
                .state
                .get_notes_by_id(tx_header.output_notes.clone())
                .await
                .map_err(internal_error)?;

            // Convert NoteRecord to NoteSyncRecord
            let note_sync_records: Vec<crate::db::NoteSyncRecord> =
                note_records.into_iter().map(std::convert::Into::into).collect();

            // Convert to proto using the helper method
            let proto_header = tx_header.to_proto_with_note_records(note_sync_records);
            transaction_headers.push(proto_header);
        }

        Ok(Response::new(proto::rpc_store::SyncTransactionsResponse {
            pagination_info: Some(proto::rpc_store::PaginationInfo {
                chain_tip: chain_tip.as_u32(),
                block_num: last_block_included.as_u32(),
            }),
            transaction_headers,
        }))
    }
}
