use std::collections::{BTreeSet, HashMap, hash_map::Entry};

use miden_objects::{block::BlockNumber, transaction::TransactionId};

/// Tracks transactions and their expiration block heights.
///
/// Implemented as a bi-directional map internally to allow for efficient lookups via both
/// transaction ID and block number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransactionExpirations {
    /// Transaction lookup index.
    by_tx: HashMap<TransactionId, BlockNumber>,
    /// Block number lookup index.
    by_block: HashMap<BlockNumber, BTreeSet<TransactionId>>,
}

impl TransactionExpirations {
    /// Add the transaction to the tracker.
    pub fn insert(&mut self, tx: TransactionId, block: BlockNumber) {
        self.by_tx.insert(tx, block);
        self.by_block.entry(block).or_default().insert(tx);
    }

    /// Returns all transactions that are expiring at the given block number.
    pub fn get(&mut self, block: BlockNumber) -> BTreeSet<TransactionId> {
        self.by_block.get(&block).cloned().unwrap_or_default()
    }

    /// Removes the transactions from the tracker.
    ///
    /// Unknown transactions are ignored.
    pub fn remove<'a>(&mut self, txs: impl Iterator<Item = &'a TransactionId>) {
        for tx in txs {
            if let Some(block) = self.by_tx.remove(tx) {
                let Entry::Occupied(entry) = self.by_block.entry(block).and_modify(|x| {
                    x.remove(tx);
                }) else {
                    panic!("block entry must exist as this is a bidirectional mapping");
                };

                // Prune the entire block's entry if no transactions are tracked.
                if entry.get().is_empty() {
                    entry.remove();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::Random;

    /// Removing a transaction may result in a block's mapping being empty. This test ensures that
    /// such maps are pruned to prevent endless growth.
    #[test]
    fn remove_prunes_empty_block_maps() {
        let tx = Random::with_random_seed().draw_tx_id();
        let block = BlockNumber::from(123);

        let mut uut = TransactionExpirations::default();
        uut.insert(tx, block);
        uut.remove(std::iter::once(&tx));

        assert_eq!(uut, TransactionExpirations::default());
    }

    #[test]
    fn get_empty() {
        assert!(TransactionExpirations::default().get(BlockNumber::from(123)).is_empty());
    }
}
