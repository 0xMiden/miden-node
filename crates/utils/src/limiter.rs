//! Limits for RPC and store parameters and payload sizes.
//!
//! # Rationale
//! - Parameter limits are kept at 1000 items across all multi-value RPC parameters. This caps
//!   worst-case SQL `IN` clauses and keeps responses comfortably under the 4 MiB payload budget
//!   enforced in the store.
//! - Limits are enforced both at the RPC boundary and inside the store to prevent bypasses and to
//!   avoid expensive queries even if validation is skipped earlier in the stack.
//! - `MAX_PAGINATED_PAYLOAD_BYTES` is set to 4 MiB (e.g. 1000 nullifier rows at ~36 B each, 1000
//!   transactions summaries streamed in chunks).
//!
//! Add new limits here so callers share the same values and rationale.

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

/// Maximum payload size (in bytes) for paginated responses returned by the
/// store.
pub const MAX_RESPONSE_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

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
