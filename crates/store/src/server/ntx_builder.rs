use std::num::{NonZero, TryFromIntError};

use miden_node_proto::{
    domain::account::{AccountInfo, NetworkAccountPrefix},
    generated::{self as proto, ntx_builder_store::ntx_builder_server},
};
use miden_node_utils::ErrorReport;
use miden_objects::{block::BlockNumber, note::Note};
use tonic::{Request, Response, Status};
use tracing::instrument;

use crate::{
    COMPONENT,
    db::models::Page,
    server::api::{StoreApi, internal_error, invalid_argument},
};

// NTX BUILDER ENDPOINTS
// ================================================================================================

#[tonic::async_trait]
impl ntx_builder_server::NtxBuilder for StoreApi {
    /// Returns block header for the specified block number.
    ///
    /// If the block number is not provided, block header for the latest block is returned.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.ntx_builder_server.get_block_header_by_number",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_block_header_by_number(
        &self,
        request: Request<proto::shared::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::shared::BlockHeaderByNumberResponse>, Status> {
        self.get_block_header_by_number_inner(request).await
    }

    /// Returns the chain tip's header and MMR peaks corresponding to that header.
    /// If there are N blocks, the peaks will represent the MMR at block `N - 1`.
    ///
    /// This returns all the blockchain-related information needed for executing transactions
    /// without authenticating notes.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.ntx_builder_server.get_current_blockchain_data",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_current_blockchain_data(
        &self,
        request: Request<proto::blockchain::MaybeBlockNumber>,
    ) -> Result<Response<proto::ntx_builder_store::CurrentBlockchainData>, Status> {
        let block_num = request.into_inner().block_num.map(BlockNumber::from);

        let response = match self
            .state
            .get_current_blockchain_data(block_num)
            .await
            .map_err(internal_error)?
        {
            Some((header, peaks)) => proto::ntx_builder_store::CurrentBlockchainData {
                current_peaks: peaks.peaks().iter().map(Into::into).collect(),
                current_block_header: Some(header.into()),
            },
            None => proto::ntx_builder_store::CurrentBlockchainData {
                current_peaks: vec![],
                current_block_header: None,
            },
        };

        Ok(Response::new(response))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.ntx_builder_server.get_network_account_details_by_prefix",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_network_account_details_by_prefix(
        &self,
        request: Request<proto::ntx_builder_store::AccountIdPrefix>,
    ) -> Result<Response<proto::ntx_builder_store::MaybeAccountDetails>, Status> {
        let request = request.into_inner();

        // Validate that the call is for a valid network account prefix
        let prefix = NetworkAccountPrefix::try_from(request.account_id_prefix).map_err(|err| {
            Status::invalid_argument(
                err.as_report_context("request does not contain a valid network account prefix"),
            )
        })?;
        let account_info: Option<AccountInfo> =
            self.state.get_network_account_details_by_prefix(prefix.inner()).await?;

        Ok(Response::new(proto::ntx_builder_store::MaybeAccountDetails {
            details: account_info.map(|acc| (&acc).into()),
        }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.ntx_builder_server.get_unconsumed_network_notes",
        skip_all,
        err
    )]
    async fn get_unconsumed_network_notes(
        &self,
        request: Request<proto::ntx_builder_store::UnconsumedNetworkNotesRequest>,
    ) -> Result<Response<proto::ntx_builder_store::UnconsumedNetworkNotes>, Status> {
        let request = request.into_inner();
        let state = self.state.clone();

        let size =
            NonZero::try_from(request.page_size as usize).map_err(|err: TryFromIntError| {
                invalid_argument(err.as_report_context("invalid page_size"))
            })?;
        let page = Page { token: request.page_token, size };
        // TODO: no need to get the whole NoteRecord here, a NetworkNote wrapper should be created
        // instead
        let (notes, next_page) =
            state.get_unconsumed_network_notes(page).await.map_err(internal_error)?;

        let mut network_notes = Vec::with_capacity(notes.len());
        for note in notes {
            // SAFETY: Network notes are filtered in the database, so they should have details;
            // otherwise the state would be corrupted
            let (assets, recipient) = note.details.unwrap().into_parts();
            let note = Note::new(assets, note.metadata, recipient);
            network_notes.push(note.into());
        }

        Ok(Response::new(proto::ntx_builder_store::UnconsumedNetworkNotes {
            notes: network_notes,
            next_token: next_page.token,
        }))
    }
}
