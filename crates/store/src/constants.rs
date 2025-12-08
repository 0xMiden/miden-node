//! Constants used for pagination and size limits across the store.

/// Maximum number of account IDs that can be requested in a single query.
pub const MAX_ACCOUNT_IDS: usize = 100;

/// Maximum number of nullifiers that can be requested in a single query.
pub const MAX_NULLIFIERS: usize = 100;

/// Maximum number of note tags that can be requested in a single query.
pub const MAX_NOTE_TAGS: usize = 100;

/// Maximum number of note IDs that can be requested in a single query.
pub const MAX_NOTE_IDS: usize = 100;

/// Maximum payload size for all paginated endpoints (4 MB).
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
