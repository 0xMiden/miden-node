use std::collections::BTreeSet;

use miden_objects::{
    account::delta::AccountUpdateDetails,
    block::BlockHeader,
    note::Nullifier,
    transaction::TransactionId,
    utils::{Deserializable, Serializable},
};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated::block_producer::{
        BlockCommitted as ProtoBlockCommitted, MempoolEvent as ProtoMempoolEvent,
        TransactionAdded as ProtoTransactionAdded,
        TransactionsReverted as ProtoTransactionsReverted, mempool_event,
    },
};

use super::note::NetworkNote;

#[derive(Debug, Clone)]
pub enum MempoolEvent {
    TransactionAdded {
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<NetworkNote>,
        account_delta: Option<AccountUpdateDetails>,
    },
    BlockCommitted {
        header: BlockHeader,
        txs: Vec<TransactionId>,
    },
    TransactionsReverted(BTreeSet<TransactionId>),
}

impl From<MempoolEvent> for ProtoMempoolEvent {
    fn from(event: MempoolEvent) -> Self {
        let event = match event {
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                let event = ProtoTransactionAdded {
                    id: Some(id.into()),
                    nullifiers: nullifiers.into_iter().map(Into::into).collect(),
                    network_notes: network_notes.into_iter().map(Into::into).collect(),
                    network_account_delta: account_delta
                        .as_ref()
                        .map(AccountUpdateDetails::to_bytes),
                };

                mempool_event::Event::TransactionAdded(event)
            },
            MempoolEvent::BlockCommitted { header, txs } => {
                mempool_event::Event::BlockCommitted(ProtoBlockCommitted {
                    block_header: Some(header.into()),
                    transactions: txs.into_iter().map(Into::into).collect(),
                })
            },
            MempoolEvent::TransactionsReverted(txs) => {
                mempool_event::Event::TransactionsReverted(ProtoTransactionsReverted {
                    reverted: txs.into_iter().map(Into::into).collect(),
                })
            },
        }
        .into();

        Self { event }
    }
}

impl TryFrom<ProtoMempoolEvent> for MempoolEvent {
    type Error = ConversionError;

    fn try_from(event: ProtoMempoolEvent) -> Result<Self, Self::Error> {
        let event = event.event.ok_or(ProtoMempoolEvent::missing_field("event"))?;

        match event {
            mempool_event::Event::TransactionAdded(tx) => {
                let id = tx.id.ok_or(ProtoTransactionAdded::missing_field("id"))?.try_into()?;
                let nullifiers =
                    tx.nullifiers.into_iter().map(TryInto::try_into).collect::<Result<_, _>>()?;
                let network_notes = tx
                    .network_notes
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<_, _>>()?;
                let account_delta = tx
                    .network_account_delta
                    .as_deref()
                    .map(AccountUpdateDetails::read_from_bytes)
                    .transpose()
                    .map_err(|err| ConversionError::deserialization_error("account_delta", err))?;

                Ok(Self::TransactionAdded {
                    id,
                    nullifiers,
                    network_notes,
                    account_delta,
                })
            },
            mempool_event::Event::BlockCommitted(block_committed) => {
                let header = block_committed
                    .block_header
                    .ok_or(ProtoBlockCommitted::missing_field("block_header"))?
                    .try_into()?;
                let txs = block_committed
                    .transactions
                    .into_iter()
                    .map(TransactionId::try_from)
                    .collect::<Result<_, _>>()?;

                Ok(Self::BlockCommitted { header, txs })
            },
            mempool_event::Event::TransactionsReverted(txs) => {
                let txs = txs
                    .reverted
                    .into_iter()
                    .map(TransactionId::try_from)
                    .collect::<Result<_, _>>()?;

                Ok(Self::TransactionsReverted(txs))
            },
        }
    }
}
