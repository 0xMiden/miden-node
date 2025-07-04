use miden_objects::{crypto::hash::rpo::RpoDigest, transaction::TransactionId};

use crate::{
    errors::ConversionError,
    generated::{blockchain as proto_blockchain, primitives as proto_primitives},
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

impl From<&TransactionId> for proto_blockchain::TransactionId {
    fn from(value: &TransactionId) -> Self {
        proto_blockchain::TransactionId { id: Some(value.into()) }
    }
}

impl From<TransactionId> for proto_blockchain::TransactionId {
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

impl TryFrom<proto_blockchain::TransactionId> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto_blockchain::TransactionId) -> Result<Self, Self::Error> {
        value
            .id
            .ok_or(ConversionError::MissingFieldInProtobufRepresentation {
                entity: "TransactionId",
                field_name: "id",
            })?
            .try_into()
    }
}
