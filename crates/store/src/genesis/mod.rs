use miden_lib::transaction::TransactionKernel;
use miden_objects::Word;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::account::{Account, AccountDelta};
use miden_objects::block::account_tree::{AccountTree, account_id_to_smt_key};
use miden_objects::block::{
    BlockAccountUpdate,
    BlockBody,
    BlockHeader,
    BlockNoteTree,
    BlockNumber,
    BlockProof,
    FeeParameters,
    ProvenBlock,
};
use miden_objects::crypto::merkle::{Forest, LargeSmt, MemoryStorage, MmrPeaks, Smt};
use miden_objects::ecdsa_signer::EcdsaSigner;
use miden_objects::note::Nullifier;
use miden_objects::transaction::OrderedTransactionHeaders;
use miden_objects::utils::serde::{ByteReader, Deserializable, DeserializationError};

use crate::errors::GenesisError;

pub mod config;

// GENESIS STATE
// ================================================================================================

/// Represents the state at genesis, which will be used to derive the genesis block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenesisState<S> {
    pub accounts: Vec<Account>,
    pub fee_parameters: FeeParameters,
    pub version: u32,
    pub timestamp: u32,
    pub signer: S,
}

/// A type-safety wrapper ensuring that genesis block data can only be created from
/// [`GenesisState`].
pub struct GenesisBlock(ProvenBlock);

impl GenesisBlock {
    pub fn inner(&self) -> &ProvenBlock {
        &self.0
    }

    pub fn into_inner(self) -> ProvenBlock {
        self.0
    }
}

impl<S> GenesisState<S> {
    pub fn new(
        accounts: Vec<Account>,
        fee_parameters: FeeParameters,
        version: u32,
        timestamp: u32,
        signer: S,
    ) -> Self {
        Self {
            accounts,
            fee_parameters,
            version,
            timestamp,
            signer,
        }
    }
}

impl<S: EcdsaSigner> GenesisState<S> {
    /// Returns the block header and the account SMT
    pub fn into_block(self) -> Result<GenesisBlock, GenesisError> {
        let accounts: Vec<BlockAccountUpdate> = self
            .accounts
            .iter()
            .map(|account| {
                let account_update_details = if account.id().is_public() {
                    AccountUpdateDetails::Delta(
                        AccountDelta::try_from(account.clone())
                            .map_err(GenesisError::AccountDelta)?,
                    )
                } else {
                    AccountUpdateDetails::Private
                };

                Ok(BlockAccountUpdate::new(
                    account.id(),
                    account.commitment(),
                    account_update_details,
                ))
            })
            .collect::<Result<Vec<_>, GenesisError>>()?;

        // Convert account updates to SMT entries using account_id_to_smt_key
        let smt_entries = accounts.iter().map(|update| {
            (account_id_to_smt_key(update.account_id()), update.final_state_commitment())
        });

        // Create LargeSmt with MemoryStorage
        let smt = LargeSmt::with_entries(MemoryStorage::default(), smt_entries)
            .expect("Failed to create LargeSmt for genesis accounts");

        let account_smt = AccountTree::new(smt).expect("Failed to create AccountTree for genesis");

        let empty_nullifiers: Vec<Nullifier> = Vec::new();
        let empty_nullifier_tree = Smt::new();

        let empty_output_notes = Vec::new();
        let empty_block_note_tree = BlockNoteTree::empty();

        let empty_transactions = OrderedTransactionHeaders::new_unchecked(Vec::new());

        let header = BlockHeader::new(
            self.version,
            Word::empty(),
            BlockNumber::GENESIS,
            MmrPeaks::new(Forest::empty(), Vec::new()).unwrap().hash_peaks(),
            account_smt.root(),
            empty_nullifier_tree.root(),
            empty_block_note_tree.root(),
            Word::empty(),
            TransactionKernel.to_commitment(),
            self.signer.public_key(),
            self.fee_parameters,
            self.timestamp,
        );

        let body = BlockBody::new_unchecked(
            accounts,
            empty_output_notes,
            empty_nullifiers,
            empty_transactions,
        );

        let block_proof = BlockProof::new_dummy();

        let signature = self.signer.sign(header.commitment());
        // SAFETY: Header and accounts should be valid by construction.
        // No notes or nullifiers are created at genesis, which is consistent with the above empty
        // block note tree root and empty nullifier tree root.
        Ok(GenesisBlock(ProvenBlock::new_unchecked(header, signature, body, block_proof)))
    }
}

// SERIALIZATION
// ================================================================================================

impl<S: Deserializable> Deserializable for GenesisState<S> {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let num_accounts = source.read_usize()?;
        let accounts = source.read_many::<Account>(num_accounts)?;

        let version = source.read_u32()?;
        let timestamp = source.read_u32()?;
        let fee_parameters = source.read::<FeeParameters>()?;
        let signer = source.read::<S>()?;

        Ok(Self::new(accounts, fee_parameters, version, timestamp, signer))
    }
}
