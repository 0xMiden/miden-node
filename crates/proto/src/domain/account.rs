use std::fmt::{Debug, Display, Formatter};

use miden_node_utils::formatting::format_opt;
use miden_objects::Word;
use miden_objects::account::{
    Account,
    AccountHeader,
    AccountId,
    AccountStorageHeader,
    StorageSlotType,
};
use miden_objects::asset::Asset;
use miden_objects::block::{AccountWitness, BlockNumber};
use miden_objects::crypto::merkle::MerklePath;
use miden_objects::note::{NoteExecutionMode, NoteTag};
use miden_objects::utils::{Deserializable, DeserializationError, Serializable};
use thiserror::Error;

use super::try_convert;
use crate::errors::{ConversionError, MissingFieldHelper};
use crate::generated::{self as proto};

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

// FROM PROTO ACCOUNT ID
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::account::AccountId> for AccountId {
    type Error = ConversionError;

    fn try_from(account_id: proto::account::AccountId) -> Result<Self, Self::Error> {
        AccountId::read_from_bytes(&account_id.id).map_err(|_| ConversionError::NotAValidFelt)
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

// ACCOUNT PROOF REQUEST
// ================================================================================================

/// Represents a request for an account proof.
pub struct AccountProofRequest {
    pub account_id: AccountId,
    pub account_details: Option<AccountDetailRequest>,
}

impl TryFrom<proto::rpc_store::AccountProofRequest> for AccountProofRequest {
    type Error = ConversionError;

    fn try_from(request: proto::rpc_store::AccountProofRequest) -> Result<Self, Self::Error> {
        let proto::rpc_store::AccountProofRequest { account_id, account_details } = request;

        let account_id = account_id
            .ok_or(proto::rpc_store::AccountProofRequest::missing_field(stringify!(account_id)))?
            .try_into()?;

        let account_details = account_details.map(TryFrom::try_from).transpose()?;

        Ok(AccountProofRequest { account_id, account_details })
    }
}

/// Represents a request for account details alongside specific storage data.
pub struct AccountDetailRequest {
    pub code_commitment: Option<Word>,
    pub storage_requests: Vec<StorageMapKeysProof>,
}

impl TryFrom<proto::rpc_store::account_proof_request::AccountDetailsRequest>
    for AccountDetailRequest
{
    type Error = ConversionError;

    fn try_from(
        request: proto::rpc_store::account_proof_request::AccountDetailsRequest,
    ) -> Result<Self, Self::Error> {
        let proto::rpc_store::account_proof_request::AccountDetailsRequest {
            code_commitment,
            storage_requests,
        } = request;

        let storage_requests = try_convert(storage_requests).collect::<Result<_, _>>()?;
        let code_commitment = code_commitment.map(Word::try_from).transpose()?;

        Ok(AccountDetailRequest { code_commitment, storage_requests })
    }
}

/// Represents a request for an account's storage map values and its proof of existence.
pub struct StorageMapKeysProof {
    /// Index of the storage map
    pub storage_index: u8,
    /// List of requested keys in the map
    pub storage_keys: Vec<Word>,
}

impl TryFrom<proto::rpc_store::account_proof_request::account_details_request::StorageRequest>
    for StorageMapKeysProof
{
    type Error = ConversionError;

    fn try_from(
        request: proto::rpc_store::account_proof_request::account_details_request::StorageRequest,
    ) -> Result<Self, Self::Error> {
        let proto::rpc_store::account_proof_request::account_details_request::StorageRequest {
            storage_slot_index,
            map_keys,
        } = request;

        Ok(StorageMapKeysProof {
            storage_index: storage_slot_index.try_into()?,
            storage_keys: try_convert(map_keys).collect::<Result<_, _>>()?,
        })
    }
}

// ACCOUNT HEADER CONVERSIONS
//================================================================================================

impl TryFrom<proto::account::AccountHeader> for AccountHeader {
    type Error = ConversionError;

    fn try_from(value: proto::account::AccountHeader) -> Result<Self, Self::Error> {
        let proto::account::AccountHeader {
            account_id,
            vault_root,
            storage_commitment,
            code_commitment,
            nonce,
        } = value;

        let account_id = account_id
            .ok_or(proto::account::AccountHeader::missing_field(stringify!(account_id)))?
            .try_into()?;
        let vault_root = vault_root
            .ok_or(proto::account::AccountHeader::missing_field(stringify!(vault_root)))?
            .try_into()?;
        let storage_commitment = storage_commitment
            .ok_or(proto::account::AccountHeader::missing_field(stringify!(storage_commitment)))?
            .try_into()?;
        let code_commitment = code_commitment
            .ok_or(proto::account::AccountHeader::missing_field(stringify!(code_commitment)))?
            .try_into()?;
        let nonce = nonce.try_into().map_err(|_e| ConversionError::NotAValidFelt)?;

        Ok(AccountHeader::new(
            account_id,
            nonce,
            vault_root,
            storage_commitment,
            code_commitment,
        ))
    }
}

impl From<AccountHeader> for proto::account::AccountHeader {
    fn from(header: AccountHeader) -> Self {
        proto::account::AccountHeader {
            account_id: Some(header.id().into()),
            vault_root: Some(header.vault_root().into()),
            storage_commitment: Some(header.storage_commitment().into()),
            code_commitment: Some(header.code_commitment().into()),
            nonce: header.nonce().as_int(),
        }
    }
}

impl From<AccountStorageHeader> for proto::account::AccountStorageHeader {
    fn from(value: AccountStorageHeader) -> Self {
        let slots = value
            .slots()
            .map(|(slot_type, slot_value)| proto::account::account_storage_header::StorageSlot {
                slot_type: storage_slot_type_to_raw(*slot_type),
                commitment: Some(proto::primitives::Digest::from(*slot_value)),
            })
            .collect();

        Self { slots }
    }
}

const fn storage_slot_type_from_raw(slot_type: u32) -> Result<StorageSlotType, ConversionError> {
    Ok(match slot_type {
        0 => StorageSlotType::Map,
        1 => StorageSlotType::Value,
        _ => return Err(ConversionError::EnumDiscriminantOutOfRange),
    })
}

const fn storage_slot_type_to_raw(slot_type: StorageSlotType) -> u32 {
    match slot_type {
        StorageSlotType::Map => 0,
        StorageSlotType::Value => 1,
    }
}

/// Represents account details returned in response to an account proof request.
pub struct AccountDetailsResponse {
    pub account_header: AccountHeader,
    pub storage_header: AccountStorageHeader,
    pub account_code: Option<Vec<u8>>,
    pub storage_proofs: Vec<StorageSlotMapProof>,
}

impl TryFrom<proto::rpc_store::account_proof::AccountDetailsResponse> for AccountDetailsResponse {
    type Error = ConversionError;

    fn try_from(
        value: proto::rpc_store::account_proof::AccountDetailsResponse,
    ) -> Result<Self, Self::Error> {
        let proto::rpc_store::account_proof::AccountDetailsResponse {
            header,
            storage_header,
            account_code,
            storage_maps,
        } = value;

        let account_header = header
            .ok_or(proto::rpc_store::account_proof::AccountDetailsResponse::missing_field(
                stringify!(header),
            ))?
            .try_into()?;

        let storage_header = storage_header.ok_or(
            proto::rpc_store::account_proof::AccountDetailsResponse::missing_field(stringify!(
                storage_header
            )),
        )?;

        let storage_header_entries = storage_header
            .slots
            .into_iter()
            .map(|slot| {
                let slot_type = storage_slot_type_from_raw(slot.slot_type)?;
                let commitment = slot
                    .commitment
                    .ok_or(proto::account::account_storage_header::StorageSlot::missing_field(
                        stringify!(commitment),
                    ))?
                    .try_into()?;
                Ok((slot_type, commitment))
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;

        let storage_header = AccountStorageHeader::new(storage_header_entries);

        let storage_proofs = try_convert(storage_maps).collect::<Result<_, _>>()?;

        Ok(AccountDetailsResponse {
            account_header,
            storage_header,
            account_code,
            storage_proofs,
        })
    }
}

impl From<AccountDetailsResponse> for proto::rpc_store::account_proof::AccountDetailsResponse {
    fn from(value: AccountDetailsResponse) -> Self {
        let AccountDetailsResponse {
            account_header,
            storage_header,
            account_code,
            storage_proofs,
        } = value;

        let header = Some(proto::account::AccountHeader::from(account_header));
        let storage_header = Some(proto::account::AccountStorageHeader::from(storage_header));

        let storage_maps = storage_proofs
            .into_iter()
            .map(|proof| {
                let storage_slot = u32::from(proof.storage_slot);
                let smt_proof = Some(proto::primitives::SmtOpening::from(proof.proof));
                proto::rpc_store::account_proof::account_details_response::StorageSlotMapProof {
                    storage_slot,
                    smt_proof,
                }
            })
            .collect();

        Self {
            header,
            storage_header,
            account_code,
            storage_maps,
        }
    }
}

/// Represents a storage slot map proof.
pub struct StorageSlotMapProof {
    pub storage_slot: u8,
    pub proof: miden_objects::crypto::merkle::SmtProof,
}

impl TryFrom<proto::rpc_store::account_proof::account_details_response::StorageSlotMapProof>
    for StorageSlotMapProof
{
    type Error = ConversionError;

    fn try_from(
        proof: proto::rpc_store::account_proof::account_details_response::StorageSlotMapProof,
    ) -> Result<Self, Self::Error> {
        let proto::rpc_store::account_proof::account_details_response::StorageSlotMapProof {
            storage_slot,
            smt_proof,
        } = proof;

        let storage_slot = storage_slot.try_into().map_err(ConversionError::TryFromIntError)?;
        let proof =
        smt_proof.ok_or(proto::rpc_store::account_proof::account_details_response::StorageSlotMapProof::missing_field(stringify!(proof)))?.try_into()?;

        Ok(StorageSlotMapProof { storage_slot, proof })
    }
}

// ACCOUNT PROOF
// ================================================================================================

/// Represents an account proof.
pub struct AccountProofResponse {
    pub block_num: BlockNumber,
    pub witness: AccountWitnessRecord,
    pub account_details: Option<AccountDetailsResponse>,
}

impl TryFrom<proto::rpc_store::AccountProof> for AccountProofResponse {
    type Error = ConversionError;

    fn try_from(proof: proto::rpc_store::AccountProof) -> Result<Self, Self::Error> {
        let proto::rpc_store::AccountProof { block_num, witness, details } = proof;

        let witness =
            witness.ok_or(proto::rpc_store::AccountProof::missing_field(stringify!(witness)))?;

        let witness = AccountWitnessRecord::try_from(witness)?;

        let account_details = details.map(AccountDetailsResponse::try_from).transpose()?;

        Ok(AccountProofResponse {
            block_num: block_num.into(),
            witness,
            account_details,
        })
    }
}

impl From<AccountProofResponse> for proto::rpc_store::AccountProof {
    fn from(value: AccountProofResponse) -> Self {
        let AccountProofResponse { block_num, witness, account_details } = value;

        proto::rpc_store::AccountProof {
            block_num: block_num.as_u32(),
            witness: Some(proto::account::AccountWitness::from(witness)),
            details: account_details
                .map(proto::rpc_store::account_proof::AccountDetailsResponse::from),
        }
    }
}

// ACCOUNT WITNESS RECORD
// ================================================================================================

#[derive(Clone, Debug)]
pub struct AccountWitnessRecord {
    pub account_id: AccountId,
    pub witness: AccountWitness,
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

impl From<AccountWitnessRecord> for proto::account::AccountWitness {
    fn from(from: AccountWitnessRecord) -> Self {
        let merkle_path: MerklePath = from.witness.clone().into_proof().into_parts().0.into();
        Self {
            account_id: Some(from.account_id.into()),
            witness_id: Some(from.witness.id().into()),
            commitment: Some(from.witness.state_commitment().into()),
            path: Some(merkle_path.into()),
        }
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

// ASSET
// ================================================================================================

impl TryFrom<proto::primitives::Asset> for Asset {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::Asset) -> Result<Self, Self::Error> {
        let inner = value.asset.ok_or(proto::primitives::Asset::missing_field("asset"))?;
        let word = Word::try_from(inner)?;

        Asset::try_from(word).map_err(ConversionError::AssetError)
    }
}

impl From<Asset> for proto::primitives::Asset {
    fn from(asset_from: Asset) -> Self {
        proto::primitives::Asset {
            asset: Some(Word::from(asset_from).into()),
        }
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

impl From<NetworkAccountPrefix> for u32 {
    fn from(value: NetworkAccountPrefix) -> Self {
        value.inner()
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
