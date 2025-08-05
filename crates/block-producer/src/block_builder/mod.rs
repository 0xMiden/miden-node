use std::ops::Range;

use futures::{FutureExt, never::Never};
use miden_block_prover::LocalBlockProver;
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::{
    MIN_PROOF_SECURITY_LEVEL,
    batch::ProvenBatch,
    block::{BlockInputs, BlockNumber, ProposedBlock, ProvenBlock},
    note::NoteHeader,
};
use miden_remote_prover_client::remote_prover::block_prover::RemoteBlockProver;
use rand::Rng;
use tokio::time::Duration;
use tracing::{Span, info, instrument};
use url::Url;

use crate::{
    COMPONENT,
    TelemetryInjectorExt,
    errors::BuildBlockError,
    mempool::SharedMempool,
    store::StoreClient,
};

// BLOCK BUILDER
// =================================================================================================

pub struct BlockBuilder {
    pub block_interval: Duration,
    /// Used to simulate block proving by sleeping for a random duration selected from this range.
    pub simulated_proof_time: Range<Duration>,

    /// Simulated block failure rate as a percentage.
    ///
    /// Note: this _must_ be sign positive and less than 1.0.
    pub failure_rate: f64,

    pub store: StoreClient,

    /// The prover used to prove a proposed block into a proven block.
    pub block_prover: BlockProver,
}

impl BlockBuilder {
    /// Creates a new [`BlockBuilder`] with the given [`StoreClient`] and optional block prover URL.
    ///
    /// If the block prover URL is not set, the block builder will use the local block prover.
    pub fn new(
        store: StoreClient,
        block_prover_url: Option<Url>,
        block_interval: Duration,
    ) -> Self {
        let block_prover = match block_prover_url {
            Some(url) => BlockProver::new_remote(url),
            None => BlockProver::new_local(MIN_PROOF_SECURITY_LEVEL),
        };

        Self {
            block_interval,
            // Note: The range cannot be empty.
            simulated_proof_time: Duration::ZERO..Duration::from_millis(1),
            failure_rate: 0.0,
            block_prover,
            store,
        }
    }
    /// Starts the [`BlockBuilder`], infinitely producing blocks at the configured interval.
    ///
    /// Block production is sequential and consists of
    ///
    ///   1. Pulling the next set of batches from the mempool
    ///   2. Compiling these batches into the next block
    ///   3. Proving the block (this is simulated using random sleeps)
    ///   4. Committing the block to the store
    pub async fn run(self, mempool: SharedMempool) {
        assert!(
            self.failure_rate < 1.0 && self.failure_rate.is_sign_positive(),
            "Failure rate must be a percentage"
        );

        let mut interval = tokio::time::interval(self.block_interval);
        // We set the interval's missed tick behaviour to burst. This means we'll catch up missed
        // blocks as fast as possible. In other words, we try our best to keep the desired block
        // interval on average. The other options would result in at least one skipped block.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);

        loop {
            interval.tick().await;

            // Errors are handled internally by the block building process.
            self.build_block(&mempool).await;
        }
    }

    /// Run the block building stages and add open-telemetry trace information where applicable.
    ///
    /// A failure in any stage will result in that block being rolled back.
    ///
    /// ## Telemetry
    ///
    /// - Creates a new root span which means each block gets its own complete trace.
    /// - Important telemetry fields are added to the root span with the `block.xxx` prefix.
    /// - Each stage has its own child span and are free to add further field data.
    /// - A failed stage will emit an error event, and both its own span and the root span will be
    ///   marked as errors.
    #[instrument(parent = None, target = COMPONENT, name = "block_builder.build_block", skip_all)]
    async fn build_block(&self, mempool: &SharedMempool) {
        use futures::TryFutureExt;

        Self::select_block(mempool)
            .inspect(SelectedBlock::inject_telemetry)
            .then(|selected| self.get_block_inputs(selected))
            .inspect_ok(BlockBatchesAndInputs::inject_telemetry)
            .and_then(|inputs| self.propose_block(inputs))
            .inspect_ok(ProposedBlock::inject_telemetry)
            .and_then(|inputs| self.prove_block(inputs))
            .inspect_ok(ProvenBlock::inject_telemetry)
            // Failure must be injected before the final pipeline stage i.e. before commit is called. The system cannot
            // handle errors after it considers the process complete (which makes sense).
            .and_then(|proven_block| async { self.inject_failure(proven_block) })
            .and_then(|proven_block| self.commit_block(mempool, proven_block))
            // Handle errors by propagating the error to the root span and rolling back the block.
            .inspect_err(|err| Span::current().set_error(err))
            .or_else(|_err| self.rollback_block(mempool).never_error())
            // All errors were handled and discarded above, so this is just type juggling
            // to drop the result.
            .unwrap_or_else(|_: Never| ())
            .await
    }

    #[instrument(target = COMPONENT, name = "block_builder.select_block", skip_all)]
    async fn select_block(mempool: &SharedMempool) -> SelectedBlock {
        let (block_number, batches) = mempool.lock().await.select_block();
        SelectedBlock { block_number, batches }
    }

