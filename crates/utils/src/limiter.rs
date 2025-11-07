//! Limit the size of a parameter list for a specific parameter
//!
//! Used for:
//! 1. the external facing RPC
//! 2. limiting SQL statements not exceeding parameter limits
//!
//! The 1st is good to terminate invalid requests as early as possible,
//! where the second is both a fallback and a safeguard not benching
//! pointless parameter combinations.

#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
#[error("parameter {which} exceeded limit {limit}: {size}")]
pub struct QueryLimitError {
    which: &'static str,
    size: usize,
    limit: usize,
}

/// Checks limits against the desired query parameters, per query parameter and
/// bails if they exceed a defined value.
pub trait QueryParamLimiter {
    /// Name of the parameter to mention in the error.
    const PARAM_NAME: &'static str;
    /// Limit that causes a bail if exceeded.
    const LIMIT: usize;
    /// Do the actual check.
    fn check(size: usize) -> Result<(), QueryLimitError> {
        if size > Self::LIMIT {
            Err(QueryLimitError {
                which: Self::PARAM_NAME,
                size,
                limit: Self::LIMIT,
            })?;
        }
        Ok(())
    }
}

/// Used for the following RPC endpoints
/// * `state_sync`
pub struct QueryParamAccountIdLimit;
impl QueryParamLimiter for QueryParamAccountIdLimit {
    const PARAM_NAME: &str = "account_id";
    const LIMIT: usize = 1000;
}

/// Used for the following RPC endpoints
/// * `select_nullifiers_by_prefix`
pub struct QueryParamNullifierPrefixLimit;
impl QueryParamLimiter for QueryParamNullifierPrefixLimit {
    const PARAM_NAME: &str = "nullifier_prefix";
    const LIMIT: usize = 1000;
}

/// Used for the following RPC endpoints
/// * `select_nullifiers_by_prefix`
/// * `sync_nullifiers`
/// * `sync_state`
pub struct QueryParamNullifierLimit;
impl QueryParamLimiter for QueryParamNullifierLimit {
    const PARAM_NAME: &str = "nullifier";
    const LIMIT: usize = 1000;
}

/// Used for the following RPC endpoints
/// * `get_note_sync`
pub struct QueryParamNoteTagLimit;
impl QueryParamLimiter for QueryParamNoteTagLimit {
    const PARAM_NAME: &str = "note_tag";
    const LIMIT: usize = 1000;
}

/// Used for the following RPC endpoints
/// `select_notes_by_id`
pub struct QueryParamNoteIdLimit;
impl QueryParamLimiter for QueryParamNoteIdLimit {
    const PARAM_NAME: &str = "note_id";
    const LIMIT: usize = 1000;
}

/// Used for internal queries retrieving note inclusion proofs by commitment.
pub struct QueryParamNoteCommitmentLimit;
impl QueryParamLimiter for QueryParamNoteCommitmentLimit {
    const PARAM_NAME: &str = "note_commitment";
    const LIMIT: usize = 1000;
}

/// Only used internally, not exposed via public RPC.
pub struct QueryParamBlockLimit;
impl QueryParamLimiter for QueryParamBlockLimit {
    const PARAM_NAME: &str = "block_header";
    const LIMIT: usize = 1000;
}
