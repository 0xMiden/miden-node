use std::fmt::{Debug, Display, Formatter};

use miden_node_utils::formatting::format_opt;
use miden_protocol::Word;
use miden_protocol::account::{
    Account,
    AccountHeader,
    AccountId,
    AccountStorageHeader,
    StorageMap,
    StorageSlotHeader,
    StorageSlotName,
    StorageSlotType,
};
use miden_protocol::asset::{Asset, AssetVault};
use miden_protocol::block::BlockNumber;
use miden_protocol::block::account_tree::AccountWitness;
use miden_protocol::crypto::merkle::smt::{SmtForest, SmtProof};
use miden_protocol::crypto::merkle::{MerkleError, SparseMerklePath};
use miden_protocol::note::{NoteExecutionMode, NoteTag};
use miden_protocol::utils::{Deserializable, DeserializationError, Serializable};
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
            details: details.as_ref().map(Serializable::to_bytes),
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
        use proto::rpc::account_storage_details::account_storage_map_details::{
            all_map_entries::StorageMapEntry,
            map_entries_with_proofs::StorageMapEntryWithProof,
            AllMapEntries,
            MapEntriesWithProofs,
            Entries as ProtoEntries,
        };

        let proto::rpc::account_storage_details::AccountStorageMapDetails {
            slot_name,
            too_many_entries,
            entries,
        } = value;

        let slot_name = StorageSlotName::new(slot_name)?;

        let entries = if too_many_entries {
            StorageMapEntries::LimitExceeded
        } else {
            match entries {
                None => {
                    return Err(proto::rpc::account_storage_details::AccountStorageMapDetails::missing_field(stringify!(entries)));
                },
                Some(ProtoEntries::AllEntries(AllMapEntries { entries })) => {
                    let entries = entries
                        .into_iter()
                        .map(|entry| {
                            let key = entry
                                .key
                                .ok_or(StorageMapEntry::missing_field(stringify!(key)))?
                                .try_into()?;
                            let value = entry
                                .value
                                .ok_or(StorageMapEntry::missing_field(stringify!(value)))?
                                .try_into()?;
                            Ok((key, value))
                        })
                        .collect::<Result<Vec<_>, ConversionError>>()?;
                    StorageMapEntries::AllEntries(entries)
                },
                Some(ProtoEntries::EntriesWithProofs(MapEntriesWithProofs { entries })) => {
                    let proofs = entries
                        .into_iter()
                        .map(|entry| {
                            let _key: Word = entry
                                .key
                                .ok_or(StorageMapEntryWithProof::missing_field(stringify!(key)))?
                                .try_into()?;
                            let _value: Word = entry
                                .value
                                .ok_or(StorageMapEntryWithProof::missing_field(stringify!(value)))?
                                .try_into()?;
                            let smt_opening = entry.proof.ok_or(
                                StorageMapEntryWithProof::missing_field(stringify!(proof)),
                            )?;
                            let smt_proof = SmtProof::try_from(smt_opening)?;
                            Ok(smt_proof)
                        })
                        .collect::<Result<Vec<_>, ConversionError>>()?;
                    StorageMapEntries::EntriesWithProofs(proofs)
                },
            }
        };

        Ok(Self { slot_name, entries })
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

/// Request of slot data values.
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

/// Account vault details
///
/// When an account contains a large number of assets (>
/// [`AccountVaultDetails::MAX_RETURN_ENTRIES`]), including all assets in a single RPC response
/// creates performance issues. In such cases, the `LimitExceeded` variant indicates to the client
/// to use the `SyncAccountVault` endpoint instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountVaultDetails {
    /// The vault has too many assets to return inline.
    /// Clients must use `SyncAccountVault` endpoint instead.
    LimitExceeded,

    /// The assets in the vault (up to `MAX_RETURN_ENTRIES`).
    Assets(Vec<Asset>),
}

impl AccountVaultDetails {
    /// Maximum number of vault entries that can be returned in a single response.
    /// Accounts with more assets will have `LimitExceeded` variant.
    pub const MAX_RETURN_ENTRIES: usize = 1000;

