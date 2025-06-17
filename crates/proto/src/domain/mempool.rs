use std::collections::BTreeSet;

use miden_objects::{block::BlockHeader, note::Nullifier, transaction::TransactionId};

use crate::{
    errors::ConversionError, generated::block_producer::MempoolEvent as ProtoMempoolEvent,
};

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

impl From<MempoolEvent> for ProtoMempoolEvent {
    fn from(value: MempoolEvent) -> Self {
        todo!()
    }
}

impl TryFrom<ProtoMempoolEvent> for MempoolEvent {
    type Error = ConversionError;

    fn try_from(value: ProtoMempoolEvent) -> Result<Self, Self::Error> {
        todo!()
    }
}
