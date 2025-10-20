use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteHeader, NoteId, Nullifier};
use miden_objects::transaction::{InputNoteCommitment, TransactionHeader, TransactionId};

use crate::domain::transaction::AuthenticatedTransaction;

/// Uniquely identifies a node in the mempool.
///
/// This effectively describes the lifecycle of a transaction in the mempool.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum NodeId {
    Transaction(TransactionId),
    // UserBatch(BatchId),
    ProposedBatch(BatchId),
    ProvenBatch(BatchId),
    Block(BlockNumber),
}

/// A node representing a [`AuthenticatedTransaction`] which is waiting for inclusion in a batch.
///
/// Once this is selected for inclusion in a batch it will be moved inside a [`ProposedBatchNode`].
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TransactionNode(Arc<AuthenticatedTransaction>);

impl TransactionNode {
    pub(super) fn new(inner: Arc<AuthenticatedTransaction>) -> Self {
        Self(inner)
    }

    pub(super) fn id(&self) -> TransactionId {
        self.0.id()
    }

    pub(super) fn inner(&self) -> &Arc<AuthenticatedTransaction> {
        &self.0
    }

    pub(super) fn expires_at(&self) -> BlockNumber {
        self.0.expires_at()
    }
}

/// Represents a batch which has been proposed by the mempool and which is undergoing proving.
///
/// Once proven it transitions to a [`ProvenBatchNode`].
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct ProposedBatchNode(Vec<Arc<AuthenticatedTransaction>>);

impl ProposedBatchNode {
    pub(super) fn push(&mut self, tx: Arc<AuthenticatedTransaction>) {
        self.0.push(tx);
    }

    pub(super) fn contains(&mut self, id: TransactionId) -> bool {
        self.0.iter().any(|tx| tx.id() == id)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn calculate_id(&self) -> BatchId {
        BatchId::from_transactions(
            self.0
                .iter()
                .map(AsRef::as_ref)
                .map(AuthenticatedTransaction::raw_proven_transaction),
        )
    }

    pub(super) fn into_proven_batch_node(self, proof: Arc<ProvenBatch>) -> ProvenBatchNode {
        let Self(txs) = self;
        ProvenBatchNode { txs, inner: proof }
    }

    pub(super) fn expires_at(&self) -> BlockNumber {
        self.0.iter().map(|tx| tx.expires_at()).min().unwrap_or_default()
    }
}

/// Represents a [`ProvenBatch`] which is waiting for inclusion in a block.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ProvenBatchNode {
    /// We need to store this in addition to the proven batch because [`ProvenBatch`] erases the
    /// transaction information. We need the original information if we want to rollback the batch
    /// but retain the transactions.
    txs: Vec<Arc<AuthenticatedTransaction>>,
    inner: Arc<ProvenBatch>,
}

impl ProvenBatchNode {
    pub(super) fn tx_headers(&self) -> impl Iterator<Item = &TransactionHeader> {
        self.inner.transactions().as_slice().iter()
    }

    pub(super) fn id(&self) -> BatchId {
        self.inner.id()
    }

    pub(super) fn inner(&self) -> &Arc<ProvenBatch> {
        &self.inner
    }

    pub(super) fn expires_at(&self) -> BlockNumber {
        self.inner.batch_expiration_block_num()
    }
}

/// Represents a block - both committed and in-progress.
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct BlockNode {
    txs: Vec<Arc<AuthenticatedTransaction>>,
    batches: Vec<Arc<ProvenBatch>>,
}

impl BlockNode {
    pub(super) fn push(&mut self, batch: ProvenBatchNode) {
        let ProvenBatchNode { txs, inner } = batch;
        self.txs.extend(txs);
        self.batches.push(inner);
    }

    pub(super) fn contains(&self, id: BatchId) -> bool {
        self.batches.iter().any(|batch| batch.id() == id)
    }

    pub(super) fn batches(&self) -> &[Arc<ProvenBatch>] {
        &self.batches
    }
}

/// Describes a node's impact on the state.
///
/// This is used to determine what state data is created or consumed by this node.
pub(super) trait Node {
    /// All [`Nullifier`]s created by this node, **including** nullifiers for erased notes. This
    /// may not be strictly necessary but it removes having to worry about reverting batches and
    /// blocks with erased notes -- since these would otherwise have different state impact than
    /// the transactions within them.
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_>;

