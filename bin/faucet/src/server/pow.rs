use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, extract::State, response::IntoResponse};
use miden_tx::utils::ToHex;
use num_bigint::BigUint;
use rand::{Rng, rng};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
use tokio::time::{Duration, interval};

use super::{
    Server,
    get_tokens::{InvalidRequest, RawMintRequest},
};

/// The initial target shift.
///
/// The target value is the reference value that is used to determine the validity of the
/// `PoW` solution. The hash of the seed and the solution must be less than the target value to be
/// valid.
///
/// The target for the challenge will be computed as `max_target / difficulty`.
/// `max_target` is the maximum target value, which is `2^256 - 1` shifted right by
/// `INITIAL_TARGET_SHIFT`.
const INITIAL_TARGET_SHIFT: usize = 16;

/// The number of active requests to increase the difficulty by 1.
const ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY: usize = 10;

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
    pub(crate) difficulty: usize,
    pub(crate) target: String,
}

impl PowParameters {
    /// Check the server signature.
    ///
    /// The server signature is the result of hashing the server salt, seed and timestamp.
    pub fn check_server_signature(&self, server_salt: &str) -> Result<(), InvalidRequest> {
        let hash = get_server_signature(
            server_salt,
            &self.pow_seed,
            self.server_timestamp,
            self.difficulty,
        );
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
    /// * `challenge_cache` - The challenge cache to be used to validate the challenge.
    ///
    /// The solution is valid if the hash H(seed, solution), when interpreted as a number,
    /// is less than the target value.
    pub fn check_pow_solution(
        &self,
        challenge_cache: &ChallengeCache,
    ) -> Result<(), InvalidRequest> {
        let mut challenges =
            challenge_cache.challenges.lock().expect("PoW challenge cache lock poisoned");

        if challenges.get(&self.pow_seed).is_some() {
            return Err(InvalidRequest::ChallengeAlreadyUsed);
        }

        // Calculate the hash
        let mut hasher = Sha3_256::new();
        hasher.update(&self.pow_seed);
        hasher.update(self.pow_solution.to_string().as_bytes());
        let hash = hasher.finalize();

        // Convert hash to a number (taking first 16 bytes to fit in u128)
        let hash_num = BigUint::from_bytes_be(&hash[..32]);
        let target =
            BigUint::parse_bytes(self.target.as_bytes(), 16).ok_or(InvalidRequest::InvalidPoW)?;

        if hash_num >= target {
            return Err(InvalidRequest::InvalidPoW);
        }

        // If we get here, the solution is valid
        // Add the challenge to the cache to prevent reuse
        challenges.insert(self.pow_seed.to_string(), self.server_timestamp);

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
            difficulty: *value
                .pow_difficulty
                .as_ref()
                .ok_or(InvalidRequest::MissingPowParameters)?,
            target: value.pow_target.as_ref().ok_or(InvalidRequest::MissingPowParameters)?.clone(),
        })
    }
}

// POW
// ================================================================================================

#[derive(Clone)]
pub struct PoW {
    pub(crate) salt: String,
    pub(crate) difficulty: Arc<AtomicUsize>,
    pub(crate) challenge_cache: ChallengeCache,
}

impl PoW {
    /// Adjust the difficulty of the `PoW`.
    ///
    /// The difficulty is adjusted based on the number of active requests.
    /// The difficulty is increased by 1 for every `ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY`
    /// requests.
    pub fn adjust_difficulty(&self, active_requests: usize) {
        let new_difficulty = (active_requests / ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY).max(1);
        self.difficulty.store(new_difficulty, Ordering::Relaxed);
    }

    /// Get the target value for a given difficulty.
    ///
    /// The target for the challenge is computed as `max_target / difficulty`.
    pub fn get_target(difficulty: usize) -> String {
        let max_target = BigUint::from_bytes_be(&[0xff; 32]) >> INITIAL_TARGET_SHIFT;
        let target = max_target / difficulty;
        target.to_str_radix(16)
    }
}

// CHALLENGE CACHE
// ================================================================================================

/// A cache for managing challenges.
///
/// Challenges are used to validate the `PoW` solution.
/// We store the solved challenges in a map with the seed as the key to ensure that each challenge
/// is only used once.
/// Challenges gets removed periodically.
#[derive(Clone, Default)]
pub struct ChallengeCache {
    /// Once a challenge is added, it cannot be submitted again.
    challenges: Arc<Mutex<HashMap<String, u64>>>,
}

impl ChallengeCache {
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
        challenges.retain(|_, timestamp| {
            (current_time - *timestamp) <= SERVER_TIMESTAMP_TOLERANCE_SECONDS
        });
    }

    /// Run the cleanup task.
    ///
    /// The cleanup task is responsible for removing expired challenges from the cache.
    /// It runs every minute and removes challenges that are not longer valid because of its
    /// timestamp.
    pub async fn run_cleanup(self) {
        let mut interval = interval(Duration::from_secs(60));

        loop {
            interval.tick().await;
            self.cleanup_expired_challenges();
        }
    }
}

