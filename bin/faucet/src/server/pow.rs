use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, extract::State, response::IntoResponse};
use miden_tx::utils::ToHex;
use rand::{Rng, rng};
use serde::Serialize;
use sha3::{Digest, Sha3_256};
use tokio::time::{Duration, interval};

use super::{Server, get_tokens::InvalidRequest};

/// The difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const DIFFICULTY: u64 = 5;

/// The tolerance for the server timestamp.
///
/// The server timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of
/// the current time.
const SERVER_TIMESTAMP_TOLERANCE_SECONDS: u64 = 30;

// CHALLENGE STATE
// ================================================================================================

/// A state for managing challenges.
///
/// Challenges are used to validate the `PoW` solution.
/// We store the challenges in a map with the seed as the key to ensure that each challenge is
/// only used once. Once a challenge is created, it gets added to the map and is removed once the
/// challenge is solved.
#[derive(Clone)]
pub struct ChallengeCache {
    /// The challenges are stored in a map with the seed as the key to ensure that each challenge
    /// is only used once. Once a challenge is created, it gets added to the map and is removed
    /// once the challenge is solved.
    challenges: Arc<Mutex<HashMap<String, Challenge>>>,
}

/// A challenge is a single `PoW` challenge.
#[derive(Clone)]
pub struct Challenge {
    timestamp: u64,
    server_signature: String,
}

