//! A collection of new types and safety wrappers used throughout the faucet.

use std::fmt::Debug;

use miden_objects::asset::FungibleAsset;
use serde::{Deserialize, Deserializer, Serialize, de};

/// Describes the asset amounts allowed by the faucet.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct AssetOptions(Vec<AssetAmount>);

impl std::fmt::Display for AssetOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;

        let mut options = self.0.iter();
        if let Some(first) = options.next() {
            write!(f, " {first}")?;
        }
        for rest in options {
            write!(f, ", {rest}")?;
        }

        write!(f, " ]")
    }
}

impl AssetOptions {
    /// Converts the given amount into an [`AssetAmount`] _iff_ if it
    /// matches one of the allowed options.
    ///
    /// This is the only valid way to create an [`AssetAmount`].
    pub fn validate(&self, amount: u64) -> Option<AssetAmount> {
        // SAFETY: Invalid amounts will be discarded because our
        //         options only contain value amounts.
        let amount = AssetAmount(amount);
        self.0.contains(&amount).then_some(amount)
    }

    /// Creates [`AssetOptions`] if all options are valid [`AssetAmount`]'s
    ///
    /// The error value contains the invalid option.
    pub fn new(options: Vec<u64>) -> Result<Self, u64> {
        if let Some(invalid) = options.iter().find(|x| **x > AssetAmount::MAX) {
            return Err(*invalid);
        }

        Ok(Self(options.into_iter().map(AssetAmount).collect()))
    }
}

/// Represents a valid asset amount for a [`FungibleAsset`].
///
/// Can only be created via [`AssetOptions`].
///
/// A [`FungibleAsset`] has a maximum representable amount
/// and this type guarantees that its value is within this range.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize)]
pub struct AssetAmount(u64);

impl std::fmt::Display for AssetAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AssetAmount {
    /// The absolute maximum asset amount allowed by the network.
    ///
    /// An [`AssetAmount`] is further restricted to the values allowed by
    /// [`AssetOptions`].
    pub const MAX: u64 = FungibleAsset::MAX_AMOUNT;

    pub fn inner(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for AssetOptions {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let options = Vec::<u64>::deserialize(de)?;

        AssetOptions::new(options).map_err(|invalid_option| {
            de::Error::invalid_value(
                de::Unexpected::Unsigned(invalid_option),
                &format!(
                    "Maximum fungible asset value allowed by the network is {}",
                    AssetAmount::MAX
                )
                .as_str(),
            )
        })
    }
}

/// Type of note to generate for a mint request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NoteType {
    Private,
    Public,
}

impl From<NoteType> for miden_objects::note::NoteType {
    fn from(value: NoteType) -> Self {
        match value {
            NoteType::Private => Self::Private,
            NoteType::Public => Self::Public,
        }
    }
}

impl std::fmt::Display for NoteType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Private => f.write_str("private"),
            Self::Public => f.write_str("public"),
        }
    }
}
