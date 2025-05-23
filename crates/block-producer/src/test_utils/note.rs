use miden_lib::transaction::TransactionKernel;
use miden_objects::{note::Note, testing::note::NoteBuilder, transaction::OutputNote};
use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};

use crate::test_utils::account::mock_account_id;

pub fn mock_note(num: u8) -> Note {
    let sender = mock_account_id(num);
    NoteBuilder::new(sender, ChaCha20Rng::from_seed([num; 32]))
        .build(&TransactionKernel::assembler().with_debug_mode(true))
        .unwrap()
}

pub fn mock_output_note(num: u8) -> OutputNote {
    OutputNote::Full(mock_note(num))
}
