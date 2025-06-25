use miden_objects::transaction::{ExecutedTransaction, TransactionWitness};
use miden_proving_service_client::proving_service::tx_prover::RemoteTransactionProver;
use miden_tx::{LocalTransactionProver, TransactionProver};
use url::Url;

use crate::block_producer::BlockProducerClient;
use crate::builder::NtxBuilderError;

// TRANSACTION PROVER
// ================================================================================================

pub enum NtbTransactionProver {
    Local(LocalTransactionProver),
    Remote(RemoteTransactionProver),
}

impl NtbTransactionProver {
    /// Proves and submits the given executed transaction.
    pub async fn prove_and_submit(
        &self,
        block_producer_client: &BlockProducerClient,
        executed_tx: ExecutedTransaction,
    ) -> Result<(), NtxBuilderError> {
        let tx_witness = TransactionWitness::from(executed_tx);

        let proven_tx = match self {
            NtbTransactionProver::Local(prover) => prover.prove(tx_witness).await,
            NtbTransactionProver::Remote(prover) => prover.prove(tx_witness).await,
        }?;

        block_producer_client
            .submit_proven_transaction(proven_tx)
            .await
            .map_err(NtxBuilderError::ProofSubmissionFailed)
    }
}

impl From<Option<Url>> for NtbTransactionProver {
    fn from(url: Option<Url>) -> Self {
        if let Some(url) = url {
            let tx_prover = RemoteTransactionProver::new(url);
            NtbTransactionProver::Remote(tx_prover)
        } else {
            NtbTransactionProver::Local(LocalTransactionProver::default())
        }
    }
}
