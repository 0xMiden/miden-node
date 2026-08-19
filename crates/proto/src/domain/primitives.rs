use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Felt, Word};

use crate::errors::ConversionError;
use crate::generated as proto;

// CONSTANTS
// ================================================================================================

const FELT_SERIALIZED_SIZE: usize = size_of::<u64>();
const WORD_SERIALIZED_SIZE: usize = Word::SERIALIZED_SIZE;

// HELPERS
// ================================================================================================

fn ensure_exact_length(
    encoded: &[u8],
    expected: usize,
    field: &'static str,
) -> Result<(), ConversionError> {
    if encoded.len() != expected {
        return Err(ConversionError::message(format!(
            "expected exactly {expected} bytes, got {}",
            encoded.len()
        ))
        .context(field));
    }

    Ok(())
}

// FELT
// ================================================================================================

impl From<Felt> for proto::primitives::Felt {
    fn from(value: Felt) -> Self {
        Self { encoded: value.to_bytes() }
    }
}

impl From<&Felt> for proto::primitives::Felt {
    fn from(value: &Felt) -> Self {
        Self { encoded: value.to_bytes() }
    }
}

impl TryFrom<proto::primitives::Felt> for Felt {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::Felt) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl TryFrom<&proto::primitives::Felt> for Felt {
    type Error = ConversionError;

    fn try_from(value: &proto::primitives::Felt) -> Result<Self, Self::Error> {
        ensure_exact_length(&value.encoded, FELT_SERIALIZED_SIZE, "felt.encoded")?;

        Self::read_from_bytes(&value.encoded)
            .map_err(|err| ConversionError::deserialization("felt.encoded", err))
    }
}

// WORD
// ================================================================================================

impl From<Word> for proto::primitives::Word {
    fn from(value: Word) -> Self {
        Self { encoded: value.to_bytes() }
    }
}

impl From<&Word> for proto::primitives::Word {
    fn from(value: &Word) -> Self {
        Self { encoded: value.to_bytes() }
    }
}

impl TryFrom<proto::primitives::Word> for Word {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::Word) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl TryFrom<&proto::primitives::Word> for Word {
    type Error = ConversionError;

    fn try_from(value: &proto::primitives::Word) -> Result<Self, Self::Error> {
        ensure_exact_length(&value.encoded, WORD_SERIALIZED_SIZE, "word.encoded")?;

        Self::read_from_bytes(&value.encoded)
            .map_err(|err| ConversionError::deserialization("word.encoded", err))
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn felt_roundtrip() {
        for felt in [Felt::ZERO, Felt::new_unchecked(42), Felt::new_unchecked(Felt::ORDER - 1)] {
            let encoded = proto::primitives::Felt::from(felt);

            assert_eq!(encoded.encoded.len(), FELT_SERIALIZED_SIZE);
            assert_eq!(Felt::try_from(encoded.clone()).unwrap(), felt);
            assert_eq!(Felt::try_from(&encoded).unwrap(), felt);
            assert_eq!(proto::primitives::Felt::from(&felt), encoded);
        }
    }

    #[test]
    fn felt_rejects_invalid_lengths() {
        for length in [0, 7, 9, 1024] {
            let value = proto::primitives::Felt { encoded: vec![0; length] };
            let err = Felt::try_from(value).unwrap_err();

            assert_eq!(
                err.to_string(),
                format!("felt.encoded: expected exactly 8 bytes, got {length}")
            );
        }
    }

    #[test]
    fn felt_rejects_non_canonical_value() {
        let value = proto::primitives::Felt {
            encoded: Felt::ORDER.to_le_bytes().to_vec(),
        };
        let err = Felt::try_from(value).unwrap_err();

        assert!(err.to_string().starts_with("failed to deserialize felt.encoded:"));
    }

    #[test]
    fn word_roundtrip() {
        let words = [
            Word::default(),
            Word::new([
                Felt::new_unchecked(1),
                Felt::new_unchecked(2),
                Felt::new_unchecked(3),
                Felt::new_unchecked(4),
            ]),
        ];

        for word in words {
            let encoded = proto::primitives::Word::from(word);

            assert_eq!(encoded.encoded.len(), WORD_SERIALIZED_SIZE);
            assert_eq!(Word::try_from(encoded.clone()).unwrap(), word);
            assert_eq!(Word::try_from(&encoded).unwrap(), word);
            assert_eq!(proto::primitives::Word::from(&word), encoded);
        }
    }

    #[test]
    fn word_rejects_invalid_lengths() {
        for length in [0, 31, 33, 1024] {
            let value = proto::primitives::Word { encoded: vec![0; length] };
            let err = Word::try_from(value).unwrap_err();

            assert_eq!(
                err.to_string(),
                format!("word.encoded: expected exactly 32 bytes, got {length}")
            );
        }
    }

    #[test]
    fn word_rejects_non_canonical_element() {
        for element_index in 0..Word::NUM_ELEMENTS {
            let mut encoded = vec![0; WORD_SERIALIZED_SIZE];
            let offset = element_index * FELT_SERIALIZED_SIZE;
            encoded[offset..offset + FELT_SERIALIZED_SIZE]
                .copy_from_slice(&Felt::ORDER.to_le_bytes());

            let value = proto::primitives::Word { encoded };
            let err = Word::try_from(value).unwrap_err();

            assert!(err.to_string().starts_with("failed to deserialize word.encoded:"));
        }
    }
}
