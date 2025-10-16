//! Tests for `AccountTreeWithHistory`

#[cfg(test)]
mod account_tree_with_history_tests {
    use miden_objects::Word;
    use miden_objects::block::{AccountTree, BlockNumber};
    use miden_objects::testing::account_id::AccountIdBuilder;

    use super::super::*;

    #[test]
    fn test_historical_queries() {
        let ids = [
            AccountIdBuilder::new().build_with_seed([1; 32]),
            AccountIdBuilder::new().build_with_seed([2; 32]),
            AccountIdBuilder::new().build_with_seed([3; 32]),
        ];

        let states = [
            vec![
                (ids[0], Word::from([1u32, 0, 0, 0])),
                (ids[1], Word::from([2u32, 0, 0, 0])),
                (ids[2], Word::from([3u32, 0, 0, 0])),
            ],
            vec![
                (ids[0], Word::from([10u32, 0, 0, 0])),
                (ids[1], Word::from([2u32, 0, 0, 0])),
                (ids[2], Word::from([30u32, 0, 0, 0])),
            ],
            vec![
                (ids[0], Word::from([10u32, 0, 0, 0])),
                (ids[1], Word::from([200u32, 0, 0, 0])),
                (ids[2], Word::from([30u32, 0, 0, 0])),
            ],
        ];

        let trees: Vec<_> =
            states.iter().map(|s| AccountTree::with_entries(s.clone()).unwrap()).collect();

        let hist = AccountTreeWithHistory::new(trees[0].clone(), BlockNumber::GENESIS);
        hist.apply_mutations([(ids[0], states[1][0].1), (ids[2], states[1][2].1)])
            .unwrap();
        hist.apply_mutations([(ids[1], states[2][1].1)]).unwrap();

        for (block, tree) in trees.iter().enumerate() {
            let hist_root = hist.root_at(BlockNumber::from(block as u32)).unwrap();
            assert_eq!(hist_root, tree.root());

            for &id in &ids {
                let hist_witness = hist.open_at(id, BlockNumber::from(block as u32)).unwrap();
                let actual = tree.open(id);
                assert_eq!(hist_witness.state_commitment(), actual.state_commitment());
                assert_eq!(hist_witness.path(), actual.path());
            }
        }
    }

    #[test]
    fn test_history_limits() {
        let id = AccountIdBuilder::new().build_with_seed([30; 32]);
        let tree = AccountTree::with_entries([(id, Word::from([1u32, 0, 0, 0]))]).unwrap();
        let hist = AccountTreeWithHistory::new(tree, BlockNumber::GENESIS);

        for i in 1..=(AccountTreeWithHistory::MAX_HISTORY + 5) {
            hist.apply_mutations([(id, Word::from([i as u32, 0, 0, 0]))]).unwrap();
        }

        let current = hist.block_number();
        assert!(hist.open_at(id, current).is_some());
        assert!(hist.open_at(id, BlockNumber::GENESIS).is_none());
        assert!(hist.open_at(id, current.child()).is_none());
    }

    #[test]
    fn test_account_lifecycle() {
        let id0 = AccountIdBuilder::new().build_with_seed([40; 32]);
        let id1 = AccountIdBuilder::new().build_with_seed([41; 32]);
        let v0 = Word::from([0u32; 4]);
        let v1 = Word::from([1u32; 4]);

        let start = &[(id0, v0)];
        let delta1 = &[(id1, v1)];
        let delta2 = &[(id0, Word::default())];

        let tree0 = AccountTree::with_entries(start.to_vec()).unwrap();
        let tree1 = {
            let mut tree1 = tree0.clone();
            let mutations = tree1.compute_mutations(delta1.to_vec()).unwrap();
            tree1.apply_mutations(mutations).unwrap();
            tree1
        };
        let tree2 = {
            let mut tree2 = tree1.clone();
            let mutations = tree2.compute_mutations(delta2.to_vec()).unwrap();
            tree2.apply_mutations(mutations).unwrap();
            tree2
        };

        let hist = AccountTreeWithHistory::new(tree0.clone(), BlockNumber::GENESIS);
        hist.apply_mutations([(id1, v1)]).unwrap();
        hist.apply_mutations([(id0, Word::default())]).unwrap();

        assert_eq!(hist.block_number(), BlockNumber::from(2));

        assert_eq!(hist.open_at(id0, BlockNumber::GENESIS).unwrap(), tree0.open(id0));
        assert_eq!(hist.open_at(id0, BlockNumber::from(1)).unwrap(), tree1.open(id0));
        assert_eq!(hist.open_at(id0, BlockNumber::from(2)).unwrap(), tree2.open(id0));

        assert_eq!(hist.open_at(id1, BlockNumber::GENESIS).unwrap(), tree0.open(id1));
        assert_eq!(hist.open_at(id1, BlockNumber::from(1)).unwrap(), tree1.open(id1));
        assert_eq!(hist.open_at(id1, BlockNumber::from(2)).unwrap(), tree2.open(id1));

        assert_eq!(hist.open_at(id0, BlockNumber::GENESIS).unwrap().state_commitment(), v0);
        assert_eq!(hist.open_at(id0, BlockNumber::from(1)).unwrap().state_commitment(), v0);
        assert_eq!(hist.open_at(id1, BlockNumber::from(1)).unwrap().state_commitment(), v1);
        assert_eq!(
            hist.open_at(id0, BlockNumber::from(2)).unwrap().state_commitment(),
            Word::default()
        );
        assert_eq!(hist.open_at(id1, BlockNumber::from(2)).unwrap().state_commitment(), v1);
    }
}