    /// Fetches block inputs from the store for the [`SelectedBlock`].
    ///
    /// For a given set of batches, we need to get the following block inputs from the store:
    ///
    /// - Note inclusion proofs for unauthenticated notes (not required to be complete due to the
    ///   possibility of note erasure)
    /// - A chain MMR with:
    ///   - All blocks referenced by batches
    ///   - All blocks referenced by note inclusion proofs
    /// - Account witnesses for all accounts updated in the block
    /// - Nullifier witnesses for all nullifiers created in the block
    ///   - Due to note erasure the set of nullifiers the block creates it not necessarily equal to
    ///     the union of all nullifiers created in proven batches. However, since we don't yet know
    ///     which nullifiers the block will actually create, we fetch witnesses for all nullifiers
    ///     created by batches. If we knew that a certain note will be erased, we would not have to
    ///     supply a nullifier witness for it.
    #[instrument(target = COMPONENT, name = "block_builder.get_block_inputs", skip_all, err)]
    async fn get_block_inputs(
        &self,
        selected_block: SelectedBlock,
    ) -> Result<BlockBatchesAndInputs, BuildBlockError> {
        let SelectedBlock { block_number: _, batches } = selected_block;

        let batch_iter = batches.iter();

        let unauthenticated_notes_iter = batch_iter.clone().flat_map(|batch| {
            // Note: .cloned() shouldn't be necessary but not having it produces an odd lifetime
            // error in BlockProducer::serve. Not sure if there's a better fix. Error:
            // implementation of `FnOnce` is not general enough
            // closure with signature `fn(&InputNoteCommitment) -> miden_objects::note::NoteId` must
            // implement `FnOnce<(&InputNoteCommitment,)>` ...but it actually implements
            // `FnOnce<(&InputNoteCommitment,)>`
            batch
                .input_notes()
                .iter()
                .cloned()
                .filter_map(|note| note.header().map(NoteHeader::id))
        });
        let block_references_iter = batch_iter.clone().map(ProvenBatch::reference_block_num);
        let account_ids_iter = batch_iter.clone().flat_map(ProvenBatch::updated_accounts);
        let created_nullifiers_iter = batch_iter.flat_map(ProvenBatch::created_nullifiers);

        let inputs = self
            .store
            .get_block_inputs(
                account_ids_iter,
                created_nullifiers_iter,
                unauthenticated_notes_iter,
                block_references_iter,
            )
            .await
            .map_err(BuildBlockError::GetBlockInputsFailed)?;

        Ok(BlockBatchesAndInputs { batches, inputs })
    }

    #[instrument(target = COMPONENT, name = "block_builder.propose_block", skip_all, err)]
    async fn propose_block(
        &self,
        batches_inputs: BlockBatchesAndInputs,
    ) -> Result<ProposedBlock, BuildBlockError> {
        let BlockBatchesAndInputs { batches, inputs } = batches_inputs;

        let proposed_block =
            ProposedBlock::new(inputs, batches).map_err(BuildBlockError::ProposeBlockFailed)?;

        Ok(proposed_block)
    }

    #[instrument(target = COMPONENT, name = "block_builder.prove_block", skip_all, err)]
    async fn prove_block(
        &self,
        proposed_block: ProposedBlock,
    ) -> Result<ProvenBlock, BuildBlockError> {
        let proven_block = self.block_prover.prove(proposed_block).await?;

        if proven_block.proof_security_level() < MIN_PROOF_SECURITY_LEVEL {
            return Err(BuildBlockError::SecurityLevelTooLow(
                proven_block.proof_security_level(),
                MIN_PROOF_SECURITY_LEVEL,
            ));
        }

        self.simulate_proving().await;

        Ok(proven_block)
    }

    #[instrument(target = COMPONENT, name = "block_builder.commit_block", skip_all, err)]
    async fn commit_block(
        &self,
        mempool: &SharedMempool,
        built_block: ProvenBlock,
    ) -> Result<(), BuildBlockError> {
        self.store
            .apply_block(&built_block)
            .await
            .map_err(BuildBlockError::StoreApplyBlockFailed)?;

        mempool.lock().await.commit_block(built_block.header().clone());

        Ok(())
    }

    #[instrument(target = COMPONENT, name = "block_builder.rollback_block", skip_all)]
    async fn rollback_block(&self, mempool: &SharedMempool) {
        mempool.lock().await.rollback_block();
    }

    #[instrument(target = COMPONENT, name = "block_builder.simulate_proving", skip_all)]
    async fn simulate_proving(&self) {
        let proving_duration = rand::rng().random_range(self.simulated_proof_time.clone());

        Span::current().set_attribute("range.min_s", self.simulated_proof_time.start);
        Span::current().set_attribute("range.max_s", self.simulated_proof_time.end);
        Span::current().set_attribute("dice_roll_s", proving_duration);

        tokio::time::sleep(proving_duration).await;
    }