    /// All output notes created by this node, **including** erased notes. This may not be strictly
    /// necessary but it removes having to worry about reverting batches and blocks with erased
    /// notes -- since these would otherwise have different state impact than the transactions
    /// within them.
    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_>;
    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_>;
    /// The account state commitment updates caused by this node.
    ///
    /// Output tuple represents each updates `(account ID, initial commitment, final commitment)`.
    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_>;
    fn transactions(&self) -> Box<dyn Iterator<Item = &Arc<AuthenticatedTransaction>> + '_>;
}

impl Node for TransactionNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.nullifiers())
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.output_note_ids())
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.unauthenticated_notes())
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        let update = self.0.account_update();
        Box::new(std::iter::once((
            update.account_id(),
            update.initial_state_commitment(),
            update.final_state_commitment(),
        )))
    }

    fn transactions(&self) -> Box<dyn Iterator<Item = &Arc<AuthenticatedTransaction>> + '_> {
        Box::new(std::iter::once(&self.0))
    }
}

impl Node for ProposedBatchNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.iter().flat_map(|tx| tx.nullifiers()))
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(|tx| tx.output_note_ids()))
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(|tx| tx.unauthenticated_notes()))
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        Box::new(self.0.iter().flat_map(|tx| {
            let update = tx.account_update();
            std::iter::once((
                update.account_id(),
                update.initial_state_commitment(),
                update.final_state_commitment(),
            ))
        }))
    }

    fn transactions(&self) -> Box<dyn Iterator<Item = &Arc<AuthenticatedTransaction>> + '_> {
        Box::new(self.0.iter())
    }
}

impl Node for ProvenBatchNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(
            self.tx_headers()
                .flat_map(|tx| tx.input_notes().iter().map(InputNoteCommitment::nullifier)),
        )
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.tx_headers().flat_map(|tx| tx.output_notes().iter().map(NoteHeader::id)))
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(
            self.inner
                .input_notes()
                .iter()
                .filter_map(|note| note.header())
                .map(NoteHeader::id),
        )
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        Box::new(self.inner.account_updates().values().map(|update| {
            (
                update.account_id(),
                update.initial_state_commitment(),
                update.final_state_commitment(),
            )
        }))
    }

    fn transactions(&self) -> Box<dyn Iterator<Item = &Arc<AuthenticatedTransaction>> + '_> {
        Box::new(self.txs.iter())
    }
}

impl Node for BlockNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.txs.iter().flat_map(|tx| tx.nullifiers()))
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.txs.iter().flat_map(|tx| tx.output_note_ids()))
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.batches.iter().flat_map(|batch| {
            batch.input_notes().iter().filter_map(|note| note.header().map(NoteHeader::id))
        }))
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        Box::new(self.batches.iter().flat_map(|batch| batch.account_updates()).map(
            |(_, update)| {
                (
                    update.account_id(),
                    update.initial_state_commitment(),
                    update.final_state_commitment(),
                )
            },
        ))
    }

    fn transactions(&self) -> Box<dyn Iterator<Item = &Arc<AuthenticatedTransaction>> + '_> {
        Box::new(self.txs.iter())
    }
}

/// Contains the current nodes of the state DAG.
///
/// Nodes are purposefully not stored as a single collection since we often want to iterate
/// through specific node types e.g. all available transactions.
///
/// This data _must_ be kept in sync with the [`InflightState's`] [`NodeIds`] since these are
/// used as the edges of the graph.
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct Nodes {
    // Nodes in the DAG
    pub(super) txs: HashMap<TransactionId, TransactionNode>,
    // user_batches: HashMap<BatchId, ProvenBatchNode>,
    pub(super) proposed_batches: HashMap<BatchId, ProposedBatchNode>,
    pub(super) proven_batches: HashMap<BatchId, ProvenBatchNode>,
    pub(super) proposed_block: Option<(BlockNumber, BlockNode)>,
    pub(super) committed_blocks: VecDeque<(BlockNumber, BlockNode)>,
}

impl Nodes {
    pub(super) fn oldest_committed_block(&self) -> Option<BlockNumber> {
        self.committed_blocks.front().map(|(number, _)| *number)
    }

    pub(super) fn inject_telemetry(&self, span: &tracing::Span) {
        use miden_node_utils::tracing::OpenTelemetrySpanExt;

        span.set_attribute("mempool.transactions.unbatched", self.txs.len());
        span.set_attribute("mempool.batches.proposed", self.proposed_batches.len());
        span.set_attribute("mempool.batches.proven", self.proven_batches.len());
    }
}
