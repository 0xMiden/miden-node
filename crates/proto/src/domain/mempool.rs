use std::collections::BTreeSet;

use miden_objects::{
    account::delta::AccountUpdateDetails, block::BlockHeader, note::Nullifier,
    transaction::TransactionId,
};

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
        // TODO: decide whether to create a new type to exclude private variant which isn't possible.
        account_delta: Option<AccountUpdateDetails>,
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
