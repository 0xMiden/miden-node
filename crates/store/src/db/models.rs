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
//! 
//! `#[derive(Queryable)` shoulde be used with `#[diesel(deserialize_as = FooRaw)]`
//! to do an indirection via `Type` and using `let foo: Foo = raw.try_into()?`;
//!  
use diesel::{deserialize::FromSql, prelude::*, sql_types::Binary, sqlite::Sqlite};

use crate::{
    db::{
        schema::{
            // the list of tables
            // referenced in `#[diesel(table_name = _)]`
            accounts, block_headers, notes, nullifiers, transactions
        }, NullifierInfo
    },
    errors::DatabaseError,
};

fn raw_sql_to_block_number(raw: i64) -> BlockNumber {
    BlockNumber::from(
        u32::try_from(raw).expect("Only values less than `u32::MAX` enter the database"),
    )
}

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

use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto::{self as proto};
use miden_objects::{
    account::{Account, AccountId},
    block::{BlockHeader, BlockNumber},
    crypto::{hash::rpo::RpoDigest, merkle::MerklePath},
    note::Nullifier, transaction::TransactionId,
};

impl TryInto<proto::domain::account::AccountInfo> for AccountInfoRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<proto::domain::account::AccountInfo, Self::Error> {
        use proto::domain::account::{AccountInfo, AccountSummary};
        let account_id = AccountId::read_from_bytes(&self.account_id[..])?;
        let account_commitment = RpoDigest::read_from_bytes(&self.account_commitment[..])?;
        let block_num = raw_sql_to_block_number(self.block_num);
        let summary = AccountSummary {
            account_id,
            account_commitment,
            block_num,
        };
        let details = self.details.as_deref().map(Account::read_from_bytes).transpose()?;
        Ok(AccountInfo { summary, details })
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = nullifiers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct NullifierRow {
    #[diesel(deserialize_as = NullifierRaw)]
    pub nullifier: Nullifier,
    #[allow(dead_code)]
    pub nullifier_prefix: i32, // TODO most usecases do not require this to be actually loaded
    #[diesel(deserialize_as = BlockNumberRaw)]
    pub block_num: BlockNumber,
}

impl TryInto<NullifierInfo> for NullifierRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NullifierInfo, Self::Error> {
        let nullifier = Nullifier::read_from_bytes(&self.nullifier)?;
        let block_num = raw_sql_to_block_number(self.block_num);
        Ok(NullifierInfo { nullifier, block_num })
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = block_headers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BlockHeadersRow {
    #[diesel(deserialize_as = BlockHeaderRaw)]
    pub block_num: BlockNumber,
    #[diesel(deserialize_as = BlockHeaderRaw)]
    pub block_header: BlockHeader,
}
impl TryInto<BlockHeader> for BlockHeadersRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockHeader, Self::Error> {
        let block_header = BlockHeader::read_from_bytes(&self.block_header[..])?;
        Ok(block_header)
    }
}



#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = block_headers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BlockHeaderRaw {
    pub block_header: Vec<u8>,
}
impl TryInto<BlockHeader> for BlockHeaderRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockHeader, Self::Error> {
        let block_header = BlockHeader::read_from_bytes(&self.block_header[..])?;
        Ok(block_header)
    }
}


// CRATE EXTERNAL PRIMITIVES

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BlockNumberRaw {
    block_num: u32,
}
impl TryInto<BlockNumber> for BlockNumberRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockNumber, Self::Error> {
        let block_header = BlockNumber::from(self.block_num as u32)?;
        Ok(block_header)
    }
}
impl From<BlockNumberRaw> for BlockNumber {
    fn from(value: BlockNumberRaw) -> Self {
        BlockNumber::from( value.block_num as _ )
    }
}


#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionIdRaw {
    transaction_id: u32,
}
impl TryInto<TransactionId> for TransactionIdRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<TransactionIdRaw, Self::Error> {
        let txid = TransactionId::read_from_bytes(&self.transaction_id[..])?;
        Ok(txid)
    }
}

impl From<TransactionIdRaw> for TransactionId {
    fn from(value: TransactionIdRaw) -> Self {
        Self::from(value.transcation_id as _ )
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = nullifiers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct NullifierRaw {
    nullifier: Vec<u8>,
}
impl TryInto<BlockNumber> for NullifierRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockNumber, Self::Error> {
        let nullifier = Nullifier::read_from_bytes(&self.nullifier[..])?;
        Ok(nullifier)
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
pub struct RpoDigestRaw {
    digest: Vec<u8>,
}
impl TryInto<RpoDigest> for RpoDigestRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<RpoDigest, Self::Error> {
        let digest = RpoDigest::read_from_bytes(&self.digest[..])?;
        Ok(digest)
    }
}
impl From<RpoDigestRaw> for RpoDigest {
    fn from(value: RpoDigestRaw) -> Self {
        value.try_into()
    }
}


#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = accounts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountIdRaw {
    account_id: Vec<u8>,
}
impl TryInto<AccountId> for AccountIdRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<AccountId, Self::Error> {
        let id = AccountId::read_from_bytes(&self.account_id[..])?;
        Ok(id)
    }
}
impl From<AccountIdRaw> for AccountId {
    fn from(value: AccountIdRaw) -> Self {
        value.try_into().unwrap()
    }
}



#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct MerklePathRaw {
    merklepath: Vec<u8>,
}
impl TryInto<MerklePath> for MerklePathRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<MerklePath, Self::Error> {
        let mp = MerklePath::read_from_bytes(&self.merklepath[..])?;
        Ok(mp)
    }
}