    pub fn new(vault: &AssetVault) -> Self {
        if vault.assets().nth(Self::MAX_RETURN_ENTRIES).is_some() {
            Self::LimitExceeded
        } else {
            Self::Assets(Vec::from_iter(vault.assets()))
        }
    }

    pub fn empty() -> Self {
        Self::Assets(Vec::new())
    }

    /// Creates `AccountVaultDetails` from a list of assets.
    pub fn from_assets(assets: Vec<Asset>) -> Self {
        if assets.len() > Self::MAX_RETURN_ENTRIES {
            Self::LimitExceeded
        } else {
            Self::Assets(assets)
        }
    }
}

impl TryFrom<proto::rpc::AccountVaultDetails> for AccountVaultDetails {
    type Error = ConversionError;

    fn try_from(value: proto::rpc::AccountVaultDetails) -> Result<Self, Self::Error> {
        let proto::rpc::AccountVaultDetails { too_many_assets, assets } = value;

        if too_many_assets {
            Ok(Self::LimitExceeded)
        } else {
            let parsed_assets =
                Result::<Vec<_>, ConversionError>::from_iter(assets.into_iter().map(|asset| {
                    let asset = asset
                        .asset
                        .ok_or(proto::primitives::Asset::missing_field(stringify!(asset)))?;
                    let asset = Word::try_from(asset)?;
                    Asset::try_from(asset).map_err(ConversionError::AssetError)
                }))?;
            Ok(Self::Assets(parsed_assets))
        }
    }
}

impl From<AccountVaultDetails> for proto::rpc::AccountVaultDetails {
    fn from(value: AccountVaultDetails) -> Self {
        match value {
            AccountVaultDetails::LimitExceeded => Self {
                too_many_assets: true,
                assets: Vec::new(),
            },
            AccountVaultDetails::Assets(assets) => Self {
                too_many_assets: false,
                assets: Vec::from_iter(assets.into_iter().map(|asset| proto::primitives::Asset {
                    asset: Some(proto::primitives::Digest::from(Word::from(asset))),
                })),
            },
        }
    }
}

/// Storage map entries for an account storage slot.
///
/// When a storage map contains many entries (> [`AccountStorageMapDetails::MAX_RETURN_ENTRIES`]),
/// returning all entries in a single RPC response creates performance issues. In such cases,
/// the `LimitExceeded` variant indicates to the client to use the `SyncStorageMaps` endpoint
/// instead.
#[derive(Debug, Clone, PartialEq)]
pub enum StorageMapEntries {
    /// The map has too many entries to return inline.
    /// Clients must use `SyncStorageMaps` endpoint instead.
    LimitExceeded,

    /// All storage map entries (key-value pairs) without proofs.
    /// Used when all entries are requested for small maps.
    AllEntries(Vec<(Word, Word)>),

