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

use std::any::type_name;

use miden_node_proto::domain::account::{NetworkAccountError, NetworkAccountPrefix};
use miden_objects::{Felt, block::BlockNumber, note::NoteTag};

#[derive(Debug, thiserror::Error)]
#[error("failed to convert a database value to it's in memory type {0}")]
pub struct DbReprConvError(&'static str);

/// Convert from and to it's database representation and back
///
/// We do not assume sanity of DB types.
pub(crate) trait DbReprConv: Sized {
    type Raw: Sized;
    type Error: std::error::Error + Send + Sync + 'static;
    fn to_raw_sql(self) -> Self::Raw;
    fn from_raw_sql(_raw: Self::Raw) -> ::std::result::Result<Self, Self::Error>;
}

impl DbReprConv for BlockNumber {
    type Raw = i64;
    type Error = DbReprConvError;
    fn from_raw_sql(raw: Self::Raw) -> ::std::result::Result<Self, Self::Error> {
        #[allow(clippy::cast_sign_loss)]
        if raw <= u32::MAX as i64 {
            Ok(BlockNumber::from(raw as u32))
        } else {
            Err(DbReprConvError(type_name::<BlockNumber>()))
        }
    }
    fn to_raw_sql(self) -> Self::Raw {
        self.as_u32() as i64
    }
}

impl DbReprConv for NetworkAccountPrefix {
    type Raw = i64;
    type Error = DbReprConvError;
    fn from_raw_sql(raw: Self::Raw) -> ::std::result::Result<Self, Self::Error> {
        NetworkAccountPrefix::try_from(raw as u32)
            .map_err(|_e| DbReprConvError(type_name::<NetworkAccountError>()))
    }
    fn to_raw_sql(self) -> Self::Raw {
        self.inner() as i64
    }
}

impl DbReprConv for NoteTag {
    type Raw = i32;
    type Error = DbReprConvError;

    #[inline(always)]
    fn from_raw_sql(raw: Self::Raw) -> ::std::result::Result<Self, Self::Error> {
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
pub(crate) fn raw_sql_to_consumed(raw: i32) -> bool {
    debug_assert!(raw == 1 || raw == 0);
    raw == 1
}

#[inline(always)]
pub(crate) fn consumed_to_raw_sql(consumed: bool) -> i32 {
    consumed as u8 as i32
}

#[inline(always)]
pub(crate) fn raw_sql_to_nullifier_prefix(raw: i32) -> u16 {
    debug_assert!(raw >= 0);
    raw as u16
}
#[inline(always)]
pub(crate) fn nullifier_prefix_to_raw_sql(prefix: u16) -> i32 {
    prefix as i32
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
    slot as i32
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
pub(crate) fn note_type_to_raw_sql(note_type_: u8) -> i32 {
    note_type_ as i32
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
