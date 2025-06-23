use miden_block_prover::LocalBlockProver;
use miden_objects::{
    MIN_PROOF_SECURITY_LEVEL, batch::ProposedBatch, block::ProposedBlock,
    transaction::TransactionWitness, utils::Serializable,
};
use miden_tx::{LocalTransactionProver, TransactionProver};
use miden_tx_batch_prover::LocalBatchProver;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{info, instrument};

use crate::{
    COMPONENT,
    commands::worker::ProverType,
    generated::{ProofType, ProvingRequest, ProvingResponse, api_server::Api as ProverApi},
};

/// The prover for the proving service.
///
/// This enum is used to store the prover for the proving service.
/// Only one prover is enabled at a time.
enum Prover {
    Transaction(Mutex<LocalTransactionProver>),
    Batch(Mutex<LocalBatchProver>),
    Block(Mutex<LocalBlockProver>),
}

impl Prover {
    fn new(prover_type: ProverType) -> Self {
        match prover_type {
            ProverType::Transaction => {
                info!(target: COMPONENT, prover_type = ?prover_type, "Transaction prover initialized");
                Self::Transaction(Mutex::new(LocalTransactionProver::default()))
            },
            ProverType::Batch => {
                info!(target: COMPONENT, prover_type = ?prover_type, security_level = MIN_PROOF_SECURITY_LEVEL, "Batch prover initialized");
                Self::Batch(Mutex::new(LocalBatchProver::new(MIN_PROOF_SECURITY_LEVEL)))
            },
            ProverType::Block => {
                info!(target: COMPONENT, prover_type = ?prover_type, security_level = MIN_PROOF_SECURITY_LEVEL, "Block prover initialized");
                Self::Block(Mutex::new(LocalBlockProver::new(MIN_PROOF_SECURITY_LEVEL)))
            },
        }
    }
}

pub struct ProverRpcApi {
    prover: Prover,
}

impl ProverRpcApi {
    pub fn new(prover_type: ProverType) -> Self {
        let prover = Prover::new(prover_type);

        Self { prover }
    }

    #[allow(clippy::result_large_err)]
    #[instrument(
        target = COMPONENT,
        name = "proving_service.prove_tx",
        skip_all,
        ret(level = "debug"),
        fields(id = tracing::field::Empty),
        err
    )]
    pub fn prove_tx(
        &self,
        transaction_witness: TransactionWitness,
    ) -> Result<Response<ProvingResponse>, tonic::Status> {
        let Prover::Transaction(prover) = &self.prover else {
            return Err(Status::unimplemented("Transaction prover is not enabled"));
        };

        let proof = prover
            .try_lock()
            .map_err(|_| Status::resource_exhausted("Server is busy handling another request"))?
            .prove(transaction_witness)
            .map_err(internal_error)?;

        // Record the transaction_id in the current tracing span
        let transaction_id = proof.id();
        tracing::Span::current().record("id", tracing::field::display(&transaction_id));

        Ok(Response::new(ProvingResponse { payload: proof.to_bytes() }))
    }

    #[allow(clippy::result_large_err)]
    #[instrument(
        target = COMPONENT,
        name = "proving_service.prove_batch",
        skip_all,
        ret(level = "debug"),
        fields(id = tracing::field::Empty),
        err
    )]
    pub fn prove_batch(
        &self,
        proposed_batch: ProposedBatch,
    ) -> Result<Response<ProvingResponse>, tonic::Status> {
        let Prover::Batch(prover) = &self.prover else {
            return Err(Status::unimplemented("Batch prover is not enabled"));
        };

        let proven_batch = prover
            .try_lock()
            .map_err(|_| Status::resource_exhausted("Server is busy handling another request"))?
            .prove(proposed_batch)
            .map_err(internal_error)?;

        // Record the batch_id in the current tracing span
        let batch_id = proven_batch.id();
        tracing::Span::current().record("id", tracing::field::display(&batch_id));

        Ok(Response::new(ProvingResponse { payload: proven_batch.to_bytes() }))
    }

    #[allow(clippy::result_large_err)]
    #[instrument(
        target = COMPONENT,
        name = "proving_service.prove_block",
        skip_all,
        ret(level = "debug"),
        fields(id = tracing::field::Empty),
        err
    )]
    pub fn prove_block(
        &self,
        proposed_block: ProposedBlock,
    ) -> Result<Response<ProvingResponse>, tonic::Status> {
        let Prover::Block(prover) = &self.prover else {
            return Err(Status::unimplemented("Block prover is not enabled"));
        };

        let proven_block = prover
            .try_lock()
            .map_err(|_| Status::resource_exhausted("Server is busy handling another request"))?
            .prove(proposed_block)
            .map_err(internal_error)?;

        // Record the commitment of the block in the current tracing span
        let block_id = proven_block.commitment();

        tracing::Span::current().record("id", tracing::field::display(&block_id));

        Ok(Response::new(ProvingResponse { payload: proven_block.to_bytes() }))
    }
}

#[async_trait::async_trait]
impl ProverApi for ProverRpcApi {
    #[instrument(
        target = COMPONENT,
        name = "proving_service.prove",
        skip_all,
        ret(level = "debug"),
        fields(id = tracing::field::Empty),
        err
    )]
    async fn prove(
        &self,
        request: Request<ProvingRequest>,
    ) -> Result<Response<ProvingResponse>, tonic::Status> {
        match request.get_ref().proof_type() {
            ProofType::Transaction => {
                let tx_witness = request.into_inner().try_into().map_err(invalid_argument)?;
                self.prove_tx(tx_witness)
            },
            ProofType::Batch => {
                let proposed_batch = request.into_inner().try_into().map_err(invalid_argument)?;
                self.prove_batch(proposed_batch)
            },
            ProofType::Block => {
                let proposed_block = request.into_inner().try_into().map_err(invalid_argument)?;
                self.prove_block(proposed_block)
            },
        }
    }
}

// UTILITIES
// ================================================================================================

/// Formats an error
fn internal_error<E: core::fmt::Debug>(err: E) -> Status {
    Status::internal(format!("{err:?}"))
}

fn invalid_argument<E: core::fmt::Debug>(err: E) -> Status {
    Status::invalid_argument(format!("{err:?}"))
}
