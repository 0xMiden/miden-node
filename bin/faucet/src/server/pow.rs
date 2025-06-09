use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, extract::State, response::IntoResponse};
use tokio::time::{Duration, interval};

use super::{
    Server,
    challenge::{CHALLENGE_LIFETIME_SECONDS, Challenge},
    get_tokens::InvalidRequest,
};
use crate::REQUESTS_QUEUE_SIZE;

/// The maximum difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const MAX_DIFFICULTY: usize = 24;

/// The number of active requests to increase the difficulty by 1.
const ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY: usize = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY;

// POW
// ================================================================================================

#[derive(Clone)]
pub struct PoW {
    pub(crate) secret: [u8; 32],
    pub(crate) difficulty: Arc<AtomicUsize>,
    pub(crate) challenge_cache: ChallengeCache,
}

impl PoW {
    /// Creates a new `PoW` instance.
    pub fn new(secret: [u8; 32]) -> Self {
        Self {
            secret,
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

        Challenge::new(difficulty, timestamp, self.secret)
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
        if !challenge
            .is_not_expired(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs())
        {
            return Err(InvalidRequest::ExpiredServerTimestamp(
                challenge.timestamp,
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
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
    /// Challenges are expired if they are older than [`CHALLENGE_LIFETIME_SECONDS`]
    /// seconds.
    pub fn cleanup_expired_challenges(&self) {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let mut challenges = self.challenges.lock().unwrap();
        challenges
            .retain(|challenge| (current_time - challenge.timestamp) <= CHALLENGE_LIFETIME_SECONDS);
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

// API ENDPOINTS
// ================================================================================================

/// Get a challenge to be used by a client for `PoW`.
pub(crate) async fn get_pow_challenge(State(server): State<Server>) -> impl IntoResponse {
    let challenge = server.pow.build_challenge();
    Json(challenge)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_secret() -> [u8; 32] {
        let mut secret = [0u8; 32];
        secret[..12].copy_from_slice(b"miden-faucet");
        secret
    }

    fn find_pow_solution(challenge: &Challenge, max_iterations: u64) -> Option<u64> {
        (0..max_iterations).find(|&nonce| challenge.validate_pow(nonce))
    }

    #[test]
    fn test_challenge_encode_decode() {
        let secret = create_test_secret();
        let difficulty = 3;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let challenge = Challenge::new(difficulty, timestamp, secret);

        let encoded = challenge.encode();
        let decoded = Challenge::decode(&encoded, secret).unwrap();

        assert_eq!(challenge.difficulty, decoded.difficulty);
        assert_eq!(challenge.timestamp, decoded.timestamp);
        assert_eq!(challenge.signature, decoded.signature);
    }

    #[test]
    fn test_pow_validation() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);

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
        let secret = create_test_secret();
        let pow = PoW::new(secret);

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
        let secret = create_test_secret();
        let pow = PoW::new(secret);

        // With very high number of active requests, difficulty should be clamped to MAX_DIFFICULTY
        pow.adjust_difficulty(2000);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), MAX_DIFFICULTY);

        // Test with an extremely high number
        pow.adjust_difficulty(100_000);
        assert_eq!(pow.difficulty.load(Ordering::Relaxed), MAX_DIFFICULTY);
    }

    #[test]
    fn test_adjust_difficulty_linear_scaling() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);

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
        let secret = create_test_secret();
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        // Valid timestamp (current time)
        let challenge = Challenge::new(1, current_time, secret);
        assert!(challenge.is_not_expired(current_time));

        // Expired timestamp (too old)
        let old_timestamp = current_time - CHALLENGE_LIFETIME_SECONDS - 10;
        let challenge = Challenge::new(1, old_timestamp, secret);
        assert!(!challenge.is_not_expired(current_time));
    }
}
