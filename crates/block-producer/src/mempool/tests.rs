use miden_objects::block::BlockNumber;
use pretty_assertions::assert_eq;

use super::*;
use crate::test_utils::{batch::TransactionBatchConstructor, MockProvenTxBuilder};

impl Mempool {
    fn for_tests() -> Self {
        Self::new(
            BlockNumber::GENESIS,
            BatchBudget::default(),
            BlockBudget::default(),
            5,
            u32::default(),
        )
    }
}

#[tokio::test]
async fn mempool_trace() {
    let (mut rx_export, _rx_shutdown) = miden_node_utils::logging::setup_test_tracing().unwrap();

    let mut uut = Mempool::for_tests();
    let txs = MockProvenTxBuilder::sequential();
    uut.add_transaction(txs[0].clone()).unwrap();

    let span_data = rx_export.recv().await.unwrap();
    assert_eq!(span_data.name, "mempool.add_transaction");
    assert!(span_data.attributes.iter().any(|kv| kv.key == "code.namespace".into()
        && kv.value == "miden_node_block_producer::mempool".into()));
}

// BATCH FAILED TESTS
// ================================================================================================

#[test]
fn children_of_failed_batches_are_ignored() {
    // Batches are proved concurrently. This makes it possible for a child job to complete after
    // the parent has been reverted (and therefore reverting the child job). Such a child job
    // should be ignored.
    let txs = MockProvenTxBuilder::sequential();

    let mut uut = Mempool::for_tests();
    uut.add_transaction(txs[0].clone()).unwrap();
    let (parent_batch, batch_txs) = uut.select_batch().unwrap();
    assert_eq!(batch_txs, vec![txs[0].clone()]);

    uut.add_transaction(txs[1].clone()).unwrap();
    let (child_batch_a, batch_txs) = uut.select_batch().unwrap();
    assert_eq!(batch_txs, vec![txs[1].clone()]);

    uut.add_transaction(txs[2].clone()).unwrap();
    let (_, batch_txs) = uut.select_batch().unwrap();
    assert_eq!(batch_txs, vec![txs[2].clone()]);

    // Child batch jobs are now dangling.
    uut.batch_failed(parent_batch);
    let reference = uut.clone();

    // Success or failure of the child job should effectively do nothing.
    uut.batch_failed(child_batch_a);
    assert_eq!(uut, reference);

    let proven_batch = ProvenBatch::mocked_from_transactions([txs[2].raw_proven_transaction()]);
    uut.batch_proved(proven_batch);
    assert_eq!(uut, reference);
}

#[test]
fn failed_batch_transactions_are_requeued() {
    let txs = MockProvenTxBuilder::sequential();

    let mut uut = Mempool::for_tests();
    uut.add_transaction(txs[0].clone()).unwrap();
    uut.select_batch().unwrap();

    uut.add_transaction(txs[1].clone()).unwrap();
    let (failed_batch, _) = uut.select_batch().unwrap();

    uut.add_transaction(txs[2].clone()).unwrap();
    uut.select_batch().unwrap();

    // Middle batch failed, so it and its child transaction should be re-entered into the queue.
    uut.batch_failed(failed_batch);

    let mut reference = Mempool::for_tests();
    reference.add_transaction(txs[0].clone()).unwrap();
    reference.select_batch().unwrap();
    reference.add_transaction(txs[1].clone()).unwrap();
    reference.add_transaction(txs[2].clone()).unwrap();

    assert_eq!(uut, reference);
}

// BLOCK COMMITTED TESTS
// ================================================================================================

/// Expired transactions should be reverted once their expiration block is committed.
#[test]
fn block_commit_reverts_expired_txns() {
    let mut uut = Mempool::for_tests();

    let tx_to_commit = MockProvenTxBuilder::with_account_index(0).build();
    let tx_to_commit = AuthenticatedTransaction::from_inner(tx_to_commit);

    // Force the tx into a pending block.
    uut.add_transaction(tx_to_commit.clone()).unwrap();
    uut.select_batch().unwrap();
    uut.batch_proved(ProvenBatch::mocked_from_transactions(
        [tx_to_commit.raw_proven_transaction()],
    ));
    let (block, _) = uut.select_block();
    // A reverted transaction behaves as if it never existed, the current state is the expected
    // outcome, plus an extra committed block at the end.
    let mut reference = uut.clone();

    // Add a new transaction which will expire when the pending block is committed.
    let tx_to_revert =
        MockProvenTxBuilder::with_account_index(1).expiration_block_num(block).build();
    let tx_to_revert = AuthenticatedTransaction::from_inner(tx_to_revert);
    uut.add_transaction(tx_to_revert).unwrap();

    // Commit the pending block which should revert the above tx.
    uut.commit_block();
    reference.commit_block();

    assert_eq!(uut, reference);
}

