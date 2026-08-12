use miden_node_block_producer::store::TransactionInputs;
use miden_node_proto::domain::batch::decode_proposed_batch;
use miden_node_proto::generated as proto;
use miden_node_proto::generated::server::sequencer_api;
use miden_node_utils::ErrorReport;
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_protocol::MIN_PROOF_SECURITY_LEVEL;
use miden_protocol::batch::ProposedBatch;
use tonic::Status;

use super::SequencerInternalService;

#[tonic::async_trait]
impl sequencer_api::SubmitAuthenticatedTxBatch for SequencerInternalService {
    type Input = proto::sequencer::AuthenticatedTransactionBatch;
    type Output = proto::blockchain::BlockNumber;

    fn decode(
        request: proto::sequencer::AuthenticatedTransactionBatch,
    ) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(output: Self::Output) -> tonic::Result<proto::blockchain::BlockNumber> {
        Ok(output)
    }

    async fn handle(
        &self,
        mut request: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        let proposed = request
            .proposed
            .take()
            .ok_or_else(|| Status::invalid_argument("missing `proposed` field"))?;
        let batch: ProposedBatch = spawn_blocking_in_current_span(move || {
            decode_proposed_batch(proposed, MIN_PROOF_SECURITY_LEVEL).map_err(Status::from)
        })
        .await
        .map_err(|err| Status::internal(format!("batch validation task failed: {err}")))??;

        if batch.transactions().len() != request.auth_inputs.len() {
            return Err(Status::invalid_argument(format!(
                "Number of inputs {} does not match number of transactions {} in batch",
                request.auth_inputs.len(),
                batch.transactions().len()
            )));
        }

        let inputs = request
            .auth_inputs
            .into_iter()
            .map(TransactionInputs::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                Status::invalid_argument(err.as_report_context("invalid auth_inputs"))
            })?;

        self.block_producer
            .submit_authenticated_tx_batch(batch, inputs)
            .await
            .map(Into::into)
            .map_err(Into::into)
    }
}
