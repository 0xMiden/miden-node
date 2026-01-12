use assert_matches::assert_matches;
use miden_node_utils::ErrorReport;
use miden_protocol::Word;

use super::*;
use crate::test_utils::note::{mock_note, mock_output_note};
use crate::test_utils::{MockProvenTxBuilder, mock_account_id};

#[test]
fn rejects_expired_transaction() {
    let chain_tip = BlockNumber::from(123);
    let mut uut = InflightState::new(chain_tip, 5, 0u32);

    let expired = MockProvenTxBuilder::with_account_index(0)
        .expiration_block_num(chain_tip)
        .build();
    let expired =
        AuthenticatedTransaction::from_inner(expired).with_authentication_height(chain_tip);

    let err = uut.add_transaction(&expired).unwrap_err();
    assert_matches!(err, AddTransactionError::Expired { .. });
}

/// Ensures that the specified expiration slack is adhered to.
#[test]
fn expiration_slack_is_respected() {
    let slack = 3;
    let chain_tip = BlockNumber::from(123);
    let expiration_limit = chain_tip + slack;
    let mut uut = InflightState::new(chain_tip, 5, slack);

    let unexpired = MockProvenTxBuilder::with_account_index(0)
        .expiration_block_num(expiration_limit + 1)
        .build();
    let unexpired =
        AuthenticatedTransaction::from_inner(unexpired).with_authentication_height(chain_tip);

    uut.add_transaction(&unexpired).unwrap();

    let expired = MockProvenTxBuilder::with_account_index(1)
        .expiration_block_num(expiration_limit)
        .build();
    let expired =
        AuthenticatedTransaction::from_inner(expired).with_authentication_height(chain_tip);

    let err = uut.add_transaction(&expired).unwrap_err();
    assert_matches!(err, AddTransactionError::Expired { .. });
}

#[test]
fn rejects_duplicate_nullifiers() {
    let account = mock_account_id(1);
    let states = (1u8..=4).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

    let note_seed = 123;
    // We need to make the note available first, in order for it to be consumed at all.
    let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1])
        .output_notes(vec![mock_output_note(note_seed)])
        .build();
    let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2])
        .unauthenticated_notes(vec![mock_note(note_seed)])
        .build();
    let tx2 = MockProvenTxBuilder::with_account(account, states[2], states[3])
        .unauthenticated_notes(vec![mock_note(note_seed)])
        .build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1)).unwrap();

    let err = uut.add_transaction(&AuthenticatedTransaction::from_inner(tx2)).unwrap_err();

    assert_matches!(
        err,
        AddTransactionError::VerificationFailed(VerifyTxError::InputNotesAlreadyConsumed(
            notes
        )) if notes == vec![mock_note(note_seed).nullifier()]
    );
}

#[test]
fn rejects_duplicate_output_notes() {
    let account = mock_account_id(1);
    let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

    let note = mock_output_note(123);
    let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1])
        .output_notes(vec![note.clone()])
        .build();
    let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2])
        .output_notes(vec![note.clone()])
        .build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();

    let err = uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1)).unwrap_err();

    assert_matches!(
        err,
        AddTransactionError::VerificationFailed(VerifyTxError::OutputNotesAlreadyExist(
            notes
        )) if notes == vec![note.id()]
    );
}

#[test]
fn rejects_account_state_mismatch() {
    let account = mock_account_id(1);
    let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

    let tx = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    let err = uut
        .add_transaction(&AuthenticatedTransaction::from_inner(tx).with_store_state(states[2]))
        .unwrap_err();

    assert_matches!(
        err,
        AddTransactionError::VerificationFailed(VerifyTxError::IncorrectAccountInitialCommitment {
            tx_initial_account_commitment: init_state,
            current_account_commitment: current_state,
        }) if init_state == states[0] && current_state == states[2].into()
    );
}

#[test]
fn account_state_transitions() {
    let account = mock_account_id(1);
    let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

    let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
    let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2]).build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1).with_empty_store_state())
        .unwrap();
}

#[test]
fn new_account_state_defaults_to_zero() {
    let account = mock_account_id(1);

    let tx =
        MockProvenTxBuilder::with_account(account, [0u8, 0, 0, 0].into(), [1u8, 0, 0, 0].into())
            .build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx).with_empty_store_state())
        .unwrap();
}

#[test]
fn inflight_account_state_overrides_input_state() {
    let account = mock_account_id(1);
    let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

    let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
    let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2]).build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();

    // Feed in an old state via input. This should be ignored, and the previous tx's final
    // state should be used.
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1).with_store_state(states[0]))
        .unwrap();
}

#[test]
fn dependency_tracking() {
    let account = mock_account_id(1);
    let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();
    let note_seed = 123;

    // Parent via account state.
    let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
    // Parent via output note.
    let tx1 = MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
        .output_notes(vec![mock_output_note(note_seed)])
        .build();

    let tx = MockProvenTxBuilder::with_account(account, states[1], states[2])
        .unauthenticated_notes(vec![mock_note(note_seed)])
        .build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0.clone())).unwrap();
    uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1.clone())).unwrap();

    let parents = uut
        .add_transaction(&AuthenticatedTransaction::from_inner(tx).with_empty_store_state())
        .unwrap();
    let expected = BTreeSet::from([tx0.id(), tx1.id()]);

    assert_eq!(parents, expected);
}

