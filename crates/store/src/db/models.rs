//! Defines models for usage with the diesel API

use diesel::prelude::*;

use crate::{db::schema::accounts, errors::DatabaseError};

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = accounts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountInfoRawRow {
    pub account_id: Vec<u8>,
    #[allow(dead_code)]
    pub network_account_id_prefix: Option<i64>,
    pub account_commitment: Vec<u8>,
    pub block_num: i64,
    pub details: Option<Vec<u8>>,
}

use miden_lib::utils::Deserializable;
use miden_node_proto as proto;
use miden_objects::{
    account::{Account, AccountId},
    block::BlockNumber,
    crypto::hash::rpo::RpoDigest,
};

impl TryInto<proto::domain::account::AccountInfo> for AccountInfoRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<proto::domain::account::AccountInfo, Self::Error> {
        use proto::domain::account::{AccountInfo, AccountSummary};
        let account_id = AccountId::read_from_bytes(&self.account_id[..])?;
        let account_commitment = RpoDigest::read_from_bytes(&self.account_commitment[..])?;
        let block_num = u32::try_from(self.block_num)
            .expect("Only values less than `u32::MAX` enter the databe");
        let block_num = BlockNumber::from(block_num);
        let summary = AccountSummary {
            account_id,
            account_commitment,
            block_num,
        };
        let details = self.details.as_deref().map(Account::read_from_bytes).transpose()?;
        Ok(AccountInfo { summary, details })
    }
}
