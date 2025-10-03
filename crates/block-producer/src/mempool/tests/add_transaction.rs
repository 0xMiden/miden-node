use std::sync::Arc;

use assert_matches::assert_matches;
use miden_objects::Word;
use miden_objects::block::BlockHeader;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::AddTransactionError;
use crate::mempool::Mempool;
use crate::test_utils::MockProvenTxBuilder;

/// Ensures that transactions that expire before or within the expiration slack of the chain tip
/// are rejected.
mod tx_expiration {
    use super::*;

    fn setup() -> Mempool {
        let (mut uut, _) = Mempool::for_tests();
        assert!(
            uut.config.expiration_slack > 0,
            "Test is only useful if there is some expiration slack"
        );

        // Create at least some locally retained state.
        let slack = uut.config.expiration_slack;
        for _ in 0..slack + 10 {
            let (number, _) = uut.select_block();
            let header = BlockHeader::mock(number, None, None, &[], Word::default());
            uut.commit_block(header);
        }

        uut
    }

    #[test]
    fn expiration_after_slack_limit_is_accepted() {
        let mut uut = setup();
        let limit = uut.chain_tip + uut.config.expiration_slack;

        let tx = MockProvenTxBuilder::with_account_index(0)
            .expiration_block_num(limit.child())
            .build();
        let tx = AuthenticatedTransaction::from_inner(tx).with_authentication_height(uut.chain_tip);
        let tx = Arc::new(tx);
        uut.add_transaction(tx).unwrap();
    }

    #[test]
    fn expiration_within_slack_limit_is_rejected() {
        let mut uut = setup();
        let limit = uut.chain_tip + uut.config.expiration_slack;

        for i in uut.chain_tip.child().as_u32()..=limit.as_u32() {
            let tx = MockProvenTxBuilder::with_account_index(0)
                .expiration_block_num(i.into())
                .build();
            let tx =
                AuthenticatedTransaction::from_inner(tx).with_authentication_height(uut.chain_tip);
            let tx = Arc::new(tx);
            let result = uut.add_transaction(tx);

            assert_matches!(
                result,
                Err(AddTransactionError::Expired { .. }),
                "Failed run with expiration {i} and limit {limit}"
            );
        }
    }

    #[test]
    fn already_expired_is_rejected() {
        let mut uut = setup();
        let tx = MockProvenTxBuilder::with_account_index(0)
            .expiration_block_num(uut.chain_tip)
            .build();
        let tx = AuthenticatedTransaction::from_inner(tx).with_authentication_height(uut.chain_tip);
        let tx = Arc::new(tx);
        let result = uut.add_transaction(tx);

        assert_matches!(result, Err(AddTransactionError::Expired { .. }));
    }
}

/// Ensures that a transaction's authentication height is always within the overlap between the
/// store chain tip and the locally retained state.
mod authentication_height {
    use super::*;

    fn setup() -> Mempool {
        let (mut uut, _) = Mempool::for_tests();
        assert!(
            uut.config.state_retention.get() > 0,
            "Test is only useful if there is some retained state"
        );

        // Create at least some locally retained state.
        let retention = uut.config.state_retention.get();
        for _ in 0..retention + 10 {
            let (number, _) = uut.select_block();
            let header = BlockHeader::mock(number, None, None, &[], Word::default());
            uut.commit_block(header);
        }

        uut
    }

    /// We expect a rejection if the tx was authenticated in some gap between the store and local
    /// state i.e. oldest_local - 2.
    #[test]
    fn stale_inputs_are_rejected() {
        let mut uut = setup();

        let oldest_local = uut.chain_tip.as_u32() - uut.config.state_retention.get() as u32 + 1;

        let tx = MockProvenTxBuilder::with_account_index(0).build();
        let tx = AuthenticatedTransaction::from_inner(tx)
            .with_authentication_height((oldest_local - 2).into());
        let tx = Arc::new(tx);
        uut.add_transaction(tx).unwrap_err();
    }

    /// Ensures that we guard against authenication height from lying about imaginary blocks beyond
    /// the chain tip. Since the authentication height is determined by the store, and this is
    /// considered internal, we panic as this is completely abnormal.
    #[test]
    #[should_panic]
    fn inputs_from_beyond_the_chain_tip_are_rejected() {
        let mut uut = setup();

        let tx = MockProvenTxBuilder::with_account_index(0).build();
        let tx = AuthenticatedTransaction::from_inner(tx)
            .with_authentication_height(uut.chain_tip.child());
        let tx = Arc::new(tx);
        let _ = uut.add_transaction(tx);
    }

    /// We expect transactions to be accepted in the `oldest-1..` range. We already test `>
    /// chain_tip` above. The `-1` is because we only require that there is _no gap_ between the
    /// authentication height and the local state, which would mean we are unable to authenticate
    /// against those blocks.
    #[test]
    fn inputs_from_within_overlap_are_accepted() {
        let mut uut = setup();

        let oldest_local = uut.chain_tip.as_u32() - uut.config.state_retention.get() as u32 + 1;

        for i in oldest_local - 1..=uut.chain_tip.as_u32() {
            let tx = MockProvenTxBuilder::with_account_index(i).build();
            let tx = AuthenticatedTransaction::from_inner(tx).with_authentication_height(i.into());
            let tx = Arc::new(tx);

            let result = uut.add_transaction(tx);

            assert_matches!(
                result,
                Ok(..),
                "Failed run with authentication height {i}, chain tip {} and oldest local {oldest_local}",
                uut.chain_tip
            );
        }
    }
}

// state
//   - nullifier
//   - account state
//   - new account
//   - new account with store state
//   - unauthenticated notes
//   - duplicate output notes
//   - state overlap with store
