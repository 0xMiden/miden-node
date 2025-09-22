//! Remote transaction prover test functionality.
//!
//! This module contains the logic for periodically testing remote transaction prover functionality
//! by sending mock transactions and checking for successful transaction proof generation.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use miden_node_proto::clients::{Builder as ClientBuilder, RemoteProverClient};
use miden_node_proto::generated as proto;
use miden_objects::asset::{Asset, FungibleAsset};
use miden_objects::batch::ProposedBatch;
use miden_objects::block::ProposedBlock;
use miden_objects::note::NoteType;
use miden_objects::testing::account_id::{ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET, ACCOUNT_ID_SENDER};
use miden_objects::transaction::TransactionWitness;
use miden_testing::{Auth, MockChainBuilder};
use miden_tx::utils::Serializable;
use miden_tx::{LocalTransactionProver, ProvingOptions};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tonic::Request;
use tracing::instrument;
use url::Url;

use crate::COMPONENT;
use crate::status::{ServiceDetails, ServiceStatus, Status};

// PROOF TYPE
// ================================================================================================

/// Remote prover types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofType {
    Transaction,
    Block,
    Batch,
}

impl From<ProofType> for proto::remote_prover::ProofType {
    fn from(value: ProofType) -> Self {
        match value {
            ProofType::Transaction => proto::remote_prover::ProofType::Transaction,
            ProofType::Block => proto::remote_prover::ProofType::Block,
            ProofType::Batch => proto::remote_prover::ProofType::Batch,
        }
    }
}

impl From<proto::remote_prover::ProofType> for ProofType {
    fn from(value: proto::remote_prover::ProofType) -> Self {
        match value {
            proto::remote_prover::ProofType::Transaction => ProofType::Transaction,
            proto::remote_prover::ProofType::Batch => ProofType::Batch,
            proto::remote_prover::ProofType::Block => ProofType::Block,
        }
    }
}

// REMOTE PROVER TEST TYPES
// ================================================================================================

/// Details of a remote transaction prover test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProverTestDetails {
    pub test_duration_ms: u64,
    pub proof_size_bytes: usize,
    pub success_count: u64,
    pub failure_count: u64,
    pub proof_type: ProofType,
}

// REMOTE TRANSACTION PROVER TEST TASK
// ================================================================================================

/// Runs a task that continuously tests remote prover functionality and updates a watch channel.
///
/// This function spawns a task that periodically sends mock request payloads to a remote prover
/// and measures the success/failure rate and performance metrics for proof generation.
///
/// # Arguments
///
/// * `prover_url` - The URL of the remote prover service to test.
/// * `name` - The name of the remote prover.
/// * `proof_type` - The type of proof to test.
/// * `serialized_request_payload` - The serialized request payload to send to the remote prover.
/// * `status_sender` - The sender for the watch channel.
///
/// # Returns
///
/// `Ok(())` if the task completes successfully, or an error if the task fails.
#[instrument(target = COMPONENT, name = "remote-prover-test-task", skip_all)]
pub async fn run_remote_prover_test_task(
    prover_url: Url,
    name: &str,
    proof_type: ProofType,
    serialized_request_payload: proto::remote_prover::ProofRequest,
    status_sender: watch::Sender<ServiceStatus>,
) -> anyhow::Result<()> {
    let mut client = ClientBuilder::new(prover_url)
        .with_tls()
        .expect("TLS is enabled")
        .with_timeout(Duration::from_secs(10))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<RemoteProverClient>();

    let mut interval = tokio::time::interval(Duration::from_secs(30)); // Test every 30 seconds
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut success_count = 0u64;
    let mut failure_count = 0u64;

    loop {
        interval.tick().await;

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("failed to get current time")?
            .as_secs();

        let status = test_remote_prover(
            &mut client,
            name,
            &proof_type,
            &serialized_request_payload,
            current_time,
            &mut success_count,
            &mut failure_count,
        )
        .await;

        // Send the status update (ignore if no receivers)
        let _ = status_sender.send(status);
    }
}

