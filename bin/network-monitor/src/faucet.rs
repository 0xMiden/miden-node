//! Faucet testing functionality.
//!
//! This module contains the logic for periodically testing faucet functionality
//! by requesting proof-of-work challenges, solving them, and submitting token requests.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use miden_objects::account::AccountId;
use miden_objects::testing::account_id::ACCOUNT_ID_SENDER;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::COMPONENT;
use crate::status::{ServiceDetails, ServiceStatus, Status};

// FAUCET TEST TYPES
// ================================================================================================

/// Details of a faucet test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaucetTestDetails {
    pub test_duration_ms: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub last_tx_id: Option<String>,
    pub last_note_id: Option<String>,
    pub challenge_difficulty: Option<u64>,
}

/// Response from the faucet's `/pow` endpoint.
#[derive(Debug, Deserialize)]
struct PowChallengeResponse {
    challenge: String,
    target: u64,
    #[allow(dead_code)] // Timestamp is part of API response but not used
    timestamp: u64,
}

/// Response from the faucet's `/get_tokens` endpoint.
#[derive(Debug, Deserialize)]
struct GetTokensResponse {
    tx_id: String,
    note_id: String,
}

// FAUCET TEST TASK
// ================================================================================================

/// Runs a task that continuously tests faucet functionality and updates a watch channel.
///
/// This function spawns a task that periodically requests proof-of-work challenges from the faucet,
/// solves them, and submits token requests to verify the faucet is operational.
///
/// # Arguments
///
/// * `faucet_url` - The URL of the faucet service to test.
/// * `status_sender` - The sender for the watch channel.
///
/// # Returns
///
/// `Ok(())` if the task completes successfully, or an error if the task fails.
#[instrument(target = COMPONENT, name = "faucet-test-task", skip_all)]
pub async fn run_faucet_test_task(
    faucet_url: Url,
    status_sender: watch::Sender<ServiceStatus>,
) -> anyhow::Result<()> {
    let client = Client::new();
    let mut success_count = 0u64;
    let mut failure_count = 0u64;
    let mut last_tx_id = None;
    let mut last_note_id = None;
    let mut last_challenge_difficulty = None;

    let mut interval = tokio::time::interval(Duration::from_secs(2 * 60)); // Test every 2 minutes
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("failed to get current time")?
            .as_secs();

        let start_time = std::time::Instant::now();

        match perform_faucet_test(&client, &faucet_url).await {
            Ok((result, challenge_difficulty)) => {
                success_count += 1;
                last_tx_id = Some(result.tx_id.clone());
                last_note_id = Some(result.note_id.clone());
                last_challenge_difficulty = Some(challenge_difficulty);
                info!("Faucet test successful: tx_id={}, note_id={}", result.tx_id, result.note_id);
            },
            Err(e) => {
                failure_count += 1;
                warn!("Faucet test failed: {}", e);
            },
        }

        let test_duration_ms = start_time.elapsed().as_millis() as u64;

        let test_details = FaucetTestDetails {
            test_duration_ms,
            success_count,
            failure_count,
            last_tx_id: last_tx_id.clone(),
            last_note_id: last_note_id.clone(),
            challenge_difficulty: last_challenge_difficulty,
        };

        let status = ServiceStatus {
            name: "Faucet".to_string(),
            status: if success_count > 0 || failure_count == 0 {
                Status::Healthy
            } else {
                Status::Unhealthy
            },
            last_checked: current_time,
            error: None,
            details: ServiceDetails::FaucetTest(test_details),
        };

        // Send the status update (ignore if no receivers)
        let _ = status_sender.send(status);
    }
}

/// Performs a complete faucet test by requesting a `PoW` challenge and submitting the solution.
///
/// # Arguments
///
/// * `client` - The HTTP client to use.
/// * `faucet_url` - The URL of the faucet service.
///
/// # Returns
///
/// The response from the faucet if successful, or an error if the test fails.
async fn perform_faucet_test(
    client: &Client,
    faucet_url: &Url,
) -> anyhow::Result<(GetTokensResponse, u64)> {
    // Use a test account ID - convert to AccountId and format properly
    let account_id = AccountId::try_from(ACCOUNT_ID_SENDER)
        .context("Failed to create AccountId from test constant")?;

    let account_id = account_id.to_string();
    debug!("Generated account ID: {} (length: {})", account_id, account_id.len());

    // Step 1: Request PoW challenge
    let pow_url = faucet_url.join("/pow")?;
    let response = client.get(pow_url).query(&[("account_id", &account_id)]).send().await?;

    let response_text = response.text().await?;
    debug!("Faucet PoW response: {}", response_text);

    let challenge_response: PowChallengeResponse = serde_json::from_str(&response_text)
        .with_context(|| format!("Failed to parse PoW response: {response_text}"))?;

    debug!(
        "Received PoW challenge: target={}, challenge={}...",
        challenge_response.target,
        &challenge_response.challenge[..16.min(challenge_response.challenge.len())]
    );

    // Step 2: Solve the PoW challenge
    let nonce = solve_pow_challenge(&challenge_response.challenge, challenge_response.target)
        .context("Failed to solve PoW challenge")?;

    debug!("Solved PoW challenge with nonce: {}", nonce);

    // Step 3: Request tokens with the solution
    let tokens_url = faucet_url.join("/get_tokens")?;
    let asset_amount = 1_000_000u64; // 1 token with 6 decimals

    let response = client
        .get(tokens_url)
        .query(&[
            ("account_id", account_id.as_str()),
            ("is_private_note", "false"),
            ("asset_amount", &asset_amount.to_string()),
            ("challenge", &challenge_response.challenge),
            ("nonce", &nonce.to_string()),
        ])
        .send()
        .await?;

    let response_text = response.text().await?;

    let tokens_response: GetTokensResponse = serde_json::from_str(&response_text)
        .with_context(|| format!("Failed to parse tokens response: {response_text}"))?;

    Ok((tokens_response, challenge_response.target))
}

/// Solves a proof-of-work challenge using SHA-256 hashing.
///
/// # Arguments
///
/// * `challenge` - The challenge string in hexadecimal format.
/// * `target` - The target value. A solution is valid if H(challenge, nonce) < target.
///
/// # Returns
///
/// The nonce that solves the challenge, or an error if no solution is found within reasonable
/// bounds.
fn solve_pow_challenge(challenge: &str, target: u64) -> anyhow::Result<u64> {
    debug!("Solving PoW challenge: challenge={}, target={}", challenge, target);
    // Try up to 10 million nonces - this should be reasonable for most difficulties
    for nonce in 0..10_000_000u64 {
        let mut hasher = Sha256::new();
        hasher.update(challenge.as_bytes());
        hasher.update(nonce.to_be_bytes());
        let hash_result = hasher.finalize();

        // Convert first 8 bytes of hash to u64 for comparison with target
        let hash_as_u64 = u64::from_be_bytes(hash_result[..8].try_into().unwrap());

        if hash_as_u64 < target {
            debug!("PoW solution found! nonce={}, hash={}, target={}", nonce, hash_as_u64, target);
            return Ok(nonce);
        }

        // Log progress every 100k attempts
        if nonce % 100_000 == 0 && nonce > 0 {
            debug!("PoW attempt {}: current_hash={}, target={}", nonce, hash_as_u64, target);
        }
    }

    anyhow::bail!("Failed to solve PoW challenge within 10M attempts")
}
