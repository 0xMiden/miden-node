use std::collections::BTreeMap;
use std::sync::Arc;

use miden_protocol::account::{
    AccountCode,
    AccountPatch,
    AccountProcedureRoot,
    AccountStoragePatch,
    AccountUpdateDetails,
    AccountVaultPatch,
    StorageMapKey,
    StorageMapPatch,
    StorageMapPatchEntries,
    StoragePatchOperation,
    StorageSlotName,
    StorageSlotPatch,
    StorageValuePatch,
};
use miden_protocol::asset::AssetId;
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{MastForest, Word};

use crate::decode::{ConversionResultExt, DecodeBytesExt, GrpcDecodeExt};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

// ACCOUNT CODE
// ================================================================================================

impl From<&AccountCode> for proto::account::AccountCode {
    fn from(code: &AccountCode) -> Self {
        Self {
            mast: code.mast().to_bytes(),
            procedure_roots: code.procedure_roots().map(Into::into).collect(),
        }
    }
}

impl From<AccountCode> for proto::account::AccountCode {
    fn from(code: AccountCode) -> Self {
        Self::from(&code)
    }
}

impl TryFrom<proto::account::AccountCode> for AccountCode {
    type Error = ConversionError;

    fn try_from(code: proto::account::AccountCode) -> Result<Self, Self::Error> {
        let mast = MastForest::decode_bytes(&code.mast, "MastForest").context("mast")?;
        let procedure_roots = code
            .procedure_roots
            .into_iter()
            .enumerate()
            .map(|(index, root)| {
                Word::try_from(root)
                    .map(AccountProcedureRoot::from_raw)
                    .context(format!("procedure_roots[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        AccountCode::from_parts(Arc::new(mast), procedure_roots).map_err(ConversionError::new)
    }
}

// STORAGE PATCHES
// ================================================================================================

const fn encode_storage_operation(operation: StoragePatchOperation) -> i32 {
    match operation {
        StoragePatchOperation::Create => proto::account::StoragePatchOperation::Create as i32,
        StoragePatchOperation::Update => proto::account::StoragePatchOperation::Update as i32,
        StoragePatchOperation::Remove => proto::account::StoragePatchOperation::Remove as i32,
    }
}

fn decode_storage_operation(operation: i32) -> Result<StoragePatchOperation, ConversionError> {
    match proto::account::StoragePatchOperation::try_from(operation) {
        Ok(proto::account::StoragePatchOperation::Create) => Ok(StoragePatchOperation::Create),
        Ok(proto::account::StoragePatchOperation::Update) => Ok(StoragePatchOperation::Update),
        Ok(proto::account::StoragePatchOperation::Remove) => Ok(StoragePatchOperation::Remove),
        Ok(proto::account::StoragePatchOperation::Unspecified) => {
            Err(ConversionError::message("storage patch operation is unspecified"))
        },
        Err(_) => {
            Err(ConversionError::message(format!("unknown storage patch operation {operation}")))
        },
    }
}

impl From<&StorageValuePatch> for proto::account::StorageValuePatch {
    fn from(patch: &StorageValuePatch) -> Self {
        Self {
            operation: encode_storage_operation(patch.patch_op()),
            value: patch.value().map(Into::into),
        }
    }
}

impl TryFrom<proto::account::StorageValuePatch> for StorageValuePatch {
    type Error = ConversionError;

    fn try_from(patch: proto::account::StorageValuePatch) -> Result<Self, Self::Error> {
        let operation = decode_storage_operation(patch.operation).context("operation")?;
        match operation {
            StoragePatchOperation::Create | StoragePatchOperation::Update => {
                let decoder = patch.decoder();
                let value = decode!(decoder, patch.value)?;
                Ok(if operation.is_create() {
                    StorageValuePatch::Create { value }
                } else {
                    StorageValuePatch::Update { value }
                })
            },
            StoragePatchOperation::Remove => {
                if patch.value.is_some() {
                    return Err(ConversionError::message(
                        "value must be absent for a remove operation",
                    )
                    .context("value"));
                }
                Ok(StorageValuePatch::Remove)
            },
        }
    }
}

impl From<&StorageMapPatch> for proto::account::StorageMapPatch {
    fn from(patch: &StorageMapPatch) -> Self {
        let entries = patch
            .entries()
            .into_iter()
            .flat_map(StorageMapPatchEntries::as_map)
            .map(|(key, value)| proto::account::StorageMapEntry {
                key: Some(Word::from(*key).into()),
                value: Some((*value).into()),
            })
            .collect();

        Self {
            operation: encode_storage_operation(patch.patch_op()),
            entries,
        }
    }
}

impl TryFrom<proto::account::StorageMapPatch> for StorageMapPatch {
    type Error = ConversionError;

    fn try_from(patch: proto::account::StorageMapPatch) -> Result<Self, Self::Error> {
        let operation = decode_storage_operation(patch.operation).context("operation")?;
        if operation.is_remove() {
            if !patch.entries.is_empty() {
                return Err(ConversionError::message(
                    "entries must be empty for a remove operation",
                )
                .context("entries"));
            }
            return Ok(StorageMapPatch::Remove);
        }

        let mut entries = BTreeMap::new();
        for (index, entry) in patch.entries.into_iter().enumerate() {
            let decoder = entry.decoder();
            let key: Word = decode!(decoder, entry.key).context(format!("entries[{index}]"))?;
            let value = decode!(decoder, entry.value).context(format!("entries[{index}]"))?;
            let key = StorageMapKey::from_raw(key);
            if entries.insert(key, value).is_some() {
                return Err(ConversionError::message("duplicate storage map key")
                    .context(format!("entries[{index}].key")));
            }
        }

        let entries = StorageMapPatchEntries::from_raw(entries);
        match operation {
            StoragePatchOperation::Create => Ok(StorageMapPatch::Create { entries }),
            StoragePatchOperation::Update if entries.is_empty() => {
                Err(ConversionError::message("entries must be non-empty for an update operation")
                    .context("entries"))
            },
            StoragePatchOperation::Update => Ok(StorageMapPatch::Update { entries }),
            StoragePatchOperation::Remove => unreachable!("remove handled above"),
        }
    }
}

enum StorageSlotPatchRef<'a> {
    Value(&'a StorageValuePatch),
    Map(&'a StorageMapPatch),
}

impl From<(&StorageSlotName, StorageSlotPatchRef<'_>)> for proto::account::StorageSlotPatch {
    fn from((slot_name, patch): (&StorageSlotName, StorageSlotPatchRef<'_>)) -> Self {
        use proto::account::storage_slot_patch::Patch;

        let patch = match patch {
            StorageSlotPatchRef::Value(value) => Patch::Value(value.into()),
            StorageSlotPatchRef::Map(map) => Patch::Map(map.into()),
        };
        Self {
            slot_name: slot_name.as_str().to_owned(),
            patch: Some(patch),
        }
    }
}

impl From<&AccountStoragePatch> for proto::account::AccountStoragePatch {
    fn from(patch: &AccountStoragePatch) -> Self {
        let mut slots = patch
            .values()
            .map(|(name, patch)| (name, StorageSlotPatchRef::Value(patch)))
            .chain(patch.maps().map(|(name, patch)| (name, StorageSlotPatchRef::Map(patch))))
            .collect::<Vec<_>>();
        slots.sort_by_key(|(name, _)| *name);

        Self {
            slots: slots.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<proto::account::AccountStoragePatch> for AccountStoragePatch {
    type Error = ConversionError;

    fn try_from(patch: proto::account::AccountStoragePatch) -> Result<Self, Self::Error> {
        use proto::account::storage_slot_patch::Patch;

        let slots = patch
            .slots
            .into_iter()
            .enumerate()
            .map(|(index, slot)| {
                let slot_path = format!("slots[{index}]");
                let slot_name = StorageSlotName::new(slot.slot_name)
                    .map_err(ConversionError::from)
                    .context("slot_name")
                    .context(slot_path.clone())?;
                let patch = match slot.patch {
                    Some(Patch::Value(value)) => StorageSlotPatch::Value(
                        value.try_into().context("patch").context(slot_path.clone())?,
                    ),
                    Some(Patch::Map(map)) => StorageSlotPatch::Map(
                        map.try_into().context("patch").context(slot_path.clone())?,
                    ),
                    None => {
                        return Err(ConversionError::missing_field::<
                            proto::account::StorageSlotPatch,
                        >("patch")
                        .context(slot_path));
                    },
                };
                Ok((slot_name, patch))
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;

        AccountStoragePatch::from_entries(slots)
            .map_err(ConversionError::new)
            .context("slots")
    }
}

// VAULT AND ACCOUNT PATCHES
// ================================================================================================

impl From<&AccountVaultPatch> for proto::account::AccountVaultPatch {
    fn from(patch: &AccountVaultPatch) -> Self {
        Self {
            entries: patch
                .iter()
                .map(|(asset_id, value)| proto::account::AccountVaultPatchEntry {
                    asset_id: Some(asset_id.to_word().into()),
                    value: Some((*value).into()),
                })
                .collect(),
        }
    }
}

impl TryFrom<proto::account::AccountVaultPatch> for AccountVaultPatch {
    type Error = ConversionError;

    fn try_from(patch: proto::account::AccountVaultPatch) -> Result<Self, Self::Error> {
        let mut entries = BTreeMap::new();
        for (index, entry) in patch.entries.into_iter().enumerate() {
            let decoder = entry.decoder();
            let asset_id: Word =
                decode!(decoder, entry.asset_id).context(format!("entries[{index}]"))?;
            let asset_id = AssetId::try_from(asset_id)
                .map_err(ConversionError::from)
                .context("asset_id")
                .context(format!("entries[{index}]"))?;
            let value = decode!(decoder, entry.value).context(format!("entries[{index}]"))?;
            if entries.insert(asset_id, value).is_some() {
                return Err(ConversionError::message("duplicate vault asset ID")
                    .context(format!("entries[{index}].asset_id")));
            }
        }

        AccountVaultPatch::new(entries)
            .map_err(ConversionError::from)
            .context("entries")
    }
}

impl From<&AccountPatch> for proto::account::AccountPatch {
    fn from(patch: &AccountPatch) -> Self {
        Self {
            account_id: Some(patch.id().into()),
            storage: Some(patch.storage().into()),
            vault: Some(patch.vault().into()),
            code: patch.code().map(Into::into),
            final_nonce: patch.final_nonce().map(Into::into),
        }
    }
}

impl From<AccountPatch> for proto::account::AccountPatch {
    fn from(patch: AccountPatch) -> Self {
        Self::from(&patch)
    }
}

impl TryFrom<proto::account::AccountPatch> for AccountPatch {
    type Error = ConversionError;

    fn try_from(patch: proto::account::AccountPatch) -> Result<Self, Self::Error> {
        let decoder = patch.decoder();
        let account_id = decode!(decoder, patch.account_id)?;
        let storage = decode!(decoder, patch.storage)?;
        let vault = decode!(decoder, patch.vault)?;
        let code = patch.code.map(TryInto::try_into).transpose().context("code")?;
        let final_nonce =
            patch.final_nonce.map(TryInto::try_into).transpose().context("final_nonce")?;

        AccountPatch::new(account_id, storage, vault, code, final_nonce)
            .map_err(ConversionError::new)
    }
}

impl From<&AccountUpdateDetails> for proto::account::AccountUpdateDetails {
    fn from(details: &AccountUpdateDetails) -> Self {
        use proto::account::account_update_details::Update;

        let update = match details {
            AccountUpdateDetails::Private => {
                Update::Private(proto::account::PrivateAccountUpdate {})
            },
            AccountUpdateDetails::Public(patch) => Update::Public(patch.into()),
        };
        Self { update: Some(update) }
    }
}

impl From<AccountUpdateDetails> for proto::account::AccountUpdateDetails {
    fn from(details: AccountUpdateDetails) -> Self {
        Self::from(&details)
    }
}

impl TryFrom<proto::account::AccountUpdateDetails> for AccountUpdateDetails {
    type Error = ConversionError;

    fn try_from(details: proto::account::AccountUpdateDetails) -> Result<Self, Self::Error> {
        use proto::account::account_update_details::Update;

        match details.update {
            Some(Update::Private(_)) => Ok(AccountUpdateDetails::Private),
            Some(Update::Public(patch)) => {
                patch.try_into().map(AccountUpdateDetails::Public).context("public")
            },
            None => Err(ConversionError::missing_field::<proto::account::AccountUpdateDetails>(
                "update",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::Word;
    use miden_protocol::account::{
        AccountCode,
        AccountId,
        AccountIdVersion,
        AccountPatch,
        AccountType,
        AccountUpdateDetails,
        AssetCallbackFlag,
        StorageMapPatch,
        StorageValuePatch,
    };

    use crate::generated as proto;

    fn public_account_id() -> AccountId {
        AccountId::dummy(
            [3; 15],
            AccountIdVersion::Version1,
            AccountType::Public,
            AssetCallbackFlag::Disabled,
        )
    }

    #[test]
    fn account_code_roundtrips_with_structured_procedure_roots() {
        let code = AccountCode::mock();
        let encoded = proto::account::AccountCode::from(&code);

        assert_eq!(encoded.procedure_roots.len(), code.procedure_roots().count());
        assert_eq!(AccountCode::try_from(encoded).unwrap(), code);
    }

    #[test]
    fn empty_public_patch_and_private_update_roundtrip() {
        let public = AccountUpdateDetails::Public(AccountPatch::empty(public_account_id()));
        assert_eq!(
            AccountUpdateDetails::try_from(proto::account::AccountUpdateDetails::from(&public))
                .unwrap(),
            public
        );

        let private = AccountUpdateDetails::Private;
        assert_eq!(
            AccountUpdateDetails::try_from(proto::account::AccountUpdateDetails::from(&private))
                .unwrap(),
            private
        );
    }

    #[test]
    fn storage_patch_operation_presence_rules_are_enforced() {
        let missing_value = proto::account::StorageValuePatch {
            operation: proto::account::StoragePatchOperation::Create as i32,
            value: None,
        };
        assert!(
            StorageValuePatch::try_from(missing_value)
                .unwrap_err()
                .to_string()
                .contains("value")
        );

        let remove_with_value = proto::account::StorageValuePatch {
            operation: proto::account::StoragePatchOperation::Remove as i32,
            value: Some(Word::default().into()),
        };
        assert!(
            StorageValuePatch::try_from(remove_with_value)
                .unwrap_err()
                .to_string()
                .contains("value")
        );

        let empty_update = proto::account::StorageMapPatch {
            operation: proto::account::StoragePatchOperation::Update as i32,
            entries: Vec::new(),
        };
        assert!(
            StorageMapPatch::try_from(empty_update)
                .unwrap_err()
                .to_string()
                .contains("entries")
        );

        let empty_create = proto::account::StorageMapPatch {
            operation: proto::account::StoragePatchOperation::Create as i32,
            entries: Vec::new(),
        };
        assert!(matches!(
            StorageMapPatch::try_from(empty_create).unwrap(),
            StorageMapPatch::Create { .. }
        ));
    }

    #[test]
    fn storage_map_patch_rejects_duplicate_keys_with_index_context() {
        let entry = proto::account::StorageMapEntry {
            key: Some(Word::from([1_u32, 2, 3, 4]).into()),
            value: Some(Word::from([5_u32, 6, 7, 8]).into()),
        };
        let patch = proto::account::StorageMapPatch {
            operation: proto::account::StoragePatchOperation::Update as i32,
            entries: vec![entry.clone(), entry],
        };

        let error = StorageMapPatch::try_from(patch).unwrap_err().to_string();
        assert!(error.contains("entries[1].key"));
    }

    #[test]
    fn account_update_details_requires_a_variant() {
        let error = AccountUpdateDetails::try_from(proto::account::AccountUpdateDetails::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("update"));
    }
}
