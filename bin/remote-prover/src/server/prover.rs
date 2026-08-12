use miden_block_prover::LocalBlockProver;
use miden_node_proto::BlockProofRequest;
use miden_node_proto::domain::batch::decode_proposed_batch;
use miden_node_proto::generated::remote_prover as proto;
use miden_node_utils::ErrorReport;
use miden_protocol::MIN_PROOF_SECURITY_LEVEL;
use miden_protocol::transaction::TransactionInputs;
use miden_protocol::utils::serde::Deserializable;
use miden_tx::LocalTransactionProver;
use miden_tx_batch::{BatchExecutor, LocalBatchProver};

use crate::server::proof_kind::ProofKind;

/// An enum representing the different types of provers available.
pub enum Prover {
    Transaction(LocalTransactionProver),
    Batch(LocalBatchProver),
    Block(LocalBlockProver),
}

impl Prover {
    /// Constructs a [`Prover`] of the specified [`ProofKind`].
    pub fn new(proof_type: ProofKind) -> Self {
        match proof_type {
            ProofKind::Transaction => Self::Transaction(LocalTransactionProver::default()),
            ProofKind::Batch => Self::Batch(LocalBatchProver::new()),
            ProofKind::Block => Self::Block(LocalBlockProver::new(MIN_PROOF_SECURITY_LEVEL)),
        }
    }

    /// Proves a [`proto::ProofRequest`] using the appropriate prover implementation as specified
    /// during construction.
    pub fn prove(&self, request: proto::ProofRequest) -> Result<proto::Proof, tonic::Status> {
        use proto::proof::Result as ProofResult;
        use proto::proof_request::Request;

        let result = match (self, request.request) {
            (Self::Transaction(prover), Some(Request::TransactionInputs(bytes))) => {
                let inputs = TransactionInputs::read_from_bytes(&bytes).map_err(|err| {
                    tonic::Status::invalid_argument(
                        err.as_report_context("failed to decode transaction inputs"),
                    )
                })?;
                let transaction = prover.prove(inputs).map_err(|err| {
                    tonic::Status::internal(err.as_report_context("failed to prove transaction"))
                })?;
                ProofResult::ProvenTransaction(transaction.into())
            },
            (Self::Batch(prover), Some(Request::ProposedBatch(batch))) => {
                let batch = decode_proposed_batch(batch, MIN_PROOF_SECURITY_LEVEL)
                    .map_err(tonic::Status::from)?;
                let executed_batch = BatchExecutor::new().execute(batch).map_err(|err| {
                    tonic::Status::internal(err.as_report_context("failed to execute batch"))
                })?;
                let batch = prover.prove(executed_batch).map_err(|err| {
                    tonic::Status::internal(err.as_report_context("failed to prove batch"))
                })?;
                ProofResult::ProvenBatch(batch.into())
            },
            (Self::Block(prover), Some(Request::BlockProofRequest(bytes))) => {
                let request = BlockProofRequest::read_from_bytes(&bytes).map_err(|err| {
                    tonic::Status::invalid_argument(
                        err.as_report_context("failed to decode block proof request"),
                    )
                })?;
                let BlockProofRequest { tx_batches, block_header, block_inputs } = request;
                let proof =
                    prover.prove(tx_batches, &block_header, block_inputs).map_err(|err| {
                        tonic::Status::internal(err.as_report_context("failed to prove block"))
                    })?;
                ProofResult::BlockProof(proof.into())
            },
            (_, None) => return Err(tonic::Status::invalid_argument("missing proof request")),
            _ => {
                return Err(tonic::Status::invalid_argument(
                    "request kind does not match the configured prover",
                ));
            },
        };

        Ok(proto::Proof { result: Some(result) })
    }

    /// Returns the context attached to failures of the blocking task running this prover.
    pub const fn task_panic_context(&self) -> &'static str {
        match self {
            Prover::Transaction(_) => "transaction prover task panicked",
            Prover::Batch(_) => "batch prover task panicked",
            Prover::Block(_) => "block prover task panicked",
        }
    }
}