impl ChallengeCache {
    pub fn new() -> Self {
        Self {
            challenges: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Add a challenge to the state.
    pub fn add_challenge(&self, seed: &str, server_signature: String, timestamp: u64) {
        let mut challenges = self.challenges.lock().unwrap();
        challenges.insert(seed.to_string(), Challenge { timestamp, server_signature });
    }

    /// Validate a challenge.
    ///
    /// The challenge is valid if the server signature matches and the timestamp is within the
    /// tolerance.
    pub fn validate_challenge(
        &self,
        seed: &str,
        server_signature: &str,
    ) -> Result<(), InvalidRequest> {
        let challenges = self.challenges.lock().unwrap();

        if let Some(challenge) = challenges.get(seed) {
            if challenge.server_signature != server_signature {
                return Err(InvalidRequest::ServerSignaturesDoNotMatch);
            }

            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs();

            if (current_time - challenge.timestamp) > SERVER_TIMESTAMP_TOLERANCE_SECONDS {
                return Err(InvalidRequest::ExpiredServerTimestamp(
                    challenge.timestamp,
                    current_time,
                ));
            }

            Ok(())
        } else {
            Err(InvalidRequest::InvalidChallenge)
        }
    }

    /// Cleanup expired challenges.
    ///
    /// Challenges are expired if they are older than [`SERVER_TIMESTAMP_TOLERANCE_SECONDS`]
    /// seconds.
    pub fn cleanup_expired_challenges(&self) {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let mut challenges = self.challenges.lock().unwrap();
        challenges.retain(|_, challenge| {
            (current_time - challenge.timestamp) <= SERVER_TIMESTAMP_TOLERANCE_SECONDS
        });
    }

    /// Remove a challenge from the state.
    pub fn remove_challenge(&self, seed: &str) {
        let mut challenges = self.challenges.lock().unwrap();
        challenges.remove(seed);
    }
}

/// Run the cleanup task.
///
/// The cleanup task is responsible for removing expired challenges from the state.
/// It runs every minute.
pub async fn run_cleanup(challenge_state: ChallengeCache) {
    let mut interval = interval(Duration::from_secs(60));

    loop {
        interval.tick().await;
        challenge_state.cleanup_expired_challenges();
    }
}

#[derive(Serialize)]
struct PoWResponse {
    seed: String,
    difficulty: u64,
    server_signature: String,
    timestamp: u64,
}

/// Get the server signature.
///
/// The server signature is the result of hashing the server salt, the seed, and the timestamp.
pub(crate) fn get_server_signature(server_salt: &str, seed: &str, timestamp: u64) -> String {
    let mut hasher = Sha3_256::new();
    hasher.update(server_salt);
    hasher.update(seed);
    hasher.update(timestamp.to_string().as_bytes());
    hasher.finalize().to_hex()
}

/// Generate a random hex string of specified length in nibbles.
fn random_hex_string(num_nibbles: usize) -> String {
    // Generate random bytes
    let mut rng = rng();
    let mut random_bytes = vec![0u8; num_nibbles / 2];
    rng.fill(&mut random_bytes[..]);

    // Convert bytes to hex string
    random_bytes.iter().fold(String::new(), |acc, byte| format!("{acc}{byte:02x}"))
}

/// Get a seed to be used by a client as the `PoW` seed.
///
/// The seed is a 64 character random hex string.
pub(crate) async fn get_pow_seed(State(server): State<Server>) -> impl IntoResponse {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    let random_seed = random_hex_string(32);

    let server_signature = get_server_signature(&server.pow_salt, &random_seed, timestamp);

    // Store the challenge
    server
        .challenge_state
        .add_challenge(&random_seed, server_signature.clone(), timestamp);

    Json(PoWResponse {
        seed: random_seed,
        difficulty: DIFFICULTY,
        server_signature,
        timestamp,
    })
}

/// Check the server signature.
///
/// The server signature is the result of hashing the server salt and the seed.
pub(crate) fn check_server_signature(
    server_salt: &str,
    server_signature: &str,
    seed: &str,
    timestamp: u64,
) -> Result<(), InvalidRequest> {
    let hash = get_server_signature(server_salt, seed, timestamp);

    if hash != server_signature {
        return Err(InvalidRequest::ServerSignaturesDoNotMatch);
    }

    Ok(())
}

/// Check a `PoW` solution.
///
/// * `challenge_state` - The challenge state to be used to validate the challenge.
/// * `seed` - The seed to be used by the client as the `PoW` seed.
/// * `server_signature` - The server signature to be used to validate the challenge.
/// * `solution` - The solution to be checked.
///
/// The solution is valid if the hash of the seed and the solution has at least `DIFFICULTY`
/// leading zeros.
///
/// Returns `true` if the solution is valid, `false` otherwise.
pub(crate) fn check_pow_solution(
    challenge_state: &ChallengeCache,
    seed: &str,
    server_signature: &str,
    solution: u64,
) -> Result<(), InvalidRequest> {
    // First validate the challenge
    challenge_state.validate_challenge(seed, server_signature)?;

    // Then check the PoW solution
    let mut hasher = Sha3_256::new();
    hasher.update(seed);
    hasher.update(solution.to_string().as_bytes());
    let hash = &hasher.finalize().to_hex();

    let leading_zeros = hash.chars().take_while(|&c| c == '0').count();
    if leading_zeros < DIFFICULTY as usize {
        return Err(InvalidRequest::InvalidPoW);
    }

    // If we get here, the solution is valid
    // Remove the challenge to prevent reuse
    challenge_state.remove_challenge(seed);

    Ok(())
}

/// Check the received timestamp.
///
/// The timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of the
/// current time.
pub(crate) fn check_server_timestamp(timestamp: u64) -> Result<(), InvalidRequest> {
    let server_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    if (server_timestamp - timestamp) > SERVER_TIMESTAMP_TOLERANCE_SECONDS {
        return Err(InvalidRequest::ExpiredServerTimestamp(timestamp, server_timestamp));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use sha3::{Digest, Sha3_256};

    use super::*;

    fn find_pow_solution(seed: &str) -> u64 {
        let mut solution = 0;
        loop {
            let mut hasher = Sha3_256::new();
            hasher.update(seed);
            hasher.update(solution.to_string().as_bytes());
            let hash = &hasher.finalize().to_hex();
            let leading_zeros = hash.chars().take_while(|&c| c == '0').count();
            if leading_zeros >= DIFFICULTY as usize {
                return solution;
            }

            solution += 1;
        }
    }

    #[test]
    fn test_check_server_signature() {
        let server_salt = "miden-faucet";
        let seed = "0x1234567890abcdef";
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let mut hasher = Sha3_256::new();
        hasher.update(server_salt);
        hasher.update(seed);
        hasher.update(timestamp.to_string().as_bytes());
        let server_signature = hasher.finalize().to_hex();

        let result = check_server_signature(server_salt, &server_signature, seed, timestamp);
        assert!(result.is_ok());

        let solution = find_pow_solution(seed);

        let challenge_state = ChallengeCache::new();
        challenge_state.add_challenge(seed, server_signature.clone(), timestamp);
        let result = check_pow_solution(&challenge_state, seed, &server_signature, solution);

        assert!(result.is_ok());
    }

    #[test]
    fn test_check_server_signature_failure() {
        let server_salt = "miden-faucet";
        let seed = "0x1234567890abcdef";
        let timestamp = 1_234_567_890;
        let server_signature = "0x1234567890abcdef";

        let result = check_server_signature(server_salt, server_signature, seed, timestamp);
        assert!(result.is_err());

        let solution = 1_234_567_890;

        let challenge_state = ChallengeCache::new();
        let result = check_pow_solution(&challenge_state, seed, server_signature, solution);

        assert!(result.is_err());
    }
}