#[test]
fn committed_parents_are_not_tracked() {
    let account = mock_account_id(1);
    let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();
    let note_seed = 123;

    // Parent via account state.
    let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
    let tx0 = AuthenticatedTransaction::from_inner(tx0);
    // Parent via output note.
    let tx1 = MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
        .output_notes(vec![mock_output_note(note_seed)])
        .build();
    let tx1 = AuthenticatedTransaction::from_inner(tx1);

    let tx = MockProvenTxBuilder::with_account(account, states[1], states[2])
        .unauthenticated_notes(vec![mock_note(note_seed)])
        .build();

    let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
    uut.add_transaction(&tx0.clone()).unwrap();
    uut.add_transaction(&tx1.clone()).unwrap();

    // Commit the parents, which should remove them from dependency tracking.
    uut.commit_block([tx0.id(), tx1.id()]);

    let parents = uut
        .add_transaction(&AuthenticatedTransaction::from_inner(tx).with_empty_store_state())
        .unwrap();

    assert!(parents.is_empty());
}

#[test]
fn tx_insertions_and_reversions_cancel_out() {
    // Reverting txs should be equivalent to them never being inserted.
    //
    // We test this by reverting some txs and equating it to the remaining set.
    // This is a form of property test.
    let states = (1u8..=5).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();
    let txs = vec![
        MockProvenTxBuilder::with_account(mock_account_id(1), states[0], states[1]),
        MockProvenTxBuilder::with_account(mock_account_id(1), states[1], states[2])
            .output_notes(vec![mock_output_note(111), mock_output_note(222)]),
        MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
            .unauthenticated_notes(vec![mock_note(222)]),
        MockProvenTxBuilder::with_account(mock_account_id(1), states[2], states[3]),
        MockProvenTxBuilder::with_account(mock_account_id(2), states[1], states[2])
            .unauthenticated_notes(vec![mock_note(111)])
            .output_notes(vec![mock_output_note(45)]),
    ];

    let txs = txs
        .into_iter()
        .map(MockProvenTxBuilder::build)
        .map(AuthenticatedTransaction::from_inner)
        .collect::<Vec<_>>();

    for i in 0..states.len() {
        // Insert all txs and then revert the last `i` of them.
        // This should match only inserting the first `N-i` of them.
        let mut reverted = InflightState::new(BlockNumber::default(), 1, 0u32);
        for (idx, tx) in txs.iter().enumerate() {
            reverted.add_transaction(tx).unwrap_or_else(|err| {
                panic!("Inserting tx #{idx} in iteration {i} should succeed: {}", err.as_report())
            });
        }
        reverted.revert_transactions(
            txs.iter().rev().take(i).rev().map(AuthenticatedTransaction::id).collect(),
        );

        let mut inserted = InflightState::new(BlockNumber::default(), 1, 0u32);
        for (idx, tx) in txs.iter().rev().skip(i).rev().enumerate() {
            inserted.add_transaction(tx).unwrap_or_else(|err| {
                panic!("Inserting tx #{idx} in iteration {i} should succeed: {}", err.as_report())
            });
        }

        assert_eq!(reverted, inserted, "Iteration {i}");
    }
}

#[test]
fn pruning_committed_state() {
    //! This is a form of property test, where we assert that pruning the first `i` of `N`
    //! transactions is equivalent to only inserting the last `N-i` transactions.
    let states = (1u8..=5).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

    // Skipping initial txs means that output notes required for subsequent unauthenticated
    // input notes wont' always be present. To work around this, we instead only use
    // authenticated input notes.
    let txs = vec![
        MockProvenTxBuilder::with_account(mock_account_id(1), states[0], states[1]),
        MockProvenTxBuilder::with_account(mock_account_id(1), states[1], states[2])
            .output_notes(vec![mock_output_note(111), mock_output_note(222)]),
        MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
            .nullifiers(vec![mock_note(222).nullifier()]),
        MockProvenTxBuilder::with_account(mock_account_id(1), states[2], states[3]),
        MockProvenTxBuilder::with_account(mock_account_id(2), states[1], states[2])
            .nullifiers(vec![mock_note(111).nullifier()])
            .output_notes(vec![mock_output_note(45)]),
    ];

    let txs = txs
        .into_iter()
        .map(MockProvenTxBuilder::build)
        .map(AuthenticatedTransaction::from_inner)
        .collect::<Vec<_>>();

    for i in 0..states.len() {
        // Insert all txs and then commit and prune the first `i` of them.
        //
        // This should match only inserting the final `N-i` transactions.
        // Note: we force all committed state to immediately be pruned by setting
        // it to zero.
        let mut committed = InflightState::new(BlockNumber::default(), 0, 0u32);
        for (idx, tx) in txs.iter().enumerate() {
            committed.add_transaction(tx).unwrap_or_else(|err| {
                panic!("Inserting tx #{idx} in iteration {i} should succeed: {}", err.as_report())
            });
        }
        committed.commit_block(txs.iter().take(i).map(AuthenticatedTransaction::id));

        let mut inserted = InflightState::new(BlockNumber::from(1), 0, 0u32);
        for (idx, tx) in txs.iter().skip(i).enumerate() {
            // We need to adjust the height since we are effectively at block "1" now.
            let tx = tx.clone().with_authentication_height(1.into());
            inserted.add_transaction(&tx).unwrap_or_else(|err| {
                panic!("Inserting tx #{idx} in iteration {i} should succeed: {}", err.as_report())
            });
        }

        assert_eq!(committed, inserted, "Iteration {i}");
    }
}