/// Tests the remote prover by sending a mock request payload.
///
/// This function sends a mock request payload to the remote prover and measures the response time
/// and success/failure rate for proof generation.
///
/// # Arguments
///
/// * `client` - The remote prover gRPC client.
/// * `name` - The name of the remote prover.
/// * `proof_type` - The type of proof to test.
/// * `serialized_request_payload` - The serialized request payload to send to the remote prover.
/// * `current_time` - The current time in seconds since UNIX epoch.
/// * `success_count` - Mutable reference to the success counter.
/// * `failure_count` - Mutable reference to the failure counter.
///
/// # Returns
///
/// A `ServiceStatus` containing the results of the proof test.
#[instrument(target = COMPONENT, name = "test-remote-prover", skip_all, ret(level = "info"))]
async fn test_remote_prover(
    client: &mut miden_node_proto::clients::RemoteProverClient,
    name: &str,
    proof_type: &ProofType,
    serialized_request_payload: &proto::remote_prover::ProofRequest,
    current_time: u64,
    success_count: &mut u64,
    failure_count: &mut u64,
) -> ServiceStatus {
    let start_time = std::time::Instant::now();

    // Create the proof request
    let request = Request::new(serialized_request_payload.clone());

    // Send the request and measure the time
    match client.prove(request).await {
        Ok(response) => {
            let duration = start_time.elapsed();
            let response_inner = response.into_inner();

            *success_count += 1;

            ServiceStatus {
                name: name.to_string(),
                status: Status::Healthy,
                last_checked: current_time,
                error: None,
                details: ServiceDetails::RemoteProverTest(ProverTestDetails {
                    test_duration_ms: duration.as_millis() as u64,
                    proof_size_bytes: response_inner.payload.len(),
                    success_count: *success_count,
                    failure_count: *failure_count,
                    proof_type: proof_type.clone(),
                }),
            }
        },
        Err(e) => {
            *failure_count += 1;

            ServiceStatus {
                name: name.to_string(),
                status: Status::Unhealthy,
                last_checked: current_time,
                error: Some(e.to_string()),
                details: ServiceDetails::RemoteProverTest(ProverTestDetails {
                    test_duration_ms: 0,
                    proof_size_bytes: 0,
                    success_count: *success_count,
                    failure_count: *failure_count,
                    proof_type: proof_type.clone(),
                }),
            }
        },
    }
}

// PROPOSED BLOCK GENERATOR
// ================================================================================================

pub fn generate_proposed_block() -> ProposedBlock {
    let mockchain = miden_testing::MockChain::new();
    mockchain.propose_block(vec![]).expect("Empty blocks are valid")
}

// PROPOSED BATCH GENERATOR
// ================================================================================================

pub async fn generate_proposed_batch() -> ProposedBatch {
    let mockchain = miden_testing::MockChain::new();

    let transaction = generate_mock_transaction().await.expect("Transaction wasn't generated");

    let proven_tx = LocalTransactionProver::new(ProvingOptions::default())
        .prove(transaction)
        .unwrap();

    mockchain.propose_transaction_batch(vec![proven_tx]).expect("Batch is valid")
}

// TRANSACTION WITNESS GENERATOR
// ================================================================================================

/// Generates a mock transaction for testing remote prover functionality.
///
/// This function creates a mock transaction using `MockChainBuilder` similar to what's done
/// in the remote prover tests. The transaction is generated once and can be reused for
/// multiple proof test calls.
pub async fn generate_mock_transaction() -> anyhow::Result<TransactionWitness> {
    let mut mock_chain_builder = MockChainBuilder::new();

    // Create an account with basic authentication
    let account = mock_chain_builder
        .add_existing_wallet(Auth::BasicAuth)
        .context("Failed to add wallet to mock chain")?;

    // Create a fungible asset
    let fungible_asset: Asset = FungibleAsset::new(
        ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET
            .try_into()
            .context("Failed to convert account ID")?,
        100,
    )
    .context("Failed to create fungible asset")?
    .into();

    // Create a P2ID note
    let note = mock_chain_builder
        .add_p2id_note(
            ACCOUNT_ID_SENDER.try_into().context("Failed to convert sender account ID")?,
            account.id(),
            &[fungible_asset],
            NoteType::Private,
        )
        .context("Failed to add P2ID note")?;

    // Build the mock chain
    let mock_chain = mock_chain_builder.build().context("Failed to build mock chain")?;

    // Build transaction context
    let tx_context = mock_chain
        .build_tx_context(account.id(), &[note.id()], &[])
        .context("Failed to build transaction context")?
        .build()
        .context("Failed to build transaction")?;

    // Execute the transaction
    let executed_transaction =
        Box::pin(tx_context.execute()).await.context("Failed to execute transaction")?;

    // Create transaction witness
    let transaction_witness = TransactionWitness::from(executed_transaction);

    Ok(transaction_witness)
}

// GENERATE TEST REQUEST PAYLOAD
// ================================================================================================

pub(crate) async fn generate_prover_test_payload(
    service_status: &ServiceStatus,
) -> proto::remote_prover::ProofRequest {
    match service_status.details.clone() {
        ServiceDetails::RemoteProverStatus(remote_prover_status_details) => {
            match remote_prover_status_details.supported_proof_type {
                ProofType::Transaction => proto::remote_prover::ProofRequest {
                    proof_type: proto::remote_prover::ProofType::Transaction.into(),
                    payload: generate_mock_transaction().await.unwrap().to_bytes(),
                },
                ProofType::Block => proto::remote_prover::ProofRequest {
                    proof_type: proto::remote_prover::ProofType::Block.into(),
                    payload: generate_proposed_block().to_bytes(),
                },
                ProofType::Batch => proto::remote_prover::ProofRequest {
                    proof_type: proto::remote_prover::ProofType::Batch.into(),
                    payload: generate_proposed_batch().await.to_bytes(),
                },
            }
        },
        _ => unreachable!("This is for remote provers only"),
    }
}
