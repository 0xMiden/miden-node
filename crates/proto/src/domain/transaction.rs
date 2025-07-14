use miden_objects::{Word, transaction::TransactionId};

use crate::{errors::ConversionError, generated as proto};

// FROM TRANSACTION ID
// ================================================================================================

impl From<&TransactionId> for proto::word::Word {
    fn from(value: &TransactionId) -> Self {
        value.as_word().into()
    }
}

impl From<TransactionId> for proto::word::Word {
    fn from(value: TransactionId) -> Self {
        value.as_word().into()
    }
}

impl From<&TransactionId> for proto::transaction::TransactionId {
    fn from(value: &TransactionId) -> Self {
        proto::transaction::TransactionId { id: Some(value.into()) }
    }
}

impl From<TransactionId> for proto::transaction::TransactionId {
    fn from(value: TransactionId) -> Self {
        (&value).into()
    }
}

// INTO TRANSACTION ID
// ================================================================================================

impl TryFrom<proto::word::Word> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto::word::Word) -> Result<Self, Self::Error> {
        let word: Word = value.try_into()?;
        Ok(word.into())
    }
}

impl TryFrom<proto::transaction::TransactionId> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto::transaction::TransactionId) -> Result<Self, Self::Error> {
        value
            .id
            .ok_or(ConversionError::MissingFieldInProtobufRepresentation {
                entity: "TransactionId",
                field_name: "id",
            })?
            .try_into()
    }
}