    /// Specific entries with their SMT proofs for client-side verification.
    /// Used when specific keys are requested from the storage map.
    EntriesWithProofs(Vec<SmtProof>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountStorageMapDetails {
    pub slot_name: StorageSlotName,
    pub entries: StorageMapEntries,
}

impl AccountStorageMapDetails {
    /// Maximum number of storage map entries that can be returned in a single response.
    pub const MAX_RETURN_ENTRIES: usize = 1000;

    /// Creates storage map details with all entries from the storage map.
    ///
    /// If the storage map has too many entries (> `MAX_RETURN_ENTRIES`),
    /// returns `LimitExceeded` variant.
    pub fn from_all_entries(slot_name: StorageSlotName, storage_map: &StorageMap) -> Self {
        if storage_map.num_entries() > Self::MAX_RETURN_ENTRIES {
            Self {
                slot_name,
                entries: StorageMapEntries::LimitExceeded,
            }
        } else {
            let entries = Vec::from_iter(storage_map.entries().map(|(k, v)| (*k, *v)));
            Self {
                slot_name,
                entries: StorageMapEntries::AllEntries(entries),
            }
        }
    }

    /// Creates storage map details based on the requested slot data.
    ///
    /// Handles both "all entries" and "specific keys" requests.
    /// Returns `LimitExceeded` if too many entries.
    pub fn new(slot_name: StorageSlotName, slot_data: SlotData, storage_map: &StorageMap) -> Self {
        match slot_data {
            SlotData::All => Self::from_all_entries(slot_name, storage_map),
            SlotData::MapKeys(keys) => {
                if keys.len() > Self::MAX_RETURN_ENTRIES {
                    Self {
                        slot_name,
                        entries: StorageMapEntries::LimitExceeded,
                    }
                } else {
                    // Query specific keys from the storage map - returns all entries without proofs
                    // For proofs, use from_specific_keys with SmtForest
                    let mut entries = Vec::with_capacity(keys.len());
                    for key in keys {
                        let value = storage_map.get(&key);
                        entries.push((key, value));
                    }
                    Self {
                        slot_name,
                        entries: StorageMapEntries::AllEntries(entries),
                    }
                }
            },
        }
    }

    /// Creates storage map details from forest-queried entries.
    ///
    /// Returns `LimitExceeded` if too many entries.
    pub fn from_forest_entries(slot_name: StorageSlotName, entries: Vec<(Word, Word)>) -> Self {
        if entries.len() > Self::MAX_RETURN_ENTRIES {
            Self {
                slot_name,
                entries: StorageMapEntries::LimitExceeded,
            }
        } else {
            Self {
                slot_name,
                entries: StorageMapEntries::AllEntries(entries),
            }
        }
    }

    /// Creates storage map details with SMT proofs for specific keys.
    ///
    /// This method queries the forest for specific keys and returns proofs that
    /// enable client-side verification of the values.
    ///
    /// Returns `LimitExceeded` if too many keys, or `MerkleError` if the forest
    /// doesn't contain sufficient data.
    pub fn from_specific_keys(
        slot_name: StorageSlotName,
        keys: &[Word],
        storage_forest: &SmtForest,
        smt_root: Word,
    ) -> Result<Self, MerkleError> {
        if keys.len() > Self::MAX_RETURN_ENTRIES {
            return Ok(Self {
                slot_name,
                entries: StorageMapEntries::LimitExceeded,
            });
        }

        // Collect SMT proofs for each key
        let mut proofs = Vec::with_capacity(keys.len());

        for key in keys {
            let proof = storage_forest.open(smt_root, *key)?;
            proofs.push(proof);
        }

        Ok(Self {
            slot_name,
            entries: StorageMapEntries::EntriesWithProofs(proofs),
        })
    }

    /// Creates storage map details from pre-computed SMT proofs.
    ///
    /// Use this when the caller has already obtained the proofs from an `SmtForest`.
    /// Returns `LimitExceeded` if too many proofs are provided.
    pub fn from_proofs(slot_name: StorageSlotName, proofs: Vec<SmtProof>) -> Self {
        if proofs.len() > Self::MAX_RETURN_ENTRIES {
            Self {
                slot_name,
                entries: StorageMapEntries::LimitExceeded,
            }
        } else {
            Self {
                slot_name,
                entries: StorageMapEntries::EntriesWithProofs(proofs),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
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
        0 => StorageSlotType::Value,
        1 => StorageSlotType::Map,
        _ => return Err(ConversionError::EnumDiscriminantOutOfRange),
    })
}

const fn storage_slot_type_to_raw(slot_type: StorageSlotType) -> u32 {
    match slot_type {
        StorageSlotType::Value => 0,
        StorageSlotType::Map => 1,
    }
}

/// Represents account details returned in response to an account proof request.
pub struct AccountDetails {
    pub account_header: AccountHeader,
    pub account_code: Option<Vec<u8>>,
    pub vault_details: AccountVaultDetails,
    pub storage_details: AccountStorageDetails,
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
        use proto::rpc::account_storage_details::account_storage_map_details::{
            AllMapEntries,
            Entries as ProtoEntries,
            MapEntriesWithProofs,
        };

        let AccountStorageMapDetails { slot_name, entries } = value;

        let (too_many_entries, proto_entries) = match entries {
            StorageMapEntries::LimitExceeded => (true, None),
            StorageMapEntries::AllEntries(entries) => {
                let all = AllMapEntries {
                    entries: Vec::from_iter(entries.into_iter().map(|(key, value)| {
                        proto::rpc::account_storage_details::account_storage_map_details::all_map_entries::StorageMapEntry {
                            key: Some(key.into()),
                            value: Some(value.into()),
                        }
                    })),
                };
                (false, Some(ProtoEntries::AllEntries(all)))
            },
            StorageMapEntries::EntriesWithProofs(proofs) => {
                use miden_protocol::crypto::merkle::smt::SmtLeaf;

                let with_proofs = MapEntriesWithProofs {
                    entries: Vec::from_iter(proofs.into_iter().map(|proof| {
                        // Get key/value from the leaf before consuming the proof
                        let (key, value) = match proof.leaf() {
                            SmtLeaf::Empty(_) => {
                                (miden_protocol::EMPTY_WORD, miden_protocol::EMPTY_WORD)
                            },
                            SmtLeaf::Single((k, v)) => (*k, *v),
                            SmtLeaf::Multiple(entries) => entries.iter().next().map_or(
                                (miden_protocol::EMPTY_WORD, miden_protocol::EMPTY_WORD),
                                |(k, v)| (*k, *v),
                            ),
                        };
                        let smt_opening = proto::primitives::SmtOpening::from(proof);
                        proto::rpc::account_storage_details::account_storage_map_details::map_entries_with_proofs::StorageMapEntryWithProof {
                            key: Some(key.into()),
                            value: Some(value.into()),
                            proof: Some(smt_opening),
                        }
                    })),
                };
                (false, Some(ProtoEntries::EntriesWithProofs(with_proofs)))
            },
        };

        Self {
            slot_name: slot_name.to_string(),
            too_many_entries,
            entries: proto_entries,
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

#[cfg(test)]
mod tests {
    use miden_protocol::crypto::merkle::EmptySubtreeRoots;
    use miden_protocol::crypto::merkle::smt::SMT_DEPTH;

    use super::*;

    fn word_from_u32(arr: [u32; 4]) -> Word {
        Word::from(arr)
    }

    fn test_slot_name() -> StorageSlotName {
        StorageSlotName::new("miden::test::storage::slot").unwrap()
    }

    fn empty_smt_root() -> Word {
        *EmptySubtreeRoots::entry(SMT_DEPTH, 0)
    }

    #[test]
    fn account_storage_map_details_from_forest_entries() {
        let slot_name = test_slot_name();
        let entries = vec![
            (word_from_u32([1, 2, 3, 4]), word_from_u32([5, 6, 7, 8])),
            (word_from_u32([9, 10, 11, 12]), word_from_u32([13, 14, 15, 16])),
        ];

        let details =
            AccountStorageMapDetails::from_forest_entries(slot_name.clone(), entries.clone());

        assert_eq!(details.slot_name, slot_name);
        assert_eq!(details.entries, StorageMapEntries::AllEntries(entries));
    }

    #[test]
    fn account_storage_map_details_from_forest_entries_limit_exceeded() {
        let slot_name = test_slot_name();
        // Create more entries than MAX_RETURN_ENTRIES
        let entries: Vec<_> = (0..=AccountStorageMapDetails::MAX_RETURN_ENTRIES)
            .map(|i| {
                let key = word_from_u32([i as u32, 0, 0, 0]);
                let value = word_from_u32([0, 0, 0, i as u32]);
                (key, value)
            })
            .collect();

        let details = AccountStorageMapDetails::from_forest_entries(slot_name.clone(), entries);

        assert_eq!(details.slot_name, slot_name);
        assert_eq!(details.entries, StorageMapEntries::LimitExceeded);
    }

    #[test]
    fn account_storage_map_details_from_specific_keys() {
        let slot_name = test_slot_name();

        // Create an SmtForest and populate it with some data
        let mut forest = SmtForest::new();
        let entries = [
            (word_from_u32([1, 0, 0, 0]), word_from_u32([10, 0, 0, 0])),
            (word_from_u32([2, 0, 0, 0]), word_from_u32([20, 0, 0, 0])),
            (word_from_u32([3, 0, 0, 0]), word_from_u32([30, 0, 0, 0])),
        ];

        // Insert entries into the forest starting from an empty root
        let smt_root = forest.batch_insert(empty_smt_root(), entries.iter().copied()).unwrap();

        // Query specific keys
        let keys = vec![word_from_u32([1, 0, 0, 0]), word_from_u32([3, 0, 0, 0])];

        let details = AccountStorageMapDetails::from_specific_keys(
            slot_name.clone(),
            &keys,
            &forest,
            smt_root,
        )
        .unwrap();

        assert_eq!(details.slot_name, slot_name);
        match details.entries {
            StorageMapEntries::EntriesWithProofs(proofs) => {
                use miden_protocol::crypto::merkle::smt::SmtLeaf;

                assert_eq!(proofs.len(), 2);

                // Helper to extract key-value from any leaf type
                let get_value = |proof: &SmtProof, expected_key: Word| -> Word {
                    match proof.leaf() {
                        SmtLeaf::Single((k, v)) if *k == expected_key => *v,
                        SmtLeaf::Multiple(entries) => entries
                            .iter()
                            .find(|(k, _)| *k == expected_key)
                            .map_or(miden_protocol::EMPTY_WORD, |(_, v)| *v),
                        _ => miden_protocol::EMPTY_WORD,
                    }
                };

                let first_key = word_from_u32([1, 0, 0, 0]);
                let second_key = word_from_u32([3, 0, 0, 0]);
                let first_value = get_value(&proofs[0], first_key);
                let second_value = get_value(&proofs[1], second_key);

                assert_eq!(first_value, word_from_u32([10, 0, 0, 0]));
                assert_eq!(second_value, word_from_u32([30, 0, 0, 0]));
            },
            StorageMapEntries::LimitExceeded | StorageMapEntries::AllEntries(_) => {
                panic!("Expected EntriesWithProofs")
            },
        }
    }

    #[test]
    fn account_storage_map_details_from_specific_keys_nonexistent_returns_proof() {
        let slot_name = test_slot_name();

        // Create an SmtForest with one entry so the root is tracked
        let mut forest = SmtForest::new();
        let entries = [(word_from_u32([1, 0, 0, 0]), word_from_u32([10, 0, 0, 0]))];
        let smt_root = forest.batch_insert(empty_smt_root(), entries.iter().copied()).unwrap();

        // Query a key that doesn't exist in the tree - should return a proof
        // (the proof will show non-membership or point to an adjacent leaf)
        let keys = vec![word_from_u32([99, 0, 0, 0])];

        let details = AccountStorageMapDetails::from_specific_keys(
            slot_name.clone(),
            &keys,
            &forest,
            smt_root,
        )
        .unwrap();

        match details.entries {
            StorageMapEntries::EntriesWithProofs(proofs) => {
                // We got a proof for the non-existent key
                assert_eq!(proofs.len(), 1);
                // The proof exists and can be used to verify non-membership
            },
            StorageMapEntries::LimitExceeded | StorageMapEntries::AllEntries(_) => {
                panic!("Expected EntriesWithProofs")
            },
        }
    }

    #[test]
    fn account_storage_map_details_from_specific_keys_limit_exceeded() {
        let slot_name = test_slot_name();
        let mut forest = SmtForest::new();

        // Create a forest with some data to get a valid root
        let entries = [(word_from_u32([1, 0, 0, 0]), word_from_u32([10, 0, 0, 0]))];
        let smt_root = forest.batch_insert(empty_smt_root(), entries.iter().copied()).unwrap();

        // Create more keys than MAX_RETURN_ENTRIES
        let keys: Vec<_> = (0..=AccountStorageMapDetails::MAX_RETURN_ENTRIES)
            .map(|i| word_from_u32([i as u32, 0, 0, 0]))
            .collect();

        let details = AccountStorageMapDetails::from_specific_keys(
            slot_name.clone(),
            &keys,
            &forest,
            smt_root,
        )
        .unwrap();

        assert_eq!(details.entries, StorageMapEntries::LimitExceeded);
    }
}
