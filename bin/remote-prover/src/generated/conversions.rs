// CONVERSIONS
// ================================================================================================

use miden_objects::{
    batch::ProposedBatch,
    block::ProposedBlock,
    transaction::{ProvenTransaction, TransactionWitness},
};
use miden_tx::utils::{Deserializable, DeserializationError, Serializable};

use crate::{
    api::ProofType,
    generated::{self as proto, ProvingRequest, ProvingResponse},
};

impl From<ProvenTransaction> for ProvingResponse {
    fn from(value: ProvenTransaction) -> Self {
        ProvingResponse { payload: value.to_bytes() }
    }
}

impl TryFrom<ProvingResponse> for ProvenTransaction {
    type Error = DeserializationError;

    fn try_from(response: ProvingResponse) -> Result<Self, Self::Error> {
        ProvenTransaction::read_from_bytes(&response.payload)
    }
}

impl TryFrom<ProvingRequest> for TransactionWitness {
    type Error = DeserializationError;

    fn try_from(request: ProvingRequest) -> Result<Self, Self::Error> {
        TransactionWitness::read_from_bytes(&request.payload)
    }
}

impl TryFrom<ProvingRequest> for ProposedBatch {
    type Error = DeserializationError;

    fn try_from(request: ProvingRequest) -> Result<Self, Self::Error> {
        ProposedBatch::read_from_bytes(&request.payload)
    }
}

impl TryFrom<ProvingRequest> for ProposedBlock {
    type Error = DeserializationError;

    fn try_from(request: ProvingRequest) -> Result<Self, Self::Error> {
        ProposedBlock::read_from_bytes(&request.payload)
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
