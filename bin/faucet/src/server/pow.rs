use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, extract::State, response::IntoResponse};
use miden_tx::utils::{ToHex, hex_to_bytes};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
use tokio::time::{Duration, interval};

use super::{Server, get_tokens::InvalidRequest};
use crate::REQUESTS_QUEUE_SIZE;

/// The maximum difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const MAX_DIFFICULTY: usize = 24;

/// The number of active requests to increase the difficulty by 1.
const ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY: usize = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY;

/// The tolerance for the server timestamp.
///
/// The server timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of
/// the current time.
pub(crate) const SERVER_TIMESTAMP_TOLERANCE_SECONDS: u64 = 30;

// CHALLENGE
// ================================================================================================

/// A challenge for proof-of-work validation.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct Challenge {
    pub(crate) difficulty: usize,
    pub(crate) timestamp: u64,
    pub(crate) signature: [u8; 32],
}

impl Challenge {
    /// Creates a new challenge with the given parameters.
    pub fn new(difficulty: usize, timestamp: u64, signature: [u8; 32]) -> Self {
        Self { difficulty, timestamp, signature }
    }

    /// Decodes the challenge and verifies that the signature part of the challenge is valid
    /// in the context of the specified salt.
    pub fn decode(value: &str, salt: [u8; 32]) -> Result<Self, InvalidRequest> {
        // Parse the hex-encoded challenge string
        let bytes: [u8; 48] =
            hex_to_bytes(value).map_err(|_| InvalidRequest::MissingPowParameters)?;

        if bytes.len() != 48 {
            // 8 + 8 + 32 bytes
            return Err(InvalidRequest::MissingPowParameters);
        }

        let difficulty = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let timestamp = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let signature: [u8; 32] = bytes[16..48].try_into().unwrap();

        let challenge = Self::new(difficulty, timestamp, signature);

        // Verify the signature
        let expected_signature = Self::compute_signature(salt, difficulty, timestamp);
        if signature != expected_signature {
            return Err(InvalidRequest::ServerSignaturesDoNotMatch);
        }

        Ok(challenge)
    }

    /// Encodes the challenge into a hex string.
    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(48);
        bytes.extend_from_slice(&(self.difficulty as u64).to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(&self.signature);
        bytes.to_hex()
    }

    /// Checks whether the provided nonce satisfies the difficulty requirement encoded in the
    /// challenge.
    pub fn validate_pow(&self, nonce: u64) -> bool {
        let mut hasher = Sha3_256::new();
        hasher.update(self.encode());
        hasher.update(nonce.to_be_bytes());
        let hash = hasher.finalize();

        let leading_zeros = hash.iter().take_while(|&b| *b == 0).count();
        leading_zeros >= self.difficulty
    }

    /// Checks if the challenge timestamp is still valid.
    pub fn is_timestamp_valid(&self) -> bool {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time should always be later than UNIX epoch")
            .as_secs();

        (current_time - self.timestamp) <= SERVER_TIMESTAMP_TOLERANCE_SECONDS
    }

    /// Computes the signature for a challenge.
    fn compute_signature(salt: [u8; 32], difficulty: usize, timestamp: u64) -> [u8; 32] {
        let mut hasher = Sha3_256::new();
        hasher.update(salt);
        hasher.update(difficulty.to_be_bytes());
        hasher.update(timestamp.to_be_bytes());
        hasher.finalize().into()
    }
}

// POW
// ================================================================================================

#[derive(Clone)]
pub struct PoW {
    pub(crate) salt: [u8; 32],
    pub(crate) difficulty: Arc<AtomicUsize>,
    pub(crate) challenge_cache: ChallengeCache,
}

impl PoW {
    /// Creates a new `PoW` instance.
    pub fn new(salt: [u8; 32]) -> Self {
        Self {
            salt,
            difficulty: Arc::new(AtomicUsize::new(1)),
            challenge_cache: ChallengeCache::default(),
        }
    }

    /// Generates a new challenge.
    pub fn build_challenge(&self) -> Challenge {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let difficulty = self.difficulty.load(Ordering::Relaxed);
        let signature = Challenge::compute_signature(self.salt, difficulty, timestamp);

        Challenge::new(difficulty, timestamp, signature)
    }

    /// Submits a challenge to the `PoW` instance.
    ///
    /// The challenge is validated and added to the cache.
    ///
    /// # Errors
    /// Returns an error if:
    /// * The challenge is expired.
    /// * The challenge is invalid.
    /// * The challenge was already used.
    pub fn submit_challenge(
        &self,
        challenge: &Challenge,
        nonce: u64,
    ) -> Result<(), InvalidRequest> {
        // Check timestamp validity
        if !challenge.is_timestamp_valid() {
            return Err(InvalidRequest::ExpiredServerTimestamp(
                challenge.timestamp,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("System time should always be later than UNIX epoch")
                    .as_secs(),
            ));
        }

        // Validate the proof of work
        if !challenge.validate_pow(nonce) {
            return Err(InvalidRequest::InvalidPoW);
        }

        // Check if challenge was already used
        if !self.challenge_cache.challenges.lock().unwrap().insert(challenge.clone()) {
            return Err(InvalidRequest::ChallengeAlreadyUsed);
        }

        Ok(())
    }

    /// Adjust the difficulty of the `PoW`.
    ///
    /// The difficulty is adjusted based on the number of active requests.
    /// The difficulty is increased by 1 for every `ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY` active
    /// requests. The difficulty is clamped between 1 and `MAX_DIFFICULTY`.
    pub fn adjust_difficulty(&self, active_requests: usize) {
        let new_difficulty =
            (active_requests / ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY).clamp(1, MAX_DIFFICULTY);
        self.difficulty.store(new_difficulty, Ordering::Relaxed);
    }
}

