use miden_objects::{account::{Account, AccountId}, note::{NoteExecutionMode, NoteTag}};
use thiserror::Error;


pub type AccountPrefix = u32;

/// Wrapper for network account prefix
/// Provides type safety for accounts that are meant for network execution
pub struct NetworkAccountPrefix(u32);

impl NetworkAccountPrefix {
    pub fn inner(&self) -> u32 {
        return self.0
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
        return Ok(NetworkAccountPrefix(value));
    }
}

impl TryFrom<AccountId> for NetworkAccountPrefix {
    type Error = NetworkAccountError;

    fn try_from(id: AccountId) -> Result<Self, Self::Error> {
        if !id.is_network() {
            Err(NetworkAccountError::NotNetworkAccount(id))
        } else {
            let prefix = get_account_id_tag_prefix(id);
            Ok(NetworkAccountPrefix(prefix))
        }
    }
}

impl TryFrom<NoteTag> for NetworkAccountPrefix {
    type Error = NetworkAccountError;

    fn try_from(tag: NoteTag) -> Result<Self, Self::Error> {
        if tag.execution_mode() != NoteExecutionMode::Network || !tag.is_single_target(){
            return Err(NetworkAccountError::InvalidExecutionMode(tag));
        }

        let tag_inner:u32 = tag.into();
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