#[derive(Serialize)]
struct PoWResponse {
    seed: String,
    target: String,
    server_signature: String,
    timestamp: u64,
}

/// Get the server signature.
///
/// The server signature is the result of hashing the server salt, the seed, and the timestamp.
pub(crate) fn get_server_signature(
    server_salt: &str,
    seed: &str,
    timestamp: u64,
    difficulty: usize,
) -> String {
    let mut hasher = Sha3_256::new();
    hasher.update(server_salt);
    hasher.update(seed);
    hasher.update(timestamp.to_string().as_bytes());
    hasher.update(difficulty.to_string().as_bytes());
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
    let difficulty = server.pow.difficulty.load(Ordering::Relaxed);

    let server_signature =
        get_server_signature(&server.pow.salt, &random_seed, timestamp, difficulty);

    Json(PoWResponse {
        seed: random_seed,
        target: PoW::get_target(difficulty),
        server_signature,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use sha3::{Digest, Sha3_256};

    use super::*;

    fn find_pow_solution(seed: &str, target: &str) -> u64 {
        let target = BigUint::parse_bytes(target.as_bytes(), 16).unwrap();
        let mut solution = 0;

        loop {
            let mut hasher = Sha3_256::new();
            hasher.update(seed);
            hasher.update(solution.to_string().as_bytes());
            let hash = hasher.finalize();
            let hash_num = BigUint::from_bytes_be(&hash[..32]);

            if hash_num < target {
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

        let difficulty = 3;

        let mut hasher = Sha3_256::new();
        hasher.update(server_salt);
        hasher.update(seed);
        hasher.update(timestamp.to_string().as_bytes());
        hasher.update(difficulty.to_string().as_bytes());
        let server_signature = hasher.finalize().to_hex();

        let target = PoW::get_target(difficulty);
        let solution = find_pow_solution(seed, &target);

        let pow_parameters = PowParameters {
            pow_seed: seed.to_string(),
            server_signature: server_signature.clone(),
            server_timestamp: timestamp,
            pow_solution: solution,
            difficulty,
            target,
        };

        let result = pow_parameters.check_server_signature(server_salt);

        assert!(result.is_ok());

        let challenge_cache = ChallengeCache::default();

        let result = pow_parameters.check_pow_solution(&challenge_cache);

        assert!(result.is_ok());

        // Check that the challenge is not valid anymore
        let result = pow_parameters.check_pow_solution(&challenge_cache);
        assert!(result.is_err());
    }

    #[test]
    fn test_check_server_signature_failure() {
        let server_salt = "miden-faucet";
        let seed = "0x1234567890abcdef";
        let timestamp = 1_234_567_890;
        let server_signature = "0x1234567890abcdef";

        let difficulty = 3;
        let target = PoW::get_target(difficulty);

        let pow_parameters = PowParameters {
            pow_seed: seed.to_string(),
            server_signature: server_signature.to_string(),
            server_timestamp: timestamp,
            pow_solution: 1_234_567_890,
            difficulty,
            target,
        };
        let result = pow_parameters.check_server_signature(server_salt);
        assert!(result.is_err());

        let challenge_cache = ChallengeCache::default();
        let result = pow_parameters.check_pow_solution(&challenge_cache);

        assert!(result.is_err());
    }

    #[test]
    fn test_adjust_difficulty_minimum_clamp() {
        // Test that difficulty is clamped to minimum value of 1
        let pow = PoW {
            salt: "test-salt".to_string(),
            difficulty: Arc::new(AtomicUsize::new(10)),
            challenge_cache: ChallengeCache::default(),
        };

        // With 0 active requests, difficulty should be clamped to 1
        pow.adjust_difficulty(0);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 1);

        // With requests less than ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY (8),
        // difficulty should still be 1
        pow.adjust_difficulty(7);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_adjust_difficulty_caling() {
        // Test that difficulty scales with active requests
        let pow = PoW {
            salt: "test-salt".to_string(),
            difficulty: Arc::new(AtomicUsize::new(1)),
            challenge_cache: ChallengeCache::default(),
        };

        // 10 active requests should give difficulty 1
        pow.adjust_difficulty(10);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 1);

        // 20 active requests should give difficulty 2 (20 / 10 = 2)
        pow.adjust_difficulty(20);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 2);

        // 30 active requests should give difficulty 3 (30 / 10 = 3)
        pow.adjust_difficulty(30);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 3);

        // 40 active requests should give difficulty 4 (40 / 10 = 4)
        pow.adjust_difficulty(40);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 4);

        // 300 active requests should give difficulty 30 (300 / 10 = 30)
        pow.adjust_difficulty(300);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 30);
    }
}
