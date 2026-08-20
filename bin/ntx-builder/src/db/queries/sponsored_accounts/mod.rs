//! Resolves the accounts whose pending feature notes just gained a sponsorship.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::WriteTx;
use miden_protocol::account::AccountId;

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
    let mut accounts = Vec::new();
    for sponsorship in sponsorships {
        let rows =
            tx.query(SQL, &[&sponsorship.feature_note_id()], |row| row.get::<AccountId>(0))?;
        accounts.extend(rows);
    }
    Ok(accounts)
}
