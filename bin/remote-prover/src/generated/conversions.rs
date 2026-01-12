// CONVERSIONS
// ================================================================================================

use miden_node_proto::BlockProofRequest;
use miden_protocol::batch::ProposedBatch;
use miden_protocol::transaction::{ProvenTransaction, TransactionInputs};
use miden_tx::utils::{Deserializable, DeserializationError, Serializable};

use crate::api::ProofType;
use crate::generated as proto;

impl From<ProvenTransaction> for proto::Proof {
    fn from(value: ProvenTransaction) -> Self {
        proto::Proof { payload: value.to_bytes() }
    }
}

impl TryFrom<proto::Proof> for ProvenTransaction {
    type Error = DeserializationError;

    fn try_from(response: proto::Proof) -> Result<Self, Self::Error> {
        ProvenTransaction::read_from_bytes(&response.payload)
    }
}

impl TryFrom<proto::ProofRequest> for TransactionInputs {
    type Error = DeserializationError;

    fn try_from(request: proto::ProofRequest) -> Result<Self, Self::Error> {
        TransactionInputs::read_from_bytes(&request.payload)
    }
}

impl TryFrom<proto::ProofRequest> for ProposedBatch {
    type Error = DeserializationError;

    fn try_from(request: proto::ProofRequest) -> Result<Self, Self::Error> {
        ProposedBatch::read_from_bytes(&request.payload)
    }
}

impl TryFrom<proto::ProofRequest> for BlockProofRequest {
    type Error = DeserializationError;

    fn try_from(request: proto::ProofRequest) -> Result<Self, Self::Error> {
        BlockProofRequest::read_from_bytes(&request.payload)
    }
}

impl From<ProofType> for proto::ProofType {
    fn from(value: ProofType) -> Self {
        match value {
            ProofType::Transaction => proto::ProofType::Transaction,
            ProofType::Batch => proto::ProofType::Batch,
            ProofType::Block => proto::ProofType::Block,
        }
    }
}

impl From<proto::ProofType> for ProofType {
    fn from(value: proto::ProofType) -> Self {
        match value {
            proto::ProofType::Transaction => ProofType::Transaction,
            proto::ProofType::Batch => ProofType::Batch,
            proto::ProofType::Block => ProofType::Block,
        }
    }
}

impl TryFrom<i32> for ProofType {
    type Error = String;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ProofType::Transaction),
            1 => Ok(ProofType::Batch),
            2 => Ok(ProofType::Block),
            _ => Err(format!("unknown ProverType value: {value}")),
        }
    }
}

impl From<ProofType> for i32 {
    fn from(value: ProofType) -> Self {
        match value {
            ProofType::Transaction => 0,
            ProofType::Batch => 1,
            ProofType::Block => 2,
        }
    }
}
