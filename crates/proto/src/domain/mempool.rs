use std::collections::BTreeSet;

use miden_objects::{block::BlockHeader, note::Nullifier, transaction::TransactionId};

use super::note::NetworkNote;

#[derive(Debug, Clone)]
pub enum MempoolEvent {
    TransactionAdded {
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<NetworkNote>,
    },
    BlockCommitted {
        header: BlockHeader,
        txs: Vec<TransactionId>,
    },
    TransactionsReverted(BTreeSet<TransactionId>),
}

impl From<MempoolEvent> for crate::generated::block_producer::MempoolEvent {
    fn from(value: MempoolEvent) -> Self {
        todo!()
    }
}
