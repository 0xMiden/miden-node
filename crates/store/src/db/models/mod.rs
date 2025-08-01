//! Defines models for usage with the diesel API
//!
//! Note: `select` can either be used as
//! `SelectDsl::select(schema::foo::table, (schema::foo::some_cool_id, ))`
//! or
//! `SelectDsl::select(schema::foo::table, FooRawRow::as_selectable())`.
//!
//! The former can be used to avoid declaring extra types, while the latter
//! is better if a full row is in need of loading and avoids duplicate
//! specification.
//!
//! Note: The fully qualified syntax yields for _much_ better errors.
//! The first step in debugging should always be using the fully qualified
//! calling syntext when dealing with diesel.

// TODO provide helper functions to limit the scope of these
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_lossless)]

use std::num::NonZeroUsize;

use diesel::{prelude::*, sqlite::Sqlite};

use crate::{
    db::{
        NoteRecord, NoteSyncRecord, NullifierInfo,
        schema::{
            // the list of tables
            // referenced in `#[diesel(table_name = _)]`
            accounts,
            block_headers,
            notes,
            nullifiers,
            transactions,
        },
    },
    errors::DatabaseError,
};

pub(crate) mod conv;

pub mod queries;
mod types;
pub(crate) mod utils;

pub use types::*;
pub(crate) use utils::*;

/// The page token and size to query from the DB.
#[derive(Debug, Copy, Clone)]
pub struct Page {
    pub token: Option<u64>,
    pub size: NonZeroUsize,
}
