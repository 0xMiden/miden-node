use miden_lib::transaction::TransactionKernel;
use miden_objects::{
    account::{delta::AccountUpdateDetails, Account},
    block::{BlockAccountUpdate, BlockHeader, BlockNoteTree, BlockNumber, ProvenBlock},
    crypto::merkle::{MmrPeaks, SimpleSmt, Smt},
    note::Nullifier,
    utils::serde::{ByteReader, ByteWriter, Deserializable, DeserializationError, Serializable},
    Digest, ACCOUNT_TREE_DEPTH,
};

use crate::errors::GenesisError;

// GENESIS STATE
// ================================================================================================

/// Represents the state at genesis, which will be used to derive the genesis block.
#[derive(Debug, PartialEq, Eq)]
pub struct GenesisState {
    pub accounts: Vec<Account>,
    pub version: u32,
    pub timestamp: u32,
}

impl GenesisState {
    pub fn new(accounts: Vec<Account>, version: u32, timestamp: u32) -> Self {
        Self { accounts, version, timestamp }
    }

    /// Returns the block header and the account SMT
    pub fn into_block(self) -> Result<ProvenBlock, GenesisError> {
        let accounts: Vec<BlockAccountUpdate> = self
            .accounts
            .iter()
            .map(|account| {
                let account_update_details = if account.id().is_public() {
                    AccountUpdateDetails::New(account.clone())
                } else {
                    AccountUpdateDetails::Private
                };

                BlockAccountUpdate::new(
                    account.id(),
                    account.hash(),
                    account_update_details,
                    vec![],
                )
            })
            .collect();

        let account_smt: SimpleSmt<ACCOUNT_TREE_DEPTH> =
            SimpleSmt::with_leaves(accounts.iter().map(|update| {
                (update.account_id().prefix().into(), update.final_state_commitment().into())
            }))?;

        let empty_nullifiers: Vec<Nullifier> = Vec::new();
        let empty_nullifier_tree = Smt::new();

        let empty_output_notes = Vec::new();
        let empty_block_note_tree = BlockNoteTree::empty();

        let header = BlockHeader::new(
            self.version,
            Digest::default(),
            BlockNumber::GENESIS,
            MmrPeaks::new(0, Vec::new()).unwrap().hash_peaks(),
            account_smt.root(),
            empty_nullifier_tree.root(),
            empty_block_note_tree.root(),
            Digest::default(),
            TransactionKernel::kernel_root(),
            Digest::default(),
            self.timestamp,
        );

        // SAFETY: Header and accounts should be valid by construction.
        // No notes or nullifiers are created at genesis, which is consistent with the above empty
        // block note tree root and empty nullifier tree root.
        Ok(ProvenBlock::new_unchecked(
            header,
            accounts,
            empty_output_notes,
            empty_nullifiers,
        ))
    }
}

// SERIALIZATION
// ================================================================================================

impl Serializable for GenesisState {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        assert!(u64::try_from(self.accounts.len()).is_ok(), "too many accounts in GenesisState");
        target.write_usize(self.accounts.len());
        target.write_many(&self.accounts);

        target.write_u32(self.version);
        target.write_u32(self.timestamp);
    }
}

impl Deserializable for GenesisState {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let num_accounts = source.read_usize()?;
        let accounts = source.read_many::<Account>(num_accounts)?;

        let version = source.read_u32()?;
        let timestamp = source.read_u32()?;

        Ok(Self::new(accounts, version, timestamp))
    }
}
