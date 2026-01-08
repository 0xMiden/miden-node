use std::fmt::{Debug, Display, Formatter};

use miden_node_utils::formatting::format_opt;
use miden_protocol::account::{
    Account,
    AccountCode,
    AccountHeader,
    AccountId,
    AccountStorage,
    AccountStorageHeader,
    PartialAccount,
    PartialStorage,
    StorageMap,
    StorageSlotHeader,
    StorageSlotName,
    StorageSlotType,
};
use miden_protocol::asset::{Asset, AssetVault, PartialVault};
use miden_protocol::block::BlockNumber;
use miden_protocol::block::account_tree::AccountWitness;
use miden_protocol::crypto::merkle::SparseMerklePath;
use miden_protocol::note::{NoteExecutionMode, NoteTag};
use miden_protocol::utils::{Deserializable, DeserializationError, Serializable};
use miden_protocol::{Word, ZERO};
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
            details: details.as_ref().map(miden_protocol::utils::Serializable::to_bytes),
        }
    }
}

// ACCOUNT PROOF REQUEST
// ================================================================================================

/// Represents a request for an account proof.
pub struct AccountProofRequest {
    pub account_id: AccountId,
    // If not present, the latest account proof references the latest available
    pub block_num: Option<BlockNumber>,
    pub details: Option<AccountDetailRequest>,
}

impl TryFrom<proto::rpc::AccountProofRequest> for AccountProofRequest {
    type Error = ConversionError;

    fn try_from(value: proto::rpc::AccountProofRequest) -> Result<Self, Self::Error> {
        let proto::rpc::AccountProofRequest { account_id, block_num, details } = value;

        let account_id = account_id
            .ok_or(proto::rpc::AccountProofRequest::missing_field(stringify!(account_id)))?
            .try_into()?;
        let block_num = block_num.map(Into::into);

        let details = details.map(TryFrom::try_from).transpose()?;

        Ok(AccountProofRequest { account_id, block_num, details })
    }
}

/// Represents a request for account details alongside specific storage data.
pub struct AccountDetailRequest {
    pub code_commitment: Option<Word>,
    pub asset_vault_commitment: Option<Word>,
    pub storage_requests: Vec<StorageMapRequest>,
}

impl TryFrom<proto::rpc::account_proof_request::AccountDetailRequest> for AccountDetailRequest {
    type Error = ConversionError;

    fn try_from(
        value: proto::rpc::account_proof_request::AccountDetailRequest,
    ) -> Result<Self, Self::Error> {
        let proto::rpc::account_proof_request::AccountDetailRequest {
            code_commitment,
            asset_vault_commitment,
            storage_maps,
        } = value;

        let code_commitment = code_commitment.map(TryFrom::try_from).transpose()?;
        let asset_vault_commitment = asset_vault_commitment.map(TryFrom::try_from).transpose()?;
        let storage_requests = try_convert(storage_maps).collect::<Result<_, _>>()?;

        Ok(AccountDetailRequest {
            code_commitment,
            asset_vault_commitment,
            storage_requests,
        })
    }
}

impl TryFrom<proto::account::AccountStorageHeader> for AccountStorageHeader {
    type Error = ConversionError;

