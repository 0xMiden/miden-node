use std::collections::BTreeSet;

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::Word;
use miden_objects::note::Nullifier;
use miden_objects::transaction::TransactionId;

use crate::state::State;
use crate::store::StoreClient;

/// Regression test for issue #1312
///
/// This test verifies that the NtxBuilder's state handling correctly processes transactions
/// that contain nullifiers without corresponding network notes. This scenario can occur when:
/// - A transaction consumes a non-network note (e.g., a private note)
/// - The nullifier is included in the transaction but is not tracked by the NtxBuilder
///
/// The test ensures...
/// 1. such transactions are accepted
/// 2. the state remains consistent after processing
/// 3. the nullifier is skipped, since it has no corresponding note
/// 4. subsequent operations continue to work correctly
#[tokio::test]
async fn issue_1312_nullifier_without_note() {
    let store = StoreClient::new("http://example.com:1111".parse().unwrap());
    let mut state = State::load(store).await.unwrap();

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
        MempoolEvent::TransactionsReverted(std::iter::once(tx_id).collect::<BTreeSet<_>>());
    state.mempool_update(revert_event).await.unwrap();

    assert_eq!(state.chain_tip(), initial_chain_tip);
}
