use std::collections::HashSet;

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::fee::test_fee_params;
use miden_objects::Word;
use miden_objects::block::BlockHeader;
use miden_objects::ecdsa_signer::{EcdsaSigner, LocalEcdsaSigner};
use miden_objects::note::Nullifier;
use miden_objects::transaction::{PartialBlockchain, TransactionId};

use crate::state::State;
use crate::store::StoreClient;

/// Helper function to create a mock State for testing without needing a real store.
fn create_mock_state() -> State {
    // Create a minimal genesis block header
    let chain_tip_header = BlockHeader::new(
        1_u8.into(),                            // version
        Word::default(),                        // prev_hash
        0_u32.into(),                           // block_num (genesis)
        Word::default(),                        // chain_root
        Word::default(),                        // account_root
        Word::default(),                        // nullifier_root
        Word::default(),                        // note_root
        Word::default(),                        // tx_hash
        Word::default(),                        // kernel_root
        LocalEcdsaSigner::dummy().public_key(), // public_key
        test_fee_params(),                      // fee_parameters
        0_u32,                                  // timestamp
    );

    // Create an empty partial blockchain
    let chain_mmr = PartialBlockchain::default();
    // Create a mock store client (it won't be used in this test)
    let store = StoreClient::new("http://localhost:9999".parse().unwrap());

    State::new_for_testing(chain_tip_header, chain_mmr, store)
}

/// Regression test for issue #1312
///
/// This test verifies that the `NtxBuilder`'s state handling correctly processes transactions
/// that contain nullifiers without corresponding network notes. This scenario can occur when:
/// - A transaction consumes a non-network note (e.g., a private note)
/// - The nullifier is included in the transaction but is not tracked by the `NtxBuilder`
///
/// The test ensures...
/// 1. such transactions are accepted
/// 2. the state remains consistent after processing
/// 3. the nullifier is skipped, since it has no corresponding note
/// 4. subsequent operations continue to work correctly
#[tokio::test]
async fn issue_1312_nullifier_without_note() {
    let mut state = create_mock_state();

    let initial_chain_tip = state.chain_tip();

    let tx_id =
        TransactionId::new(Word::default(), Word::default(), Word::default(), Word::default());
    let nullifier =
        Nullifier::new(Word::default(), Word::default(), Word::default(), Word::default());

    // Add transaction with nullifier but no network notes.
    let add_event = MempoolEvent::TransactionAdded {
        id: tx_id,
        nullifiers: vec![nullifier],
        network_notes: vec![],
        account_delta: None,
    };

    state.mempool_update(add_event).await.unwrap();

    assert_eq!(state.chain_tip(), initial_chain_tip);

    // Verify state integrity.
    let candidate = state.select_candidate(std::num::NonZeroUsize::new(10).unwrap());
    assert!(candidate.is_none());

    // Revert transaction.
    let revert_event =
        MempoolEvent::TransactionsReverted(std::iter::once(tx_id).collect::<HashSet<_>>());
    state.mempool_update(revert_event).await.unwrap();

    assert_eq!(state.chain_tip(), initial_chain_tip);
}
