use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use miden_objects::account::AccountId;
use tokio::time::{Duration, interval};

use super::challenge::{CHALLENGE_LIFETIME_SECONDS, Challenge};
use crate::{
    REQUESTS_QUEUE_SIZE,
    server::{ApiKey, get_pow::PowRequest, get_tokens::InvalidRequest},
};

/// The maximum difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const MAX_DIFFICULTY: usize = 24;

/// The number of active requests to increase the difficulty by 1.
const ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY: usize = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY;

/// The time window in seconds for rate limiting per account ID.
/// Must be less than [`CHALLENGE_LIFETIME_SECONDS`] to effectively rate limit.
const ACCOUNT_ID_RATE_LIMIT_WINDOW: u64 = 5;

// POW
// ================================================================================================

#[derive(Clone)]
pub(crate) struct PoW {
    secret: [u8; 32],
    difficulty_per_key: Arc<Mutex<HashMap<ApiKey, usize>>>,
    challenge_cache: ChallengeCache,
}

impl PoW {
    /// Creates a new `PoW` instance.
    pub fn new(secret: [u8; 32]) -> Self {
        let challenge_cache = ChallengeCache::default();

        // Start the cleanup task
        let cleanup_state = challenge_cache.clone();
        tokio::spawn(async move {
            cleanup_state.run_cleanup().await;
        });

        Self {
            secret,
            difficulty_per_key: Arc::new(Mutex::new(HashMap::new())),
            challenge_cache,
        }
    }

    /// Generates a new challenge.
    pub fn build_challenge(&self, request: PowRequest) -> Challenge {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();

        let difficulty = *self
            .difficulty_per_key
            .lock()
            .expect("PoW difficulty per key map lock poisoned")
            .entry(request.api_key.clone())
            .or_insert(1);

        Challenge::new(difficulty, timestamp, self.secret, request.account_id, request.api_key)
    }

    /// Adjust the difficulty of the `PoW`.
    ///
    /// The difficulty is adjusted based on the number of active requests.
    /// The difficulty is increased by 1 for every `ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY` active
    /// requests. The difficulty is clamped between 1 and `MAX_DIFFICULTY`.
    pub fn adjust_difficulty(&self, active_requests: usize, api_key: ApiKey) {
        let new_difficulty =
            (active_requests / ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY).clamp(1, MAX_DIFFICULTY);
        self.difficulty_per_key
            .lock()
            .expect("PoW difficulty per key map lock poisoned")
            .insert(api_key, new_difficulty);
    }

