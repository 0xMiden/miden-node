use std::fmt::{Display, Formatter};

use hex::{FromHex, ToHex};
use miden_objects::{Felt, StarkField, Word, note::NoteId};

use crate::{errors::ConversionError, generated::word as proto};

// CONSTANTS
// ================================================================================================

pub const DIGEST_DATA_SIZE: usize = 32;

// FORMATTING
// ================================================================================================

impl Display for proto::Word {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl ToHex for &proto::Word {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (*self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (*self).encode_hex_upper()
    }
}

impl ToHex for proto::Word {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        let mut data: Vec<char> = Vec::with_capacity(DIGEST_DATA_SIZE);
        data.extend(format!("{:016x}", self.w0).chars());
        data.extend(format!("{:016x}", self.w1).chars());
        data.extend(format!("{:016x}", self.w2).chars());
        data.extend(format!("{:016x}", self.w3).chars());
        data.into_iter().collect()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        let mut data: Vec<char> = Vec::with_capacity(DIGEST_DATA_SIZE);
        data.extend(format!("{:016X}", self.w0).chars());
        data.extend(format!("{:016X}", self.w1).chars());
        data.extend(format!("{:016X}", self.w2).chars());
        data.extend(format!("{:016X}", self.w3).chars());
        data.into_iter().collect()
    }
}

impl FromHex for proto::Word {
    type Error = ConversionError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let data = hex::decode(hex)?;

        match data.len() {
            size if size < DIGEST_DATA_SIZE => {
                Err(ConversionError::InsufficientData { expected: DIGEST_DATA_SIZE, got: size })
            },
            size if size > DIGEST_DATA_SIZE => {
                Err(ConversionError::TooMuchData { expected: DIGEST_DATA_SIZE, got: size })
            },
            _ => {
                let w0 = u64::from_be_bytes(data[..8].try_into().unwrap());
                let w1 = u64::from_be_bytes(data[8..16].try_into().unwrap());
                let w2 = u64::from_be_bytes(data[16..24].try_into().unwrap());
                let w3 = u64::from_be_bytes(data[24..32].try_into().unwrap());

                Ok(proto::Word { w0, w1, w2, w3 })
            },
        }
    }
}

// INTO
// ================================================================================================

impl From<[u64; 4]> for proto::Word {
    fn from(value: [u64; 4]) -> Self {
        Self {
            w0: value[0],
            w1: value[1],
            w2: value[2],
            w3: value[3],
        }
    }
}

impl From<&[u64; 4]> for proto::Word {
    fn from(value: &[u64; 4]) -> Self {
        (*value).into()
    }
}

impl From<[Felt; 4]> for proto::Word {
    fn from(value: [Felt; 4]) -> Self {
        Self {
            w0: value[0].as_int(),
            w1: value[1].as_int(),
            w2: value[2].as_int(),
            w3: value[3].as_int(),
        }
    }
}

impl From<&[Felt; 4]> for proto::Word {
    fn from(value: &[Felt; 4]) -> Self {
        (*value).into()
    }
}

impl From<Word> for proto::Word {
    fn from(value: Word) -> Self {
        Self {
            w0: value[0].as_int(),
            w1: value[1].as_int(),
            w2: value[2].as_int(),
            w3: value[3].as_int(),
        }
    }
}

impl From<&Word> for proto::Word {
    fn from(value: &Word) -> Self {
        (*value).into()
    }
}

impl From<&NoteId> for proto::Word {
    fn from(value: &NoteId) -> Self {
        value.as_word().into()
    }
}

impl From<NoteId> for proto::Word {
    fn from(value: NoteId) -> Self {
        value.as_word().into()
    }
}

// FROM DIGEST
// ================================================================================================

impl From<proto::Word> for [u64; 4] {
    fn from(value: proto::Word) -> Self {
        [value.w0, value.w1, value.w2, value.w3]
    }
}

impl TryFrom<proto::Word> for [Felt; 4] {
    type Error = ConversionError;

    fn try_from(value: proto::Word) -> Result<Self, Self::Error> {
        if [value.w0, value.w1, value.w2, value.w3]
            .iter()
            .any(|v| *v >= <Felt as StarkField>::MODULUS)
        {
            return Err(ConversionError::NotAValidFelt);
        }

        Ok([
            Felt::new(value.w0),
            Felt::new(value.w1),
            Felt::new(value.w2),
            Felt::new(value.w3),
        ])
    }
}

impl TryFrom<proto::Word> for Word {
    type Error = ConversionError;

    fn try_from(value: proto::Word) -> Result<Self, Self::Error> {
        Ok(Self::new(value.try_into()?))
    }
}

impl TryFrom<&proto::Word> for [Felt; 4] {
    type Error = ConversionError;

    fn try_from(value: &proto::Word) -> Result<Self, Self::Error> {
        (*value).try_into()
    }
}

impl TryFrom<&proto::Word> for Word {
    type Error = ConversionError;

    fn try_from(value: &proto::Word) -> Result<Self, Self::Error> {
        (*value).try_into()
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod test {
    use hex::{FromHex, ToHex};
    use proptest::prelude::*;

    use crate::generated::word as proto;

    #[test]
    fn hex_word() {
        let word = proto::Word {
            w0: 0x306A_B7A6_F795_CAD7,
            w1: 0x4927_3716_D099_AA04,
            w2: 0xF741_2C3D_E726_4450,
            w3: 0x976B_8764_9DB3_B82F,
        };
        let encoded: String = ToHex::encode_hex(&word);
        let round_trip: Result<proto::Word, _> = FromHex::from_hex::<&[u8]>(encoded.as_ref());
        assert_eq!(word, round_trip.unwrap());

        let word = proto::Word { w0: 0, w1: 0, w2: 0, w3: 0 };
        let encoded: String = ToHex::encode_hex(&word);
        let round_trip: Result<proto::Word, _> = FromHex::from_hex::<&[u8]>(encoded.as_ref());
        assert_eq!(word, round_trip.unwrap());
    }

    proptest! {
        #[test]
        fn encode_decode(
            w0: u64,
            w1: u64,
            w2: u64,
            w3: u64,
        ) {
            let word = proto::Word { w0, w1, w2, w3 };
            let encoded: String = ToHex::encode_hex(&word);
            let round_trip: Result<proto::Word, _> = FromHex::from_hex::<&[u8]>(encoded.as_ref());
            assert_eq!(word, round_trip.unwrap());
        }
    }
}