    #[instrument(target = COMPONENT, name = "block_builder.inject_failure", skip_all, err)]
    fn inject_failure<T>(&self, value: T) -> Result<T, BuildBlockError> {
        let roll = rand::rng().random::<f64>();

        Span::current().set_attribute("failure_rate", self.failure_rate);
        Span::current().set_attribute("dice_roll", roll);

        if roll < self.failure_rate {
            Err(BuildBlockError::InjectedFailure)
        } else {
            Ok(value)
        }
    }
}

/// A wrapper around batches selected for inlucion in a block, primarily used to be able to inject
/// telemetry in-between the selection and fetching the required [`BlockInputs`].
struct SelectedBlock {
    block_number: BlockNumber,
    batches: Vec<ProvenBatch>,
}

impl TelemetryInjectorExt for SelectedBlock {
    fn inject_telemetry(&self) {
        let span = Span::current();
        span.set_attribute("block.number", self.block_number);
        span.set_attribute("block.batches.count", self.batches.len() as u32);
        let tx_count = self
            .batches
            .iter()
            .fold(0, |acc, batch| acc + batch.transactions().as_slice().len());
        span.set_attribute("block.transactions.count", tx_count);
    }
}

/// A wrapper around the inputs needed to build a [`ProposedBlock`], primarily used to be able to
/// inject telemetry in-between fetching block inputs and proposing the block.
struct BlockBatchesAndInputs {
    batches: Vec<ProvenBatch>,
    inputs: BlockInputs,
}

impl TelemetryInjectorExt for BlockBatchesAndInputs {
    fn inject_telemetry(&self) {
        let span = Span::current();

        // SAFETY: We do not expect to have more than u32::MAX of any count per block.
        span.set_attribute(
            "block.updated_accounts.count",
            i64::try_from(self.inputs.account_witnesses().len())
                .expect("less than u32::MAX account updates"),
        );
        span.set_attribute(
            "block.erased_note_proofs.count",
            i64::try_from(self.inputs.unauthenticated_note_proofs().len())
                .expect("less than u32::MAX unauthenticated notes"),
        );
    }
}

impl TelemetryInjectorExt for ProposedBlock {
    /// Emit the input and output note related attributes. We do this here since this is the
    /// earliest point we can set attributes after note erasure was done.
    fn inject_telemetry(&self) {
        let span = Span::current();

        span.set_attribute("block.nullifiers.count", self.created_nullifiers().len());

        let num_block_created_notes: usize = self.output_note_batches().iter().map(Vec::len).sum();
        span.set_attribute("block.output_notes.count", num_block_created_notes);

        let num_batch_created_notes = self.batches().num_created_notes();
        span.set_attribute("block.batches.output_notes.count", num_batch_created_notes);

        let num_erased_notes = num_batch_created_notes
            .checked_sub(num_block_created_notes)
            .expect("all batches in the block should not create fewer notes than the block itself");
        span.set_attribute("block.erased_notes.count", num_erased_notes);
    }
}

impl TelemetryInjectorExt for ProvenBlock {
    fn inject_telemetry(&self) {
        let span = Span::current();
        let header = self.header();

        span.set_attribute("block.commitment", header.commitment());
        span.set_attribute("block.sub_commitment", header.sub_commitment());
        span.set_attribute("block.prev_block_commitment", header.prev_block_commitment());
        span.set_attribute("block.timestamp", header.timestamp());

        span.set_attribute("block.protocol.version", i64::from(header.version()));

        span.set_attribute("block.commitments.kernel", header.tx_kernel_commitment());
        span.set_attribute("block.commitments.nullifier", header.nullifier_root());
        span.set_attribute("block.commitments.account", header.account_root());
        span.set_attribute("block.commitments.chain", header.chain_commitment());
        span.set_attribute("block.commitments.note", header.note_root());
        span.set_attribute("block.commitments.transaction", header.tx_commitment());
    }
}

// BLOCK PROVER
// ================================================================================================

pub enum BlockProver {
    Local(LocalBlockProver),
    Remote(RemoteBlockProver),
}

impl BlockProver {
    pub fn new_local(security_level: u32) -> Self {
        info!(target: COMPONENT, "Using local block prover");
        Self::Local(LocalBlockProver::new(security_level))
    }

    pub fn new_remote(endpoint: impl Into<String>) -> Self {
        info!(target: COMPONENT, "Using remote block prover");
        Self::Remote(RemoteBlockProver::new(endpoint))
    }

    #[instrument(target = COMPONENT, skip_all, err)]
    async fn prove(&self, proposed_block: ProposedBlock) -> Result<ProvenBlock, BuildBlockError> {
        match self {
            Self::Local(prover) => {
                prover.prove(proposed_block).map_err(BuildBlockError::ProveBlockFailed)
            },
            Self::Remote(prover) => prover
                .prove(proposed_block)
                .await
                .map_err(BuildBlockError::RemoteProverClientError),
        }
    }
}