    fn try_from(value: proto::account::AccountStorageHeader) -> Result<Self, Self::Error> {
        let proto::account::AccountStorageHeader { slots } = value;

        let slot_headers = slots
            .into_iter()
            .map(|slot| {
                let slot_name = StorageSlotName::new(slot.slot_name)?;
                let slot_type = storage_slot_type_from_raw(slot.slot_type)?;
                let commitment =
                    slot.commitment.ok_or(ConversionError::NotAValidFelt)?.try_into()?;
                Ok(StorageSlotHeader::new(slot_name, slot_type, commitment))
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;

        Ok(AccountStorageHeader::new(slot_headers)?)
    }
}

impl TryFrom<proto::rpc::account_storage_details::AccountStorageMapDetails>
    for AccountStorageMapDetails
{
    type Error = ConversionError;

    fn try_from(
        value: proto::rpc::account_storage_details::AccountStorageMapDetails,
    ) -> Result<Self, Self::Error> {
        let proto::rpc::account_storage_details::AccountStorageMapDetails {
            slot_name,
            too_many_entries,
            entries,
        } = value;

        let slot_name = StorageSlotName::new(slot_name)?;

        // Extract map_entries from the MapEntries message
        let map_entries = if let Some(entries) = entries {
            entries
                .entries
                .into_iter()
                .map(|entry| {
                    let key = entry
                        .key
                        .ok_or(proto::rpc::account_storage_details::account_storage_map_details::map_entries::StorageMapEntry::missing_field(
                            stringify!(key),
                        ))?
                        .try_into()?;
                    let value = entry
                        .value
                        .ok_or(proto::rpc::account_storage_details::account_storage_map_details::map_entries::StorageMapEntry::missing_field(
                            stringify!(value),
                        ))?
                        .try_into()?;
                    Ok((key, value))
                })
                .collect::<Result<Vec<_>, ConversionError>>()?
        } else {
            Vec::new()
        };

        Ok(Self { slot_name, too_many_entries, map_entries })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapRequest {
    pub slot_name: StorageSlotName,
    pub slot_data: SlotData,
}

impl TryFrom<proto::rpc::account_proof_request::account_detail_request::StorageMapDetailRequest>
    for StorageMapRequest
{
    type Error = ConversionError;

    fn try_from(
        value: proto::rpc::account_proof_request::account_detail_request::StorageMapDetailRequest,
    ) -> Result<Self, Self::Error> {
        let proto::rpc::account_proof_request::account_detail_request::StorageMapDetailRequest {
            slot_name,
            slot_data,
        } = value;

        let slot_name = StorageSlotName::new(slot_name)?;
        let slot_data = slot_data.ok_or(proto::rpc::account_proof_request::account_detail_request::StorageMapDetailRequest::missing_field(stringify!(slot_data)))?.try_into()?;

        Ok(StorageMapRequest { slot_name, slot_data })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotData {
    All,
    MapKeys(Vec<Word>),
}

impl TryFrom<proto::rpc::account_proof_request::account_detail_request::storage_map_detail_request::SlotData>
    for SlotData
{
    type Error = ConversionError;

    fn try_from(value: proto::rpc::account_proof_request::account_detail_request::storage_map_detail_request::SlotData) -> Result<Self, Self::Error> {
        use proto::rpc::account_proof_request::account_detail_request::storage_map_detail_request::SlotData as ProtoSlotData;

        Ok(match value {
            ProtoSlotData::AllEntries(true) => SlotData::All,
            ProtoSlotData::AllEntries(false) => return Err(ConversionError::EnumDiscriminantOutOfRange),
            ProtoSlotData::MapKeys(keys) => {
                let keys = try_convert(keys.map_keys).collect::<Result<Vec<_>, _>>()?;
                SlotData::MapKeys(keys)
            },
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
            .map(|slot_header| proto::account::account_storage_header::StorageSlot {
                slot_name: slot_header.name().to_string(),
                slot_type: storage_slot_type_to_raw(slot_header.slot_type()),
                commitment: Some(proto::primitives::Digest::from(slot_header.value())),
            })
            .collect();

        Self { slots }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountVaultDetails {
    pub too_many_assets: bool,
    pub assets: Vec<Asset>,
}
impl AccountVaultDetails {
    const MAX_RETURN_ENTRIES: usize = 1000;

    pub fn new(vault: &AssetVault) -> Self {
        if vault.assets().nth(Self::MAX_RETURN_ENTRIES).is_some() {
            Self::too_many()
        } else {
            Self {
                too_many_assets: false,
                assets: Vec::from_iter(vault.assets()),
            }
        }
    }

    pub fn empty() -> Self {
        Self {
            too_many_assets: false,
            assets: Vec::new(),
        }
    }

    fn too_many() -> Self {
        Self {
            too_many_assets: true,
            assets: Vec::new(),
        }
    }
}

impl TryFrom<proto::rpc::AccountVaultDetails> for AccountVaultDetails {
    type Error = ConversionError;

    fn try_from(value: proto::rpc::AccountVaultDetails) -> Result<Self, Self::Error> {
        let proto::rpc::AccountVaultDetails { too_many_assets, assets } = value;

        let assets =
            Result::<Vec<_>, ConversionError>::from_iter(assets.into_iter().map(|asset| {
                let asset = asset
                    .asset
                    .ok_or(proto::primitives::Asset::missing_field(stringify!(asset)))?;
                let asset = Word::try_from(asset)?;
                Asset::try_from(asset).map_err(ConversionError::AssetError)
            }))?;
        Ok(Self { too_many_assets, assets })
    }
}

impl From<AccountVaultDetails> for proto::rpc::AccountVaultDetails {
    fn from(value: AccountVaultDetails) -> Self {
        let AccountVaultDetails { too_many_assets, assets } = value;

        Self {
            too_many_assets,
            assets: Vec::from_iter(assets.into_iter().map(|asset| proto::primitives::Asset {
                asset: Some(proto::primitives::Digest::from(Word::from(asset))),
            })),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStorageMapDetails {
    pub slot_name: StorageSlotName,
    pub too_many_entries: bool,
    pub map_entries: Vec<(Word, Word)>,
}

impl AccountStorageMapDetails {
    const MAX_RETURN_ENTRIES: usize = 1000;

    pub fn new(slot_name: StorageSlotName, slot_data: SlotData, storage_map: &StorageMap) -> Self {
        match slot_data {
            SlotData::All => Self::from_all_entries(slot_name, storage_map),
            SlotData::MapKeys(keys) => Self::from_specific_keys(slot_name, &keys[..], storage_map),
        }
    }

    fn from_all_entries(slot_name: StorageSlotName, storage_map: &StorageMap) -> Self {
        if storage_map.num_entries() > Self::MAX_RETURN_ENTRIES {
            Self::too_many_entries(slot_name)
        } else {
            let map_entries = Vec::from_iter(storage_map.entries().map(|(k, v)| (*k, *v)));
            Self {
                slot_name,
                too_many_entries: false,
                map_entries,
            }
        }
    }

    fn from_specific_keys(
        slot_name: StorageSlotName,
        keys: &[Word],
        storage_map: &StorageMap,
    ) -> Self {
        if keys.len() > Self::MAX_RETURN_ENTRIES {
            Self::too_many_entries(slot_name)
        } else {
            // TODO For now, we return all entries instead of specific keys with proofs
            Self::from_all_entries(slot_name, storage_map)
        }
    }

    pub fn too_many_entries(slot_name: StorageSlotName) -> Self {
        Self {
            slot_name,
            too_many_entries: true,
            map_entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStorageDetails {
    pub header: AccountStorageHeader,
    pub map_details: Vec<AccountStorageMapDetails>,
}

impl TryFrom<proto::rpc::AccountStorageDetails> for AccountStorageDetails {
    type Error = ConversionError;

    fn try_from(value: proto::rpc::AccountStorageDetails) -> Result<Self, Self::Error> {
        let proto::rpc::AccountStorageDetails { header, map_details } = value;

        let header = header
            .ok_or(proto::rpc::AccountStorageDetails::missing_field(stringify!(header)))?
            .try_into()?;

        let map_details = try_convert(map_details).collect::<Result<Vec<_>, _>>()?;

        Ok(Self { header, map_details })
    }
}

impl From<AccountStorageDetails> for proto::rpc::AccountStorageDetails {
    fn from(value: AccountStorageDetails) -> Self {
        let AccountStorageDetails { header, map_details } = value;

        Self {
            header: Some(header.into()),
            map_details: map_details.into_iter().map(Into::into).collect(),
        }
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
pub struct AccountDetails {
    pub account_header: AccountHeader,
    pub account_code: Option<Vec<u8>>,
    pub vault_details: AccountVaultDetails,
    pub storage_details: AccountStorageDetails,
}

impl TryFrom<&AccountDetails> for PartialAccount {
    type Error = ConversionError;

    fn try_from(account_details: &AccountDetails) -> Result<Self, Self::Error> {
        // Derive account code.
        let account_code = account_details
            .account_code
            .as_ref()
            .ok_or(ConversionError::AccountCodeMissing)?;
        let account_code = AccountCode::from_bytes(account_code)?;

        // Derive partial storage.
        let account_storage = AccountStorage::new(Vec::new())?; // TODO(currentpr): how to get storage?
        let partial_storage = if account_details.account_header.nonce() == ZERO {
            PartialStorage::new_full(account_storage)
        } else {
            PartialStorage::new_minimal(&account_storage)
        };

        // Derive partial vault.
        let asset_vault = AssetVault::new(&account_details.vault_details.assets)?;
        let partial_vault = PartialVault::new_minimal(&asset_vault);

        // Construct partial account.
        let partial_account = PartialAccount::new(
            account_details.account_header.id(),
            account_details.account_header.nonce(),
            account_code,
            partial_storage,
            partial_vault,
            None, // TODO(currentpr): Seed?
        )?;
        Ok(partial_account)
    }
}

/// Represents the response to an account proof request.
pub struct AccountProofResponse {
    pub block_num: BlockNumber,
    pub witness: AccountWitness,
    pub details: Option<AccountDetails>,
}

impl TryFrom<proto::rpc::AccountProofResponse> for AccountProofResponse {
    type Error = ConversionError;

    fn try_from(value: proto::rpc::AccountProofResponse) -> Result<Self, Self::Error> {
        let proto::rpc::AccountProofResponse { block_num, witness, details } = value;

        let block_num = block_num
            .ok_or(proto::rpc::AccountProofResponse::missing_field(stringify!(block_num)))?
            .into();

        let witness = witness
            .ok_or(proto::rpc::AccountProofResponse::missing_field(stringify!(witness)))?
            .try_into()?;

        let details = details.map(TryFrom::try_from).transpose()?;

        Ok(AccountProofResponse { block_num, witness, details })
    }
}

impl From<AccountProofResponse> for proto::rpc::AccountProofResponse {
    fn from(value: AccountProofResponse) -> Self {
        let AccountProofResponse { block_num, witness, details } = value;

        Self {
            witness: Some(witness.into()),
            details: details.map(Into::into),
            block_num: Some(block_num.into()),
        }
    }
}

impl TryFrom<proto::rpc::account_proof_response::AccountDetails> for AccountDetails {
    type Error = ConversionError;

    fn try_from(
        value: proto::rpc::account_proof_response::AccountDetails,
    ) -> Result<Self, Self::Error> {
        let proto::rpc::account_proof_response::AccountDetails {
            header,
            code,
            vault_details,
            storage_details,
        } = value;

        let account_header = header
            .ok_or(proto::rpc::account_proof_response::AccountDetails::missing_field(stringify!(
                header
            )))?
            .try_into()?;

        let storage_details = storage_details
            .ok_or(proto::rpc::account_proof_response::AccountDetails::missing_field(stringify!(
                storage_details
            )))?
            .try_into()?;

        let vault_details = vault_details
            .ok_or(proto::rpc::account_proof_response::AccountDetails::missing_field(stringify!(
                vault_details
            )))?
            .try_into()?;
        let account_code = code;

        Ok(AccountDetails {
            account_header,
            account_code,
            vault_details,
            storage_details,
        })
    }
}

impl From<AccountDetails> for proto::rpc::account_proof_response::AccountDetails {
    fn from(value: AccountDetails) -> Self {
        let AccountDetails {
            account_header,
            storage_details,
            account_code,
            vault_details,
        } = value;

        let header = Some(proto::account::AccountHeader::from(account_header));
        let storage_details = Some(storage_details.into());
        let code = account_code;
        let vault_details = Some(vault_details.into());

        Self {
            header,
            storage_details,
            code,
            vault_details,
        }
    }
}

impl From<AccountStorageMapDetails>
    for proto::rpc::account_storage_details::AccountStorageMapDetails
{
    fn from(value: AccountStorageMapDetails) -> Self {
        use proto::rpc::account_storage_details::account_storage_map_details;

        let AccountStorageMapDetails { slot_name, too_many_entries, map_entries } = value;

        let entries = Some(account_storage_map_details::MapEntries {
            entries: Vec::from_iter(map_entries.into_iter().map(|(key, value)| {
                account_storage_map_details::map_entries::StorageMapEntry {
                    key: Some(key.into()),
                    value: Some(value.into()),
                }
            })),
        });

        Self {
            slot_name: slot_name.to_string(),
            too_many_entries,
            entries,
        }
    }
}

// ACCOUNT WITNESS
// ================================================================================================

impl TryFrom<proto::account::AccountWitness> for AccountWitness {
    type Error = ConversionError;

    fn try_from(account_witness: proto::account::AccountWitness) -> Result<Self, Self::Error> {
        let witness_id = account_witness
            .witness_id
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(witness_id)))?
            .try_into()?;
        let commitment = account_witness
            .commitment
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(commitment)))?
            .try_into()?;
        let path = account_witness
            .path
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(path)))?
            .try_into()?;

        AccountWitness::new(witness_id, commitment, path).map_err(|err| {
            ConversionError::deserialization_error(
                "AccountWitness",
                DeserializationError::InvalidValue(err.to_string()),
            )
        })
    }
}

impl From<AccountWitness> for proto::account::AccountWitness {
    fn from(witness: AccountWitness) -> Self {
        Self {
            account_id: Some(witness.id().into()),
            witness_id: Some(witness.id().into()),
            commitment: Some(witness.state_commitment().into()),
            path: Some(witness.into_proof().into_parts().0.into()),
        }
    }
}

// ACCOUNT WITNESS RECORD
// ================================================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
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
        let path: SparseMerklePath = account_witness_record
            .path
            .as_ref()
            .ok_or(proto::account::AccountWitness::missing_field(stringify!(path)))?
            .clone()
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
        Self {
            account_id: Some(from.account_id.into()),
            witness_id: Some(from.witness.id().into()),
            commitment: Some(from.witness.state_commitment().into()),
            path: Some(from.witness.path().clone().into()),
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

impl TryFrom<proto::store::transaction_inputs::AccountTransactionInputRecord> for AccountState {
    type Error = ConversionError;

    fn try_from(
        from: proto::store::transaction_inputs::AccountTransactionInputRecord,
    ) -> Result<Self, Self::Error> {
        let account_id = from
            .account_id
            .ok_or(proto::store::transaction_inputs::AccountTransactionInputRecord::missing_field(
                stringify!(account_id),
            ))?
            .try_into()?;

        let account_commitment = from
            .account_commitment
            .ok_or(proto::store::transaction_inputs::AccountTransactionInputRecord::missing_field(
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

impl From<AccountState> for proto::store::transaction_inputs::AccountTransactionInputRecord {
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
