use miden_objects::{crypto::hash::rpo::RpoDigest, transaction::TransactionId};

use crate::{
    errors::ConversionError,
    generated::{primitives as proto_primitives, transaction as proto_transaction},
};

// FROM TRANSACTION ID
// ================================================================================================

impl From<&TransactionId> for proto_primitives::Digest {
    fn from(value: &TransactionId) -> Self {
        (*value).inner().into()
    }
}

impl From<TransactionId> for proto_primitives::Digest {
    fn from(value: TransactionId) -> Self {
        value.inner().into()
    }
}

impl From<&TransactionId> for proto_transaction::TransactionId {
    fn from(value: &TransactionId) -> Self {
        proto_transaction::TransactionId { id: Some(value.into()) }
    }
}

impl From<TransactionId> for proto_transaction::TransactionId {
    fn from(value: TransactionId) -> Self {
        (&value).into()
    }
}

// INTO TRANSACTION ID
// ================================================================================================

impl TryFrom<proto_primitives::Digest> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto_primitives::Digest) -> Result<Self, Self::Error> {
        let digest: RpoDigest = value.try_into()?;
        Ok(digest.into())
    }
}

impl TryFrom<proto_transaction::TransactionId> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto_transaction::TransactionId) -> Result<Self, Self::Error> {
        value
            .id
            .ok_or(ConversionError::MissingFieldInProtobufRepresentation {
                entity: "TransactionId",
                field_name: "id",
            })?
            .try_into()
    }
}
