use std::collections::BTreeSet;

use miden_objects::{
    account::delta::AccountUpdateDetails,
    block::BlockHeader,
    note::Nullifier,
    transaction::TransactionId,
    utils::{Deserializable, Serializable},
};

use super::note::NetworkNote;
use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated as proto,
};

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

impl MempoolEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            MempoolEvent::TransactionAdded { .. } => "TransactionAdded",
            MempoolEvent::BlockCommitted { .. } => "BlockCommitted",
            MempoolEvent::TransactionsReverted(_) => "TransactionsReverted",
        }
    }
}

impl From<MempoolEvent> for proto::block_producer::MempoolEvent {
    fn from(event: MempoolEvent) -> Self {
        let event = match event {
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                let event = proto::block_producer::mempool_event::TransactionAdded {
                    id: Some(id.into()),
                    nullifiers: nullifiers.into_iter().map(Into::into).collect(),
                    network_notes: network_notes.into_iter().map(Into::into).collect(),
                    network_account_delta: account_delta
                        .as_ref()
                        .map(AccountUpdateDetails::to_bytes),
                };

                proto::block_producer::mempool_event::Event::TransactionAdded(event)
            },
            MempoolEvent::BlockCommitted { header, txs } => {
                proto::block_producer::mempool_event::Event::BlockCommitted(
                    proto::block_producer::mempool_event::BlockCommitted {
                        block_header: Some(header.into()),
                        transactions: txs.into_iter().map(Into::into).collect(),
                    },
                )
            },
            MempoolEvent::TransactionsReverted(txs) => {
                proto::block_producer::mempool_event::Event::TransactionsReverted(
                    proto::block_producer::mempool_event::TransactionsReverted {
                        reverted: txs.into_iter().map(Into::into).collect(),
                    },
                )
            },
        }
        .into();

        Self { event }
    }
}

impl TryFrom<proto::block_producer::MempoolEvent> for MempoolEvent {
    type Error = ConversionError;

    fn try_from(event: proto::block_producer::MempoolEvent) -> Result<Self, Self::Error> {
        let event =
            event.event.ok_or(proto::block_producer::MempoolEvent::missing_field("event"))?;

        match event {
            proto::block_producer::mempool_event::Event::TransactionAdded(tx) => {
                let id = tx
                    .id
                    .ok_or(proto::block_producer::mempool_event::TransactionAdded::missing_field(
                        "id",
                    ))?
                    .try_into()?;
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
            proto::block_producer::mempool_event::Event::BlockCommitted(block_committed) => {
                let header = block_committed
                    .block_header
                    .ok_or(proto::block_producer::mempool_event::BlockCommitted::missing_field(
                        "block_header",
                    ))?
                    .try_into()?;
                let txs = block_committed
                    .transactions
                    .into_iter()
                    .map(TransactionId::try_from)
                    .collect::<Result<_, _>>()?;

                Ok(Self::BlockCommitted { header, txs })
            },
            proto::block_producer::mempool_event::Event::TransactionsReverted(txs) => {
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