// CHALLENGE CACHE
// ================================================================================================

/// A cache for managing challenges.
///
/// Challenges are used to validate the `PoW` solution.
/// We store the solved challenges in a map with the challenge key to ensure that each challenge
/// is only used once.
/// Challenges get removed periodically.
#[derive(Clone, Default)]
pub struct ChallengeCache {
    /// Once a challenge is added, it cannot be submitted again.
    challenges: Arc<Mutex<HashSet<Challenge>>>,
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
        challenges.retain(|challenge| {
            (current_time - challenge.timestamp) <= SERVER_TIMESTAMP_TOLERANCE_SECONDS
        });
    }

    /// Run the cleanup task.
    ///
    /// The cleanup task is responsible for removing expired challenges from the cache.
    /// It runs every minute and removes challenges that are no longer valid because of their
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
    challenge: String,
    difficulty: usize,
    timestamp: u64,
}

/// Get a challenge to be used by a client for `PoW`.
pub(crate) async fn get_pow_challenge(State(server): State<Server>) -> impl IntoResponse {
    let challenge = server.pow.build_challenge();

    Json(PoWResponse {
        challenge: challenge.encode(),
        difficulty: challenge.difficulty,
        timestamp: challenge.timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_salt() -> [u8; 32] {
        let mut salt = [0u8; 32];
        salt[..12].copy_from_slice(b"miden-faucet");
        salt
    }

    fn find_pow_solution(challenge: &Challenge, max_iterations: u64) -> Option<u64> {
        (0..max_iterations).find(|&nonce| challenge.validate_pow(nonce))
    }

    #[test]
    fn test_challenge_encode_decode() {
        let salt = create_test_salt();
        let difficulty = 3;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let signature = Challenge::compute_signature(salt, difficulty, timestamp);
        let challenge = Challenge::new(difficulty, timestamp, signature);

        let encoded = challenge.encode();
        let decoded = Challenge::decode(&encoded, salt).unwrap();

        assert_eq!(challenge.difficulty, decoded.difficulty);
        assert_eq!(challenge.timestamp, decoded.timestamp);
        assert_eq!(challenge.signature, decoded.signature);
    }

    #[test]
    fn test_pow_validation() {
        let salt = create_test_salt();
        let pow = PoW::new(salt);

        // Set difficulty to 1 for faster testing
        pow.difficulty.store(1, Ordering::Relaxed);

        let challenge = pow.build_challenge();
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result = pow.submit_challenge(&challenge, nonce);
        assert!(result.is_ok());

        // Try to use the same challenge again - should fail
        let result = pow.submit_challenge(&challenge, nonce);
        assert!(result.is_err());
    }

    #[test]
    fn test_adjust_difficulty_minimum_clamp() {
        let salt = create_test_salt();
        let pow = PoW::new(salt);

        // With 0 active requests, difficulty should be clamped to 1
        pow.adjust_difficulty(0);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 1);

        // With requests less than ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY,
        // difficulty should still be 1
        pow.adjust_difficulty(40);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_adjust_difficulty_maximum_clamp() {
        let salt = create_test_salt();
        let pow = PoW::new(salt);

        // With very high number of active requests, difficulty should be clamped to MAX_DIFFICULTY
        pow.adjust_difficulty(2000);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), MAX_DIFFICULTY);

        // Test with an extremely high number
        pow.adjust_difficulty(100_000);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), MAX_DIFFICULTY);
    }

    #[test]
    fn test_adjust_difficulty_linear_scaling() {
        let salt = create_test_salt();
        let pow = PoW::new(salt);

        // ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY = 1000 / 24
        // = 41

        // 41 active requests should give difficulty 1
        pow.adjust_difficulty(41);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 1);

        // 82 active requests should give difficulty 2 (82 / 41 = 2)
        pow.adjust_difficulty(82);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 2);

        // 123 active requests should give difficulty 3 (123 / 41 = 3)
        pow.adjust_difficulty(123);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 3);

        // 205 active requests should give difficulty 5 (205 / 41 = 5)
        pow.adjust_difficulty(205);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 5);

        // 984 active requests should give difficulty 24 (984 / 41 = 24)
        pow.adjust_difficulty(984);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), 24);
    }

    #[test]
    fn test_adjust_difficulty_constants_validation() {
        assert_eq!(MAX_DIFFICULTY, 24);
        assert_eq!(ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY, REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY);

        // With current values: REQUESTS_QUEUE_SIZE = 1000, MAX_DIFFICULTY = 24
        // ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY should be 41 (1000 / 24 = 41.666... truncated to
        // 41)
        assert_eq!(ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY, 41);
    }

    #[test]
    fn test_timestamp_validation() {
        let salt = create_test_salt();
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        // Valid timestamp (current time)
        let signature = Challenge::compute_signature(salt, 1, current_time);
        let challenge = Challenge::new(1, current_time, signature);
        assert!(challenge.is_timestamp_valid());

        // Expired timestamp (too old)
        let old_timestamp = current_time - SERVER_TIMESTAMP_TOLERANCE_SECONDS - 10;
        let signature = Challenge::compute_signature(salt, 1, old_timestamp);
        let challenge = Challenge::new(1, old_timestamp, signature);
        assert!(!challenge.is_timestamp_valid());
    }
}
