use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, extract::State, response::IntoResponse};
use miden_tx::utils::ToHex;
use rand::{Rng, rng};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
use tokio::time::{Duration, interval};

use super::{
    Server,
    get_tokens::{InvalidRequest, RawMintRequest},
};

/// The difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const DIFFICULTY: u64 = 5;

/// The tolerance for the server timestamp.
///
/// The server timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of
/// the current time.
pub(crate) const SERVER_TIMESTAMP_TOLERANCE_SECONDS: u64 = 30;

// POW PARAMETERS
// ================================================================================================

/// Parameters for the `PoW` challenge.
///
/// This struct is used to store the parameters for the `PoW` challenge.
/// It is used to validate the `PoW` challenge and to store the parameters for the `PoW` challenge.
#[derive(Deserialize)]
pub(crate) struct PowParameters {
    pub(crate) pow_seed: String,
    pub(crate) server_signature: String,
    pub(crate) server_timestamp: u64,
    pub(crate) pow_solution: u64,
}

impl PowParameters {
    /// Check the server signature.
    ///
    /// The server signature is the result of hashing the server salt, seed and timestamp.
    pub fn check_server_signature(&self, server_salt: &str) -> Result<(), InvalidRequest> {
        let hash = get_server_signature(server_salt, &self.pow_seed, self.server_timestamp);
        if hash != self.server_signature {
            return Err(InvalidRequest::ServerSignaturesDoNotMatch);
        }
        Ok(())
    }

    /// Check the received timestamp.
    ///
    /// The timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of the
    /// current time.
    pub fn check_server_timestamp(&self) -> Result<(), InvalidRequest> {
        let server_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        if (server_timestamp - self.server_timestamp) > SERVER_TIMESTAMP_TOLERANCE_SECONDS {
            return Err(InvalidRequest::ExpiredServerTimestamp(
                self.server_timestamp,
                server_timestamp,
            ));
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
    pub fn check_pow_solution(
        &self,
        challenge_state: &ChallengeCache,
    ) -> Result<(), InvalidRequest> {
        // Then check the PoW solution
        let mut hasher = Sha3_256::new();
        hasher.update(&self.pow_seed);
        hasher.update(self.pow_solution.to_string().as_bytes());
        let hash = &hasher.finalize().to_hex();

        let leading_zeros = hash.chars().take_while(|&c| c == '0').count();
        if leading_zeros < DIFFICULTY as usize {
            return Err(InvalidRequest::InvalidPoW);
        }

        // If we get here, the solution is valid
        // Remove the challenge to prevent reuse
        challenge_state.remove_challenge(&self.pow_seed);

        Ok(())
    }
}

impl TryFrom<&RawMintRequest> for PowParameters {
    type Error = InvalidRequest;

    fn try_from(value: &RawMintRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            pow_seed: value.pow_seed.as_ref().ok_or(InvalidRequest::MissingPowParameters)?.clone(),
            server_signature: value
                .server_signature
                .as_ref()
                .ok_or(InvalidRequest::MissingPowParameters)?
                .clone(),
            server_timestamp: *value
                .server_timestamp
                .as_ref()
                .ok_or(InvalidRequest::MissingPowParameters)?,
            pow_solution: *value
                .pow_solution
                .as_ref()
                .ok_or(InvalidRequest::MissingPowParameters)?,
        })
    }
}

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
    /// The challenge is valid if the server signature matches.
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
        .challenge_cache
        .add_challenge(&random_seed, server_signature.clone(), timestamp);

    Json(PoWResponse {
        seed: random_seed,
        difficulty: DIFFICULTY,
        server_signature,
        timestamp,
    })
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

        let solution = find_pow_solution(seed);

        let pow_parameters = PowParameters {
            pow_seed: seed.to_string(),
            server_signature: server_signature.clone(),
            server_timestamp: timestamp,
            pow_solution: solution,
        };

        let result = pow_parameters.check_server_signature(server_salt);

        assert!(result.is_ok());

        let challenge_state = ChallengeCache::new();
        challenge_state.add_challenge(seed, server_signature, timestamp);
        let result = pow_parameters.check_pow_solution(&challenge_state);

        assert!(result.is_ok());
    }

    #[test]
    fn test_check_server_signature_failure() {
        let server_salt = "miden-faucet";
        let seed = "0x1234567890abcdef";
        let timestamp = 1_234_567_890;
        let server_signature = "0x1234567890abcdef";

        let pow_parameters = PowParameters {
            pow_seed: seed.to_string(),
            server_signature: server_signature.to_string(),
            server_timestamp: timestamp,
            pow_solution: 0,
        };
        let result = pow_parameters.check_server_signature(server_salt);
        assert!(result.is_err());

        let challenge_state = ChallengeCache::new();
        let result = pow_parameters.check_pow_solution(&challenge_state);

        assert!(result.is_err());
    }
}
