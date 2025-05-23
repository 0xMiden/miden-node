use std::{any::type_name, num::TryFromIntError};

use miden_node_utils::network_note::NetworkNoteError;
use miden_objects::{
    crypto::merkle::{SmtLeafError, SmtProofError},
    utils::DeserializationError,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("hex error")]
    HexError(#[from] hex::FromHexError),
    #[error("note error")]
    NoteError(#[from] miden_objects::NoteError),
    #[error("network note error")]
    NetworkNoteError(#[from] NetworkNoteError),
    #[error("SMT leaf error")]
    SmtLeafError(#[from] SmtLeafError),
    #[error("SMT proof error")]
    SmtProofError(#[from] SmtProofError),
    #[error("integer conversion error: {0}")]
    TryFromIntError(#[from] TryFromIntError),
    #[error("too much data, expected {expected}, got {got}")]
    TooMuchData { expected: usize, got: usize },
    #[error("not enough data, expected {expected}, got {got}")]
    InsufficientData { expected: usize, got: usize },
    #[error("value is not in the range 0..MODULUS")]
    NotAValidFelt,
    #[error("field `{entity}::{field_name}` is missing")]
    MissingFieldInProtobufRepresentation {
        entity: &'static str,
        field_name: &'static str,
    },
    #[error("MMR error")]
    MmrError(#[from] miden_objects::crypto::merkle::MmrError),
    #[error("failed to deserialize {entity}")]
    DeserializationError {
        entity: &'static str,
        source: DeserializationError,
    },
}

impl ConversionError {
    pub fn deserialization_error(entity: &'static str, source: DeserializationError) -> Self {
        Self::DeserializationError { entity, source }
    }
}

pub trait MissingFieldHelper {
    fn missing_field(field_name: &'static str) -> ConversionError;
}

impl<T: prost::Message> MissingFieldHelper for T {
    fn missing_field(field_name: &'static str) -> ConversionError {
        ConversionError::MissingFieldInProtobufRepresentation {
            entity: type_name::<T>(),
            field_name,
        }
    }
}