    /// Submits a challenge.
    ///
    /// The challenge is validated and added to the cache.
    ///
    /// # Errors
    /// Returns an error if:
    /// * The challenge is expired.
    /// * The challenge is invalid.
    /// * The challenge was already used.
    ///
    /// # Panics
    /// Panics if the challenge cache lock is poisoned.
    pub(crate) fn submit_challenge(
        &self,
        timestamp: u64,
        challenge: &str,
        nonce: u64,
        account_id: AccountId,
        api_key: &ApiKey,
    ) -> Result<(), InvalidRequest> {
        let challenge = Challenge::decode(challenge, self.secret)?;

        // Check if the last timestamp for the account id is within the rate limit time
        let prev_timestamp = self
            .challenge_cache
            .account_id_timestamps
            .lock()
            .expect("PoW account id timestamps map lock poisoned")
            .insert(account_id, timestamp);
        let is_rate_limited = prev_timestamp.is_some_and(|prev_timestamp| {
            (timestamp - prev_timestamp) < ACCOUNT_ID_RATE_LIMIT_WINDOW
        });
        if is_rate_limited {
            return Err(InvalidRequest::RateLimited);
        }

        // Check timestamp validity
        if challenge.is_expired(timestamp) {
            return Err(InvalidRequest::ExpiredServerTimestamp(challenge.timestamp, timestamp));
        }

        // Validate the challenge
        let valid_account_id = account_id == challenge.account_id;
        let valid_api_key = *api_key == challenge.api_key;
        let valid_nonce = challenge.validate_pow(nonce);
        if !(valid_account_id && valid_api_key && valid_nonce) {
            return Err(InvalidRequest::InvalidPoW);
        }

        // Check if challenge was already used
        if !self
            .challenge_cache
            .challenges
            .lock()
            .expect("PoW challenge cache lock poisoned")
            .insert(challenge)
        {
            return Err(InvalidRequest::ChallengeAlreadyUsed);
        }

        Ok(())
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
struct ChallengeCache {
    /// Once a challenge is added, it cannot be submitted again.
    challenges: Arc<Mutex<BTreeSet<Challenge>>>,
    /// A map of account IDs to timestamps of their last challenge submission.
    account_id_timestamps: Arc<Mutex<BTreeMap<AccountId, u64>>>,
}

impl ChallengeCache {
    /// Cleanup expired challenges.
    ///
    /// Challenges are expired if they are older than [`CHALLENGE_LIFETIME_SECONDS`]
    /// seconds.
    pub fn cleanup_expired_challenges(&self) {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
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

// TESTS
// ================================================================================================

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
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let api_key = ApiKey::generate();

        let challenge = Challenge::new(difficulty, timestamp, secret, account_id, api_key);

        let encoded = challenge.encode();
        let decoded = Challenge::decode(&encoded, secret).unwrap();

        assert_eq!(challenge.difficulty, decoded.difficulty);
        assert_eq!(challenge.timestamp, decoded.timestamp);
        assert_eq!(challenge.signature, decoded.signature);
    }

    #[tokio::test]
    async fn test_pow_validation() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();

        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result = pow.submit_challenge(
            challenge.timestamp,
            &challenge.encode(),
            nonce,
            account_id,
            &api_key,
        );
        assert!(result.is_ok());

        // Try to use the same challenge again with different nonce- should fail
        let result = pow.submit_challenge(
            challenge.timestamp,
            &challenge.encode(),
            nonce + 1,
            account_id,
            &api_key,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_adjust_difficulty_minimum_clamp() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();

        // With 0 active requests, difficulty should be clamped to 1
        pow.adjust_difficulty(0, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 1);

        // With requests less than ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY,
        // difficulty should still be 1
        pow.adjust_difficulty(40, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 1);
    }

    #[tokio::test]
    async fn test_adjust_difficulty_maximum_clamp() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();

        // With very high number of active requests, difficulty should be clamped to MAX_DIFFICULTY
        pow.adjust_difficulty(2000, api_key.clone());
        assert_eq!(
            pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(),
            MAX_DIFFICULTY
        );

        // Test with an extremely high number
        pow.adjust_difficulty(100_000, api_key.clone());
        assert_eq!(
            pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(),
            MAX_DIFFICULTY
        );
    }

    #[tokio::test]
    async fn test_adjust_difficulty_linear_scaling() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();

        // ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY = 1000 / 24
        // = 41

        // 41 active requests should give difficulty 1
        pow.adjust_difficulty(41, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 1);

        // 82 active requests should give difficulty 2 (82 / 41 = 2)
        pow.adjust_difficulty(82, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 2);

        // 123 active requests should give difficulty 3 (123 / 41 = 3)
        pow.adjust_difficulty(123, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 3);

        // 205 active requests should give difficulty 5 (205 / 41 = 5)
        pow.adjust_difficulty(205, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 5);

        // 984 active requests should give difficulty 24 (984 / 41 = 24)
        pow.adjust_difficulty(984, api_key.clone());
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key).unwrap().clone(), 24);
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
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();

        // Valid timestamp (current time)
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let challenge = Challenge::new(1, current_time, secret, account_id, ApiKey::generate());
        assert!(!challenge.is_expired(current_time));

        // Expired timestamp (too old)
        let old_timestamp = current_time - CHALLENGE_LIFETIME_SECONDS - 10;
        let challenge = Challenge::new(1, old_timestamp, secret, account_id, ApiKey::generate());
        assert!(challenge.is_expired(current_time));
    }

    #[tokio::test]
    async fn difficulty_is_adjusted_per_key() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key_1 = ApiKey::generate();
        let api_key_2 = ApiKey::generate();

        pow.adjust_difficulty(ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY, api_key_1.clone());
        pow.adjust_difficulty(ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY * 2, api_key_2.clone());

        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key_1).unwrap().clone(), 1);
        assert_eq!(pow.difficulty_per_key.lock().unwrap().get(&api_key_2).unwrap().clone(), 2);
    }

    #[tokio::test]
    async fn account_id_is_rate_limited() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();

        // Solve first challenge
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result = pow.submit_challenge(
            challenge.timestamp,
            &challenge.encode(),
            nonce,
            account_id,
            &api_key,
        );
        assert!(result.is_ok());

        // Try to solve second challenge but should fail because of rate limiting
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result = pow.submit_challenge(
            challenge.timestamp,
            &challenge.encode(),
            nonce,
            account_id,
            &api_key,
        );
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(InvalidRequest::RateLimited)));
    }
}
