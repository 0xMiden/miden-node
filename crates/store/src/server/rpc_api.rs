use miden_node_proto::convert;
use miden_node_proto::domain::account::AccountInfo;
use miden_node_proto::generated::store::rpc_server;
use miden_node_proto::generated::{self as proto};
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::note::NoteId;
use tonic::{Request, Response, Status};
use tracing::{debug, info, instrument};

use crate::COMPONENT;
use crate::constants::{MAX_ACCOUNT_IDS, MAX_NOTE_IDS, MAX_NOTE_TAGS, MAX_NULLIFIERS};
use crate::errors::{
    CheckNullifiersError,
    GetBlockByNumberError,
    GetNoteScriptByRootError,
    GetNotesByIdError,
    NoteSyncError,
    SyncAccountVaultError,
    SyncNullifiersError,
    SyncStorageMapsError,
    SyncTransactionsError,
};
use crate::server::api::{
    StoreApi,
    convert_digests_to_words,
    internal_error,
    read_account_id,
    read_account_ids,
    read_block_range,
    read_root,
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
        request: Request<proto::rpc::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::rpc::BlockHeaderByNumberResponse>, Status> {
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
        request: Request<proto::rpc::NullifierList>,
    ) -> Result<Response<proto::rpc::CheckNullifiersResponse>, Status> {
        // Validate the nullifiers and convert them to Word values. Stop on first error.
        let request = request.into_inner();

        // Validate nullifiers count
        if request.nullifiers.len() > MAX_NULLIFIERS {
            return Err(CheckNullifiersError::TooManyNullifiers(
                request.nullifiers.len(),
                MAX_NULLIFIERS,
            )
            .into());
        }

        let nullifiers = validate_nullifiers::<CheckNullifiersError>(&request.nullifiers)?;

        // Query the state for the request's nullifiers
        let proofs = self.state.check_nullifiers(&nullifiers).await;

        Ok(Response::new(proto::rpc::CheckNullifiersResponse {
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
        request: Request<proto::rpc::SyncNullifiersRequest>,
    ) -> Result<Response<proto::rpc::SyncNullifiersResponse>, Status> {
        let request = request.into_inner();

        if request.prefix_len != 16 {
            return Err(SyncNullifiersError::InvalidPrefixLength(request.prefix_len).into());
        }

        let chain_tip = self.state.latest_block_num().await;
        let block_range =
            read_block_range::<SyncNullifiersError>(request.block_range, "SyncNullifiersRequest")?
                .into_inclusive_range::<SyncNullifiersError>(&chain_tip)?;

        let (nullifiers, block_num) = self
            .state
            .sync_nullifiers(request.prefix_len, request.nullifiers, block_range)
            .await
            .map_err(SyncNullifiersError::from)?;

        let nullifiers = nullifiers
            .into_iter()
            .map(|nullifier_info| proto::rpc::sync_nullifiers_response::NullifierUpdate {
                nullifier: Some(nullifier_info.nullifier.into()),
                block_num: nullifier_info.block_num.as_u32(),
            })
            .collect();

        Ok(Response::new(proto::rpc::SyncNullifiersResponse {
            pagination_info: Some(proto::rpc::PaginationInfo {
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
        request: Request<proto::rpc::SyncStateRequest>,
    ) -> Result<Response<proto::rpc::SyncStateResponse>, Status> {
        let request = request.into_inner();

        let account_ids: Vec<AccountId> = read_account_ids::<Status>(&request.account_ids)?;

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

        Ok(Response::new(proto::rpc::SyncStateResponse {
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
        request: Request<proto::rpc::SyncNotesRequest>,
    ) -> Result<Response<proto::rpc::SyncNotesResponse>, Status> {
        let request = request.into_inner();

        let chain_tip = self.state.latest_block_num().await;
        let block_range =
            read_block_range::<NoteSyncError>(request.block_range, "SyncNotesRequest")?
                .into_inclusive_range::<NoteSyncError>(&chain_tip)?;

        // Validate note tags count
        if request.note_tags.len() > MAX_NOTE_TAGS {
            return Err(
                NoteSyncError::TooManyNoteTags(request.note_tags.len(), MAX_NOTE_TAGS).into()
            );
        }

        let (state, mmr_proof, last_block_included) =
            self.state.sync_notes(request.note_tags, block_range).await?;

        let notes = state.notes.into_iter().map(Into::into).collect();

        Ok(Response::new(proto::rpc::SyncNotesResponse {
            pagination_info: Some(proto::rpc::PaginationInfo {
                chain_tip: chain_tip.as_u32(),
                block_num: last_block_included.as_u32(),
            }),
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

        // Validate note IDs count
        if note_ids.len() > MAX_NOTE_IDS {
            return Err(GetNotesByIdError::TooManyNoteIds(note_ids.len(), MAX_NOTE_IDS).into());
        }

        let note_ids: Vec<Word> = convert_digests_to_words::<GetNotesByIdError, _>(note_ids)?;

        let note_ids: Vec<NoteId> = note_ids.into_iter().map(NoteId::from_raw).collect();

        let notes = self
            .state
            .get_notes_by_id(note_ids)
            .await
            .map_err(GetNotesByIdError::from)?
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
        let account_id = read_account_id::<Status>(Some(request))?;
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

        let block = self
            .state
            .load_block(request.block_num.into())
            .await
            .map_err(GetBlockByNumberError::from)?;

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
        request: Request<proto::rpc::AccountProofRequest>,
    ) -> Result<Response<proto::rpc::AccountProofResponse>, Status> {
        debug!(target: COMPONENT, ?request);
        let request = request.into_inner();
        let account_proof_request = request.try_into()?;

        let proof = self.state.get_account_proof(account_proof_request).await?;

        Ok(Response::new(proof.into()))
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
        request: Request<proto::rpc::SyncAccountVaultRequest>,
    ) -> Result<Response<proto::rpc::SyncAccountVaultResponse>, Status> {
        let request = request.into_inner();
        let chain_tip = self.state.latest_block_num().await;

        let account_id: AccountId = read_account_id::<SyncAccountVaultError>(request.account_id)?;

        if !account_id.is_public() {
            return Err(SyncAccountVaultError::AccountNotPublic(account_id).into());
        }

        let block_range = read_block_range::<SyncAccountVaultError>(
            request.block_range,
            "SyncAccountVaultRequest",
        )?
        .into_inclusive_range::<SyncAccountVaultError>(&chain_tip)?;

        let (last_included_block, updates) = self
            .state
            .sync_account_vault(account_id, block_range)
            .await
            .map_err(SyncAccountVaultError::from)?;

        let updates = updates
            .into_iter()
            .map(|update| {
                let vault_key: Word = update.vault_key.into();
                proto::rpc::AccountVaultUpdate {
                    vault_key: Some(vault_key.into()),
                    asset: update.asset.map(Into::into),
                    block_num: update.block_num.as_u32(),
                }
            })
            .collect();

        Ok(Response::new(proto::rpc::SyncAccountVaultResponse {
            pagination_info: Some(proto::rpc::PaginationInfo {
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
        request: Request<proto::rpc::SyncStorageMapsRequest>,
    ) -> Result<Response<proto::rpc::SyncStorageMapsResponse>, Status> {
        let request = request.into_inner();

        let account_id = read_account_id::<SyncStorageMapsError>(request.account_id)?;

        if !account_id.is_public() {
            Err(SyncStorageMapsError::AccountNotPublic(account_id))?;
        }

        let chain_tip = self.state.latest_block_num().await;
        let block_range = read_block_range::<SyncStorageMapsError>(
            request.block_range,
            "SyncStorageMapsRequest",
        )?
        .into_inclusive_range::<SyncStorageMapsError>(&chain_tip)?;

        let storage_maps_page = self
            .state
            .get_storage_map_sync_values(account_id, block_range)
            .await
            .map_err(SyncStorageMapsError::from)?;

        let updates = storage_maps_page
            .values
            .into_iter()
            .map(|map_value| proto::rpc::StorageMapUpdate {
                slot_name: map_value.slot_name.to_string(),
                key: Some(map_value.key.into()),
                value: Some(map_value.value.into()),
                block_num: map_value.block_num.as_u32(),
            })
            .collect();

        Ok(Response::new(proto::rpc::SyncStorageMapsResponse {
            pagination_info: Some(proto::rpc::PaginationInfo {
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
    ) -> Result<Response<proto::rpc::StoreStatus>, Status> {
        Ok(Response::new(proto::rpc::StoreStatus {
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
    ) -> Result<Response<proto::rpc::MaybeNoteScript>, Status> {
        debug!(target: COMPONENT, request = ?request);

        let root = read_root::<GetNoteScriptByRootError>(request.into_inner().root, "NoteRoot")?;

        let note_script = self
            .state
            .get_note_script_by_root(root)
            .await
            .map_err(GetNoteScriptByRootError::from)?;

        Ok(Response::new(proto::rpc::MaybeNoteScript {
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
        request: Request<proto::rpc::SyncTransactionsRequest>,
    ) -> Result<Response<proto::rpc::SyncTransactionsResponse>, Status> {
        debug!(target: COMPONENT, request = ?request);

        let request = request.into_inner();

        let chain_tip = self.state.latest_block_num().await;
        let block_range = read_block_range::<SyncTransactionsError>(
            request.block_range,
            "SyncTransactionsRequest",
        )?
        .into_inclusive_range::<SyncTransactionsError>(&chain_tip)?;

        let account_ids: Vec<AccountId> =
            read_account_ids::<SyncTransactionsError>(&request.account_ids)?;

        // Validate account IDs count
        if account_ids.len() > MAX_ACCOUNT_IDS {
            return Err(SyncTransactionsError::TooManyAccountIds(
                account_ids.len(),
                MAX_ACCOUNT_IDS,
            )
            .into());
        }

        let (last_block_included, transaction_records_db) = self
            .state
            .sync_transactions(account_ids, block_range.clone())
            .await
            .map_err(SyncTransactionsError::from)?;

        // Convert database records (already joined with their notes) into proto TransactionRecord
        let transactions = transaction_records_db.into_iter().map(Into::into).collect();

        Ok(Response::new(proto::rpc::SyncTransactionsResponse {
            pagination_info: Some(proto::rpc::PaginationInfo {
                chain_tip: chain_tip.as_u32(),
                block_num: last_block_included.as_u32(),
            }),
            transactions,
        }))
    }
}
