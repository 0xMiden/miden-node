use std::ops::Range;

use itertools::Itertools;
use miden_node_utils::fee::test_fee;
use miden_objects::account::AccountId;
use miden_objects::asset::FungibleAsset;
use miden_objects::block::BlockNumber;
use miden_objects::note::{Note, NoteExecutionHint, NoteHeader, NoteMetadata, NoteType, Nullifier};
use miden_objects::testing::account_id::ACCOUNT_ID_NATIVE_ASSET_FAUCET;
use miden_objects::transaction::{
    InputNote,
    OutputNote,
    ProvenTransaction,
    ProvenTransactionBuilder,
};
use miden_objects::vm::ExecutionProof;
use miden_objects::{Felt, Hasher, ONE, Word};
use rand::Rng;

use super::MockPrivateAccount;
use crate::domain::transaction::AuthenticatedTransaction;

pub struct MockProvenTxBuilder {
    account_id: AccountId,
    initial_account_commitment: Word,
    final_account_commitment: Word,
    expiration_block_num: BlockNumber,
    output_notes: Option<Vec<OutputNote>>,
    input_notes: Option<Vec<InputNote>>,
    nullifiers: Option<Vec<Nullifier>>,
    fee: FungibleAsset,
}

impl MockProvenTxBuilder {
    pub fn with_account_index(account_index: u32) -> Self {
        let mock_account: MockPrivateAccount = account_index.into();

        Self::with_account(mock_account.id, mock_account.states[0], mock_account.states[1])
    }

    /// Generates 3 random, sequential transactions acting on the same account.
    pub fn sequential() -> [AuthenticatedTransaction; 3] {
        let mut rng = rand::rng();
        let mock_account: MockPrivateAccount<4> = rng.random::<u32>().into();

        (0..3)
            .map(|i| {
                Self::with_account(
                    mock_account.id,
                    mock_account.states[i],
                    mock_account.states[i + 1],
                )
            })
            .map(|tx| AuthenticatedTransaction::from_inner(tx.build()))
            .collect_vec()
            .try_into()
            .expect("Sizes should match")
    }

    pub fn with_account(
        account_id: AccountId,
        initial_account_commitment: Word,
        final_account_commitment: Word,
    ) -> Self {
        Self {
            account_id,
            initial_account_commitment,
            final_account_commitment,
            expiration_block_num: u32::MAX.into(),
            output_notes: None,
            input_notes: None,
            nullifiers: None,
            fee: test_fee(),
        }
    }

    #[must_use]
    pub fn unauthenticated_notes(mut self, notes: Vec<Note>) -> Self {
        self.input_notes = Some(notes.into_iter().map(InputNote::unauthenticated).collect());

        self
    }

    #[must_use]
    pub fn nullifiers(mut self, nullifiers: Vec<Nullifier>) -> Self {
        self.nullifiers = Some(nullifiers);

        self
    }

    #[must_use]
    pub fn expiration_block_num(mut self, expiration_block_num: BlockNumber) -> Self {
        self.expiration_block_num = expiration_block_num;

        self
    }

    #[must_use]
    pub fn output_notes(mut self, notes: Vec<OutputNote>) -> Self {
        self.output_notes = Some(notes);

        self
    }

    #[must_use]
    pub fn nullifiers_range(self, range: Range<u64>) -> Self {
        let nullifiers = range
            .map(|index| {
                let nullifier = Word::from([ONE, ONE, ONE, Felt::new(index)]);

                Nullifier::from(nullifier)
            })
            .collect();

        self.nullifiers(nullifiers)
    }

    #[must_use]
    pub fn private_notes_created_range(self, range: Range<u64>) -> Self {
        let notes = range
            .map(|note_index| {
                let note_id = Hasher::hash(&note_index.to_be_bytes());
                let note_metadata = NoteMetadata::new(
                    self.account_id,
                    NoteType::Private,
                    0.into(),
                    NoteExecutionHint::none(),
                    ONE,
                )
                .unwrap();

                OutputNote::Header(NoteHeader::new(note_id.into(), note_metadata))
            })
            .collect();

        self.output_notes(notes)
    }

    pub fn build(self) -> ProvenTransaction {
        ProvenTransactionBuilder::new(
            self.account_id,
            self.initial_account_commitment,
            self.final_account_commitment,
            Word::empty(),
            BlockNumber::from(0),
            Word::empty(),
            self.fee,
            self.expiration_block_num,
            ExecutionProof::new_dummy(),
        )
        .add_input_notes(self.input_notes.unwrap_or_default())
        .add_input_notes(self.nullifiers.unwrap_or_default())
        .add_output_notes(self.output_notes.unwrap_or_default())
        .build()
        .unwrap()
    }
}

pub fn mock_proven_tx(
    account_index: u8,
    unauthenticated_notes: Vec<Note>,
    output_notes: Vec<OutputNote>,
) -> ProvenTransaction {
    MockProvenTxBuilder::with_account_index(account_index.into())
        .unauthenticated_notes(unauthenticated_notes)
        .output_notes(output_notes)
        .build()
}
