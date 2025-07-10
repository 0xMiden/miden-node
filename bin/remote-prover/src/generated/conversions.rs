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
    generated::{Proof, ProofRequest, remote_prover::ProofType as ProtoProofType},
};

impl From<ProvenTransaction> for Proof {
    fn from(value: ProvenTransaction) -> Self {
        Proof { payload: value.to_bytes() }
    }
}

impl TryFrom<Proof> for ProvenTransaction {
    type Error = DeserializationError;

    fn try_from(response: Proof) -> Result<Self, Self::Error> {
        ProvenTransaction::read_from_bytes(&response.payload)
    }
}

impl TryFrom<ProofRequest> for TransactionWitness {
    type Error = DeserializationError;

    fn try_from(request: ProofRequest) -> Result<Self, Self::Error> {
        TransactionWitness::read_from_bytes(&request.payload)
    }
}

impl TryFrom<ProofRequest> for ProposedBatch {
    type Error = DeserializationError;

    fn try_from(request: ProofRequest) -> Result<Self, Self::Error> {
        ProposedBatch::read_from_bytes(&request.payload)
    }
}

impl TryFrom<ProofRequest> for ProposedBlock {
    type Error = DeserializationError;

    fn try_from(request: ProofRequest) -> Result<Self, Self::Error> {
        ProposedBlock::read_from_bytes(&request.payload)
    }
}

impl From<ProofType> for ProtoProofType {
    fn from(value: ProofType) -> Self {
        match value {
            ProofType::Transaction => ProtoProofType::Transaction,
            ProofType::Batch => ProtoProofType::Batch,
            ProofType::Block => ProtoProofType::Block,
        }
    }
}

impl From<ProtoProofType> for ProofType {
    fn from(value: ProtoProofType) -> Self {
        match value {
            ProtoProofType::Transaction => ProofType::Transaction,
            ProtoProofType::Batch => ProofType::Batch,
            ProtoProofType::Block => ProofType::Block,
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
