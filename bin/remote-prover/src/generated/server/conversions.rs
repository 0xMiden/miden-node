// CONVERSIONS
// ================================================================================================

use miden_objects::block::{ProposedBlock, ProvenBlock};
use miden_objects::batch::{ProposedBatch, ProvenBatch};
use miden_objects::transaction::{ProvenTransaction, TransactionWitness};
use miden_objects::utils::{Deserializable, DeserializationError, Serializable};

use crate::generated::server as proto;

// PROVEN TRANSACTION TO PROOF
// ================================================================================================

impl From<ProvenTransaction> for proto::Proof {
    fn from(value: ProvenTransaction) -> Self {
        proto::Proof { payload: value.to_bytes() }
    }
}

// Note: Duplicate TryFrom implementations moved to client modules to avoid conflicts

impl TryFrom<proto::ProofRequest> for TransactionWitness {
    type Error = DeserializationError;

    fn try_from(request: proto::ProofRequest) -> Result<Self, Self::Error> {
        TransactionWitness::read_from_bytes(&request.payload)
    }
}

// PROVEN BATCH TO PROOF
// ================================================================================================

impl From<ProvenBatch> for proto::Proof {
    fn from(batch: ProvenBatch) -> Self {
        proto::Proof { payload: batch.to_bytes() }
    }
}

impl TryFrom<proto::ProofRequest> for ProposedBatch {
    type Error = DeserializationError;

    fn try_from(request: proto::ProofRequest) -> Result<Self, Self::Error> {
        ProposedBatch::read_from_bytes(&request.payload)
    }
}

// PROVEN BLOCK TO PROOF
// ================================================================================================

impl From<ProvenBlock> for proto::Proof {
    fn from(value: ProvenBlock) -> Self {
        proto::Proof { payload: value.to_bytes() }
    }
}

impl TryFrom<proto::ProofRequest> for ProposedBlock {
    type Error = DeserializationError;

    fn try_from(request: proto::ProofRequest) -> Result<Self, Self::Error> {
        ProposedBlock::read_from_bytes(&request.payload)
    }
}

// PROOF TYPE CONVERSIONS
// ================================================================================================

impl From<proto::ProofType> for crate::server::api::ProofType {
    fn from(value: proto::ProofType) -> Self {
        match value {
            proto::ProofType::Transaction => Self::Transaction,
            proto::ProofType::Batch => Self::Batch,
            proto::ProofType::Block => Self::Block,
        }
    }
}

impl From<crate::server::api::ProofType> for proto::ProofType {
    fn from(value: crate::server::api::ProofType) -> Self {
        match value {
            crate::server::api::ProofType::Transaction => Self::Transaction,
            crate::server::api::ProofType::Batch => Self::Batch,
            crate::server::api::ProofType::Block => Self::Block,
        }
    }
}

impl From<crate::server::api::ProofType> for i32 {
    fn from(value: crate::server::api::ProofType) -> Self {
        match value {
            crate::server::api::ProofType::Transaction => 0,
            crate::server::api::ProofType::Batch => 1,
            crate::server::api::ProofType::Block => 2,
        }
    }
}
