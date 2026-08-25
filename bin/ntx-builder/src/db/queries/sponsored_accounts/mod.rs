//! Resolves the accounts whose pending feature notes just gained a sponsorship.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::{InList, WriteTx};
use miden_protocol::account::AccountId;
use miden_protocol::utils::serde::Serializable;

use crate::sponsorship::SponsorshipNote;

const SQL: &str = include_str!("sponsored_account.sql");

/// Returns, for each sponsorship, the account targeted by the pending feature note it is bound to.
///
/// The result may name the same account several times (once per sponsorship); the coordinator
/// counts every occurrence towards the account's work counter.
pub fn sponsored_accounts(
    tx: &WriteTx<'_>,
    sponsorships: &[SponsorshipNote],
) -> Result<Vec<AccountId>, DatabaseError> {
    let serialized: Vec<Vec<u8>> = sponsorships
        .iter()
        .map(|sponsorship| sponsorship.feature_note_id().to_bytes())
        .collect();
    let feature_note_ids = InList::from_blobs(serialized.iter().map(Vec::as_slice));

    tx.query(SQL, &[&feature_note_ids], |row| row.get::<AccountId>(0))
}
