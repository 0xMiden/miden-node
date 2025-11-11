use std::convert::Infallible;

use miden_node_proto::generated::block_producer_store::block_producer_server;
use miden_node_proto::generated::{self as proto};
use miden_node_proto::try_convert;
use miden_node_utils::ErrorReport;
use miden_objects::Word;
use miden_objects::block::{BlockNumber, ProvenBlock};
use miden_objects::utils::Deserializable;
use tonic::{Request, Response, Status};
use tracing::{debug, instrument};

use crate::COMPONENT;
use crate::server::api::{
    StoreApi,
    conversion_error_to_status,
    internal_error,
    read_account_id,
    read_account_ids,
    read_block_numbers,
    validate_note_commitments,
    validate_nullifiers,
};

// BLOCK PRODUCER ENDPOINTS
// ================================================================================================

#[tonic::async_trait]
impl block_producer_server::BlockProducer for StoreApi {
    /// Returns block header for the specified block number.
    ///
    /// If the block number is not provided, block header for the latest block is returned.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.block_producer_server.get_block_header_by_number",
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

    /// Updates the local DB by inserting a new block header and the related data.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.block_producer_server.apply_block",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn apply_block(
        &self,
        request: Request<proto::blockchain::Block>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        let block = ProvenBlock::read_from_bytes(&request.block).map_err(|err| {
            Status::invalid_argument(err.as_report_context("block deserialization error"))
        })?;

        let block_num = block.header().block_num().as_u32();

        debug!(
            target: COMPONENT,
            block_num,
            block_commitment = %block.commitment(),
            account_count = block.updated_accounts().len(),
            note_count = block.output_notes().count(),
            nullifier_count = block.created_nullifiers().len(),
        );

        self.state.apply_block(block).await?;

        Ok(Response::new(()))
    }

    /// Returns data needed by the block producer to construct and prove the next block.
    #[instrument(
            parent = None,
            target = COMPONENT,
            name = "store.block_producer_server.get_block_inputs",
            skip_all,
            ret(level = "debug"),
            err
        )]
    async fn get_block_inputs(
        &self,
        request: Request<proto::block_producer_store::BlockInputsRequest>,
    ) -> Result<Response<proto::block_producer_store::BlockInputs>, Status> {
        let request = request.into_inner();

        let account_ids = read_account_ids::<Status>(&request.account_ids)?;
        let nullifiers = validate_nullifiers(&request.nullifiers)
            .map_err(|err| conversion_error_to_status(&err))?;
        let unauthenticated_note_commitments =
            validate_note_commitments(&request.unauthenticated_notes)?;
        let reference_blocks = read_block_numbers(&request.reference_blocks);
        let unauthenticated_note_commitments =
            unauthenticated_note_commitments.into_iter().collect();

        self.state
            .get_block_inputs(
                account_ids,
                nullifiers,
                unauthenticated_note_commitments,
                reference_blocks,
            )
            .await
            .map(proto::block_producer_store::BlockInputs::from)
            .map(Response::new)
            .map_err(internal_error)
    }

    /// Fetches the inputs for a transaction batch from the database.
    ///
    /// See [`State::get_batch_inputs`] for details.
    #[instrument(
          parent = None,
          target = COMPONENT,
          name = "store.block_producer_server.get_batch_inputs",
          skip_all,
          ret(level = "debug"),
          err
        )]
    async fn get_batch_inputs(
        &self,
        request: Request<proto::block_producer_store::BatchInputsRequest>,
    ) -> Result<Response<proto::block_producer_store::BatchInputs>, Status> {
        let request = request.into_inner();

        let note_commitments: Vec<Word> = try_convert(request.note_commitments)
            .collect::<Result<_, _>>()
            .map_err(|err| Status::invalid_argument(format!("Invalid note commitment: {err}")))?;

        let reference_blocks: Vec<u32> =
            try_convert::<_, Infallible, _, _>(request.reference_blocks)
                .collect::<Result<Vec<_>, _>>()
                .expect("operation should be infallible");
        let reference_blocks = reference_blocks.into_iter().map(BlockNumber::from).collect();

        self.state
            .get_batch_inputs(reference_blocks, note_commitments.into_iter().collect())
            .await
            .map(Into::into)
            .map(Response::new)
            .map_err(internal_error)
    }

    #[instrument(
            parent = None,
            target = COMPONENT,
            name = "store.block_producer_server.get_transaction_inputs",
            skip_all,
            ret(level = "debug"),
            err
        )]
    async fn get_transaction_inputs(
        &self,
        request: Request<proto::block_producer_store::TransactionInputsRequest>,
    ) -> Result<Response<proto::block_producer_store::TransactionInputs>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        let account_id = read_account_id::<Status>(request.account_id)?;
        let nullifiers = validate_nullifiers(&request.nullifiers)
            .map_err(|err| conversion_error_to_status(&err))?;
        let unauthenticated_note_commitments =
            validate_note_commitments(&request.unauthenticated_notes)?;

        let tx_inputs = self
            .state
            .get_transaction_inputs(account_id, &nullifiers, unauthenticated_note_commitments)
            .await?;

        let block_height = self.state.latest_block_num().await.as_u32();

        Ok(Response::new(proto::block_producer_store::TransactionInputs {
            account_state: Some(proto::block_producer_store::transaction_inputs::AccountTransactionInputRecord {
                account_id: Some(account_id.into()),
                account_commitment: Some(tx_inputs.account_commitment.into()),
            }),
            nullifiers: tx_inputs
                .nullifiers
                .into_iter()
                .map(|nullifier| proto::block_producer_store::transaction_inputs::NullifierTransactionInputRecord {
                    nullifier: Some(nullifier.nullifier.into()),
                    block_num: nullifier.block_num.as_u32(),
                })
                .collect(),
            found_unauthenticated_notes: tx_inputs
                .found_unauthenticated_notes
                .into_iter()
                .map(Into::into)
                .collect(),
            new_account_id_prefix_is_unique: tx_inputs.new_account_id_prefix_is_unique,
            block_height,
        }))
    }
}