#[test]
fn empty_block_commitment() {
    let mut uut = Mempool::for_tests();

    for _ in 0..3 {
        let (_block, _) = uut.select_block();
        uut.commit_block();
    }
}

#[test]
#[should_panic]
fn block_commitment_is_rejected_if_no_block_is_in_flight() {
    Mempool::for_tests().commit_block();
}

#[test]
#[should_panic]
fn cannot_have_multple_inflight_blocks() {
    let mut uut = Mempool::for_tests();

    uut.select_block();
    uut.select_block();
}

// BLOCK FAILED TESTS
// ================================================================================================

/// A failed block should have all of its transactions reverted.
#[test]
fn block_failure_reverts_its_transactions() {
    let mut uut = Mempool::for_tests();
    // We will revert everything so the reference should be the empty mempool.
    let reference = uut.clone();

    let reverted_txs = MockProvenTxBuilder::sequential();

    uut.add_transaction(reverted_txs[0].clone()).unwrap();
    uut.select_batch().unwrap();
    uut.batch_proved(ProvenBatch::mocked_from_transactions([
        reverted_txs[0].raw_proven_transaction()
    ]));

    // Block 1 will contain just the first batch.
    let (_number, _batches) = uut.select_block();

    // Create another dependent batch.
    uut.add_transaction(reverted_txs[1].clone()).unwrap();
    uut.select_batch();
    // Create another dependent transaction.
    uut.add_transaction(reverted_txs[2].clone()).unwrap();

    // Fail the block which should result in everything reverting.
    uut.rollback_block();

    assert_eq!(uut, reference);
}

// TRANSACTION REVERSION TESTS
// ================================================================================================

/// Ensures that reverting transactions is equivalent to them never being inserted at all.
///
/// This checks that there are no forgotten links to them exist anywhere in the mempool by
/// comparing to a reference mempool that never had them inserted.
#[test]
fn reverted_transactions_and_descendents_are_non_existent() {
    let mut uut = Mempool::for_tests();

    let reverted_txs = MockProvenTxBuilder::sequential();

    uut.add_transaction(reverted_txs[0].clone()).unwrap();
    uut.select_batch().unwrap();

    uut.add_transaction(reverted_txs[1].clone()).unwrap();
    uut.select_batch().unwrap();

    uut.add_transaction(reverted_txs[2].clone()).unwrap();
    uut.revert_transactions(vec![reverted_txs[1].id()]).unwrap();

    // We expect the second batch and the latter reverted txns to be non-existent.
    let mut reference = Mempool::for_tests();
    reference.add_transaction(reverted_txs[0].clone()).unwrap();
    reference.select_batch().unwrap();

    assert_eq!(uut, reference);
}

/// Reverting transactions causes their batches to also revert. These batches in turn contain
/// non-reverted transactions which should be requeued (and not reverted).
#[test]
fn reverted_transaction_batches_are_requeued() {
    let mut uut = Mempool::for_tests();

    let unrelated_txs = MockProvenTxBuilder::sequential();
    let reverted_txs = MockProvenTxBuilder::sequential();

    uut.add_transaction(reverted_txs[0].clone()).unwrap();
    uut.add_transaction(unrelated_txs[0].clone()).unwrap();
    uut.select_batch().unwrap();

    uut.add_transaction(reverted_txs[1].clone()).unwrap();
    uut.add_transaction(unrelated_txs[1].clone()).unwrap();
    uut.select_batch().unwrap();

    uut.add_transaction(reverted_txs[2].clone()).unwrap();
    uut.add_transaction(unrelated_txs[2].clone()).unwrap();
    uut.revert_transactions(vec![reverted_txs[1].id()]).unwrap();

    // We expect the second batch and the latter reverted txns to be non-existent.
    let mut reference = Mempool::for_tests();
    reference.add_transaction(reverted_txs[0].clone()).unwrap();
    reference.add_transaction(unrelated_txs[0].clone()).unwrap();
    reference.select_batch().unwrap();
    reference.add_transaction(unrelated_txs[1].clone()).unwrap();
    reference.add_transaction(unrelated_txs[2].clone()).unwrap();

    assert_eq!(uut, reference);
}
