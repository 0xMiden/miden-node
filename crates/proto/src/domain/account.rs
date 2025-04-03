use std::fmt::{Debug, Display, Formatter};

use miden_node_utils::formatting::format_opt;
use miden_objects::{
    Digest,
    account::{Account, AccountHeader, AccountId},
    block::BlockNumber,
    crypto::{hash::rpo::RpoDigest, merkle::SmtProof},
    utils::{Deserializable, Serializable},
};

use super::try_convert;
use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated as proto,
};

// ACCOUNT ID
// ================================================================================================

impl Display for proto::account::AccountId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x")?;
        for byte in &self.id {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Debug for proto::account::AccountId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

// INTO PROTO ACCOUNT ID
// ------------------------------------------------------------------------------------------------

impl From<&AccountId> for proto::account::AccountId {
    fn from(account_id: &AccountId) -> Self {
        (*account_id).into()
    }
}

impl From<AccountId> for proto::account::AccountId {
    fn from(account_id: AccountId) -> Self {
        Self { id: account_id.to_bytes() }
    }
}

// FROM PROTO ACCOUNT ID
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::account::AccountId> for AccountId {
    type Error = ConversionError;

    fn try_from(account_id: proto::account::AccountId) -> Result<Self, Self::Error> {
        AccountId::read_from_bytes(&account_id.id).map_err(|_| ConversionError::NotAValidFelt)
    }
}

// ACCOUNT UPDATE
// ================================================================================================

#[derive(Debug, PartialEq)]
pub struct AccountSummary {
    pub account_id: AccountId,
    pub account_commitment: RpoDigest,
    pub block_num: BlockNumber,
}

impl From<&AccountSummary> for proto::account::AccountSummary {
    fn from(update: &AccountSummary) -> Self {
        Self {
            account_id: Some(update.account_id.into()),
            account_commitment: Some(update.account_commitment.into()),
            block_num: update.block_num.as_u32(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct AccountInfo {
    pub summary: AccountSummary,
    pub details: Option<Account>,
}

impl From<&AccountInfo> for proto::account::AccountInfo {
    fn from(AccountInfo { summary, details }: &AccountInfo) -> Self {
        Self {
            summary: Some(summary.into()),
            details: details.as_ref().map(miden_objects::utils::Serializable::to_bytes),
        }
    }
}

// ACCOUNT STORAGE REQUEST
// ================================================================================================

/// Represents a request for an account proof alongside specific storage data.
pub struct AccountProofRequest {
    pub account_id: AccountId,
    pub storage_requests: Vec<StorageMapKeysProof>,
}

impl TryInto<AccountProofRequest> for proto::requests::get_account_proofs_request::AccountRequest {
    type Error = ConversionError;

    fn try_into(self) -> Result<AccountProofRequest, Self::Error> {
        let proto::requests::get_account_proofs_request::AccountRequest {
            account_id,
            storage_requests,
        } = self;

        Ok(AccountProofRequest {
            account_id: account_id
                .clone()
                .ok_or(proto::requests::get_account_proofs_request::AccountRequest::missing_field(
                    stringify!(account_id),
                ))?
                .try_into()?,
            storage_requests: try_convert(storage_requests)?,
        })
    }
}

/// Represents a request for an account's storage map values and its proof of existence.
pub struct StorageMapKeysProof {
    /// Index of the storage map
    pub storage_index: u8,
    /// List of requested keys in the map
    pub storage_keys: Vec<Digest>,
}

impl TryInto<StorageMapKeysProof> for proto::requests::get_account_proofs_request::StorageRequest {
    type Error = ConversionError;

    fn try_into(self) -> Result<StorageMapKeysProof, Self::Error> {
        let proto::requests::get_account_proofs_request::StorageRequest {
            storage_slot_index,
            map_keys,
        } = self;

        Ok(StorageMapKeysProof {
            storage_index: storage_slot_index.try_into()?,
            storage_keys: try_convert(map_keys)?,
        })
    }
}

// ACCOUNT WITNESS RECORD
// ================================================================================================

#[derive(Clone, Debug)]
pub struct AccountWitnessRecord {
    pub account_id: AccountId,
    pub proof: SmtProof,
}

impl From<AccountWitnessRecord> for proto::responses::AccountWitness {
    fn from(from: AccountWitnessRecord) -> Self {
        Self {
            account_id: Some(from.account_id.into()),
            proof: Some(from.proof.into()),
        }
    }
}

impl TryFrom<proto::responses::AccountWitness> for AccountWitnessRecord {
    type Error = ConversionError;

    fn try_from(
        account_witness_record: proto::responses::AccountWitness,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            account_id: account_witness_record
                .account_id
                .ok_or(proto::responses::AccountWitness::missing_field(stringify!(account_id)))?
                .try_into()?,
            proof: account_witness_record
                .proof
                .ok_or(proto::responses::AccountWitness::missing_field(stringify!(proof)))?
                .try_into()?,
        })
    }
}

// ACCOUNT STATE
// ================================================================================================

/// Information needed from the store to verify account in transaction.
#[derive(Debug)]
pub struct AccountState {
    /// Account ID
    pub account_id: AccountId,
    /// The account commitment in the store corresponding to tx's account ID
    pub account_commitment: Option<Digest>,
}

impl Display for AccountState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{{ account_id: {}, account_commitment: {} }}",
            self.account_id,
            format_opt(self.account_commitment.as_ref()),
        ))
    }
}

impl From<AccountState> for proto::responses::AccountTransactionInputRecord {
    fn from(from: AccountState) -> Self {
        Self {
            account_id: Some(from.account_id.into()),
            account_commitment: from.account_commitment.map(Into::into),
        }
    }
}

impl From<AccountHeader> for proto::account::AccountHeader {
    fn from(from: AccountHeader) -> Self {
        Self {
            vault_root: Some(from.vault_root().into()),
            storage_commitment: Some(from.storage_commitment().into()),
            code_commitment: Some(from.code_commitment().into()),
            nonce: from.nonce().into(),
        }
    }
}

impl TryFrom<proto::responses::AccountTransactionInputRecord> for AccountState {
    type Error = ConversionError;

    fn try_from(
        from: proto::responses::AccountTransactionInputRecord,
    ) -> Result<Self, Self::Error> {
        let account_id = from
            .account_id
            .ok_or(proto::responses::AccountTransactionInputRecord::missing_field(stringify!(
                account_id
            )))?
            .try_into()?;

        let account_commitment = from
            .account_commitment
            .ok_or(proto::responses::AccountTransactionInputRecord::missing_field(stringify!(
                account_commitment
            )))?
            .try_into()?;

        // If the commitment is equal to `Digest::default()`, it signifies that this is a new
        // account which is not yet present in the Store.
        let account_commitment = if account_commitment == Digest::default() {
            None
        } else {
            Some(account_commitment)
        };

        Ok(Self { account_id, account_commitment })
    }
}
