use std::fmt::{Debug, Display, Formatter};

use miden_node_utils::formatting::format_opt;
use miden_objects::Word;
use miden_objects::account::{Account, AccountHeader, AccountId};
use miden_objects::asset::Asset;
use miden_objects::block::{AccountWitness, BlockNumber};
use miden_objects::note::{NoteExecutionMode, NoteTag};
use miden_objects::utils::{Deserializable, DeserializationError, Serializable};
use thiserror::Error;

use super::try_convert;
use crate::errors::{ConversionError, MissingFieldHelper};
use crate::generated as proto;

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
    pub account_commitment: Word,
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

impl From<&AccountInfo> for proto::account::AccountDetails {
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

impl TryInto<AccountProofRequest> for proto::rpc_store::account_proofs_request::AccountRequest {
    type Error = ConversionError;

    fn try_into(self) -> Result<AccountProofRequest, Self::Error> {
        let proto::rpc_store::account_proofs_request::AccountRequest {
            account_id,
            storage_requests,
        } = self;

        Ok(AccountProofRequest {
            account_id: account_id
                .clone()
                .ok_or(proto::rpc_store::account_proofs_request::AccountRequest::missing_field(
                    stringify!(account_id),
                ))?
                .try_into()?,
            storage_requests: try_convert(storage_requests).collect::<Result<_, _>>()?,
        })
    }
}

/// Represents a request for an account's storage map values and its proof of existence.
pub struct StorageMapKeysProof {
    /// Index of the storage map
    pub storage_index: u8,
    /// List of requested keys in the map
    pub storage_keys: Vec<Word>,
}

impl TryInto<StorageMapKeysProof>
    for proto::rpc_store::account_proofs_request::account_request::StorageRequest
{
    type Error = ConversionError;

    fn try_into(self) -> Result<StorageMapKeysProof, Self::Error> {
        let proto::rpc_store::account_proofs_request::account_request::StorageRequest {
            storage_slot_index,
            map_keys,
        } = self;

        Ok(StorageMapKeysProof {
            storage_index: storage_slot_index.try_into()?,
            storage_keys: try_convert(map_keys).collect::<Result<_, _>>()?,
        })
    }
}

// ACCOUNT WITNESS RECORD
// ================================================================================================

#[derive(Clone, Debug)]
pub struct AccountWitnessRecord {
    pub account_id: AccountId,
    pub witness: AccountWitness,
}

impl From<AccountWitnessRecord> for proto::account::AccountWitness {
    fn from(from: AccountWitnessRecord) -> Self {
        Self {
            account_id: Some(from.account_id.into()),
            witness_id: Some(from.witness.id().into()),
            commitment: Some(from.witness.state_commitment().into()),
            path: Some(from.witness.into_proof().into_parts().0.into()),
        }
    }
}

impl TryFrom<proto::account::AccountWitness> for AccountWitnessRecord {
    type Error = ConversionError;

