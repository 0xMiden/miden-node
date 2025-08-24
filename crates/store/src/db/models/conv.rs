//! Central place to define conversion from and to database primitive types
//!
//! Eventually, all of them should have types and we can implement a trait for them
//! rather than function pairs.
//!
//! Notice: All of them are infallible. The invariant is a sane content of the database
//! and humans ensure the sanity of casts.
//!
//! Notice: Keep in mind if you _need_ to expand the datatype, only if you require sorting this is
//! mandatory!
//!
//! Notice: Ensure you understand what casting does at the bit-level before changing any.
//!
//! Notice: Changing any of these are _backwards-incompatible_ changes that are not caught/covered
//! by migrations!

#![allow(
    clippy::inline_always,
    reason = "Just unification helpers of 1-2 lines of casting types"
)]
#![allow(
    dead_code,
    reason = "Not all converters are used bidirectionally, however, keeping them is a good thing"
)]
#![allow(
    clippy::cast_sign_loss,
    reason = "This is the one file where we map the signed database types to the working types"
)]
#![allow(
    clippy::cast_possible_wrap,
    reason = "We will not approach the item count where i64 and usize casting will cause issues
    on relevant platforms"
)]

use std::any::type_name;

use miden_node_proto::domain::account::{NetworkAccountError, NetworkAccountPrefix};
use miden_objects::Felt;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteExecutionMode, NoteTag};

#[derive(Debug, thiserror::Error)]
#[error("failed to convert a database value to it's in memory type {0}")]
pub struct DatabaseTypeConversionError(&'static str);

/// Convert from and to it's database representation and back
///
/// We do not assume sanity of DB types.
pub(crate) trait SqlTypeConvert: Sized {
    type Raw: Sized;
    type Error: std::error::Error + Send + Sync + 'static;
    fn to_raw_sql(self) -> Self::Raw;
    fn from_raw_sql(_raw: Self::Raw) -> Result<Self, Self::Error>;
}

impl SqlTypeConvert for BlockNumber {
    type Raw = i64;
    type Error = DatabaseTypeConversionError;
    fn from_raw_sql(raw: Self::Raw) -> Result<Self, Self::Error> {
        u32::try_from(raw)
            .map(BlockNumber::from)
            .map_err(|_| DatabaseTypeConversionError(type_name::<BlockNumber>()))
    }
    fn to_raw_sql(self) -> Self::Raw {
        i64::from(self.as_u32())
    }
}

impl SqlTypeConvert for NetworkAccountPrefix {
    type Raw = i64;
    type Error = DatabaseTypeConversionError;
    fn from_raw_sql(raw: Self::Raw) -> Result<Self, Self::Error> {
        NetworkAccountPrefix::try_from(raw as u32)
            .map_err(|_e| DatabaseTypeConversionError(type_name::<NetworkAccountError>()))
    }
    fn to_raw_sql(self) -> Self::Raw {
        i64::from(self.inner())
    }
}

impl SqlTypeConvert for NoteExecutionMode {
    type Raw = i32;
    type Error = DatabaseTypeConversionError;

    #[inline(always)]
    fn from_raw_sql(raw: Self::Raw) -> Result<Self, Self::Error> {
        Ok(match raw {
            0 => Self::Network,
            1 => Self::Local,
            _ => return Err(DatabaseTypeConversionError(type_name::<NoteExecutionMode>())),
        })
    }

    #[inline(always)]
    fn to_raw_sql(self) -> Self::Raw {
        match self {
            NoteExecutionMode::Network => 0,
            NoteExecutionMode::Local => 1,
        }
    }
}

impl SqlTypeConvert for NoteTag {
    type Raw = i32;
    type Error = DatabaseTypeConversionError;

    #[inline(always)]
    fn from_raw_sql(raw: Self::Raw) -> Result<Self, Self::Error> {
        #[allow(clippy::cast_sign_loss)]
        Ok(NoteTag::from(raw as u32))
    }

    #[inline(always)]
    fn to_raw_sql(self) -> Self::Raw {
        u32::from(self) as i32
    }
}

// Raw type conversions - eventually introduce wrapper types
// ===========================================================

#[inline(always)]
pub(crate) fn raw_sql_to_nullifier_prefix(raw: i32) -> u16 {
    debug_assert!(raw >= 0);
    raw as u16
}
#[inline(always)]
pub(crate) fn nullifier_prefix_to_raw_sql(prefix: u16) -> i32 {
    i32::from(prefix)
}

#[inline(always)]
pub(crate) fn raw_sql_to_nonce(raw: i64) -> u64 {
    debug_assert!(raw >= 0);
    raw as u64
}
#[inline(always)]
pub(crate) fn nonce_to_raw_sql(nonce: Felt) -> i64 {
    nonce.as_int() as i64
}

#[inline(always)]
pub(crate) fn raw_sql_to_slot(raw: i32) -> u8 {
    debug_assert!(raw >= 0);
    raw as u8
}
#[inline(always)]
pub(crate) fn slot_to_raw_sql(slot: u8) -> i32 {
    i32::from(slot)
}

#[inline(always)]
pub(crate) fn raw_sql_to_fungible_delta(raw: i64) -> i64 {
    raw
}
#[inline(always)]
pub(crate) fn fungible_delta_to_raw_sql(delta: i64) -> i64 {
    delta
}

#[inline(always)]
#[allow(clippy::cast_sign_loss)]
pub(crate) fn raw_sql_to_note_type(raw: i32) -> u8 {
    raw as u8
}
#[inline(always)]
pub(crate) fn note_type_to_raw_sql(note_type: u8) -> i32 {
    i32::from(note_type)
}

#[inline(always)]
pub(crate) fn raw_sql_to_execution_mode(raw: i32) -> i32 {
    // match raw {
    //     0 => NoteExecutionMode::Local,
    //     1 => NoteExecutionMode::Network,
    //    _ => unreachable!() // TODO
    // }
    raw
}
#[inline(always)]
pub(crate) fn execution_mode_to_raw_sql(mode: i32) -> i32 {
    mode
}

#[inline(always)]
pub(crate) fn raw_sql_to_execution_hint(raw: i64) -> u64 {
    raw as u64
}
#[inline(always)]
pub(crate) fn execution_hint_to_raw_sql(hint: u64) -> i64 {
    hint as i64
}

#[inline(always)]
pub(crate) fn raw_sql_to_aux(raw: i64) -> Felt {
    Felt::try_from(raw as u64).unwrap()
}
#[inline(always)]
pub(crate) fn aux_to_raw_sql(hint: Felt) -> i64 {
    hint.inner() as i64
}

#[inline(always)]
pub(crate) fn raw_sql_to_idx(raw: i32) -> usize {
    raw as usize
}
#[inline(always)]
pub(crate) fn idx_to_raw_sql(idx: usize) -> i32 {
    idx as i32
}