    fn try_from(
        account_witness_record: proto::account::AccountWitness,
    ) -> Result<Self, Self::Error> {
        let witness_id = account_witness_record
            .witness_id
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(witness_id)))?
            .try_into()?;
        let commitment = account_witness_record
            .commitment
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(commitment)))?
            .try_into()?;
        let path = account_witness_record
            .path
            .as_ref()
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(path)))?
            .try_into()?;

        let witness = AccountWitness::new(witness_id, commitment, path).map_err(|err| {
            ConversionError::deserialization_error(
                "AccountWitness",
                DeserializationError::InvalidValue(err.to_string()),
            )
        })?;

        Ok(Self {
            account_id: account_witness_record
                .account_id
                .ok_or(proto::account::AccountWitness::missing_field(stringify!(account_id)))?
                .try_into()?,
            witness,
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
    pub account_commitment: Option<Word>,
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

impl From<AccountState>
    for proto::block_producer_store::transaction_inputs::AccountTransactionInputRecord
{
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

impl TryFrom<proto::block_producer_store::transaction_inputs::AccountTransactionInputRecord>
    for AccountState
{
    type Error = ConversionError;

    fn try_from(
        from: proto::block_producer_store::transaction_inputs::AccountTransactionInputRecord,
    ) -> Result<Self, Self::Error> {
        let account_id = from
            .account_id
            .ok_or(proto::block_producer_store::transaction_inputs::AccountTransactionInputRecord::missing_field(
                stringify!(account_id),
            ))?
            .try_into()?;

        let account_commitment = from
            .account_commitment
            .ok_or(proto::block_producer_store::transaction_inputs::AccountTransactionInputRecord::missing_field(
                stringify!(account_commitment),
            ))?
            .try_into()?;

        // If the commitment is equal to `Word::empty()`, it signifies that this is a new
        // account which is not yet present in the Store.
        let account_commitment = if account_commitment == Word::empty() {
            None
        } else {
            Some(account_commitment)
        };

        Ok(Self { account_id, account_commitment })
    }
}

// ASSET
// ================================================================================================

impl From<Asset> for proto::rpc_store::Asset {
    fn from(asset_from: Asset) -> Self {
        proto::rpc_store::Asset {
            asset: Some(Word::from(asset_from).into()),
        }
    }
}

impl TryFrom<proto::rpc_store::Asset> for Asset {
    type Error = ConversionError;

    fn try_from(value: proto::rpc_store::Asset) -> Result<Self, Self::Error> {
        let inner = value.asset.ok_or(proto::rpc_store::Asset::missing_field("asset"))?;
        let word = Word::try_from(inner)?;

        Asset::try_from(word).map_err(ConversionError::AssetError)
    }
}

// NETWORK ACCOUNT PREFIX
// ================================================================================================

pub type AccountPrefix = u32;

/// Newtype wrapper for network account prefix.
/// Provides type safety for accounts that are meant for network execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct NetworkAccountPrefix(u32);

impl std::fmt::Display for NetworkAccountPrefix {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl NetworkAccountPrefix {
    pub fn inner(&self) -> u32 {
        self.0
    }
}

impl From<NetworkAccountPrefix> for u32 {
    fn from(value: NetworkAccountPrefix) -> Self {
        value.inner()
    }
}

impl TryFrom<u32> for NetworkAccountPrefix {
    type Error = NetworkAccountError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value >> 30 != 0 {
            return Err(NetworkAccountError::InvalidPrefix(value));
        }
        Ok(NetworkAccountPrefix(value))
    }
}

impl TryFrom<AccountId> for NetworkAccountPrefix {
    type Error = NetworkAccountError;

    fn try_from(id: AccountId) -> Result<Self, Self::Error> {
        if !id.is_network() {
            return Err(NetworkAccountError::NotNetworkAccount(id));
        }
        let prefix = get_account_id_tag_prefix(id);
        Ok(NetworkAccountPrefix(prefix))
    }
}

impl TryFrom<NoteTag> for NetworkAccountPrefix {
    type Error = NetworkAccountError;

    fn try_from(tag: NoteTag) -> Result<Self, Self::Error> {
        if tag.execution_mode() != NoteExecutionMode::Network || !tag.is_single_target() {
            return Err(NetworkAccountError::InvalidExecutionMode(tag));
        }

        let tag_inner: u32 = tag.into();
        assert!(tag_inner >> 30 == 0, "first 2 bits have to be 0");
        Ok(NetworkAccountPrefix(tag_inner))
    }
}

#[derive(Debug, Error)]
pub enum NetworkAccountError {
    #[error("account ID {0} is not a valid network account ID")]
    NotNetworkAccount(AccountId),
    #[error("note tag {0} is not valid for network account execution")]
    InvalidExecutionMode(NoteTag),
    #[error("note prefix should be 30-bit long ({0} has non-zero in the 2 most significant bits)")]
    InvalidPrefix(u32),
}

/// Gets the 30-bit prefix of the account ID.
fn get_account_id_tag_prefix(id: AccountId) -> AccountPrefix {
    (id.prefix().as_u64() >> 34) as AccountPrefix
}
