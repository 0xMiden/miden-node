use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use miden_objects::account::AccountId;
use tokio::time::{Duration, interval};

use super::challenge::{CHALLENGE_LIFETIME_SECONDS, Challenge};
use crate::{
    REQUESTS_QUEUE_SIZE,
    server::{
        ApiKey,
        get_pow::{InvalidPowRequest, PowRequest},
        get_tokens::InvalidMintRequest,
    },
};

/// The maximum difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const MAX_DIFFICULTY: usize = 24;

/// The number of active requests to increase the difficulty by 1.
const ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY: usize = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY;

/// The interval at which the challenge cache is cleaned up.
const CLEANUP_INTERVAL_SECONDS: u64 = 5;

// POW
// ================================================================================================

#[derive(Clone)]
pub(crate) struct PoW {
    secret: [u8; 32],
    challenge_cache: Arc<Mutex<ChallengeCache>>,
}

impl PoW {
    /// Creates a new `PoW` instance.
    pub fn new(secret: [u8; 32]) -> Self {
        let challenge_cache = Arc::new(Mutex::new(ChallengeCache::default()));

        // Start the cleanup task
        let cleanup_state = challenge_cache.clone();
        tokio::spawn(async move {
            ChallengeCache::run_cleanup(cleanup_state).await;
        });

        Self { secret, challenge_cache }
    }

    /// Generates a new challenge with the difficulty for the given API key. If the API key is not
    /// found, the difficulty is defaulted to 1.
    pub fn build_challenge(&self, request: PowRequest) -> Result<Challenge, InvalidPowRequest> {
        if self
            .challenge_cache
            .lock()
            .expect("challenge cache lock should not be poisoned")
            .has_challenge_for_account(request.account_id)
        {
            return Err(InvalidPowRequest::RateLimited);
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();
        let difficulty = self.get_difficulty(&request.api_key);

        Ok(Challenge::new(
            difficulty,
            timestamp,
            self.secret,
            request.account_id,
            request.api_key,
        ))
    }

    /// Returns the difficulty for the given API key.
    /// The difficulty is adjusted based on the number of active requests.
    /// The difficulty is increased by 1 for every `ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY` active
    /// requests. The difficulty is clamped between 1 and `MAX_DIFFICULTY`.
    pub fn get_difficulty(&self, api_key: &ApiKey) -> usize {
        let num_challenges = self
            .challenge_cache
            .lock()
            .expect("challenge cache lock should not be poisoned")
            .num_challenges_for_api_key(api_key);
        (num_challenges / ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY).clamp(1, MAX_DIFFICULTY)
    }

    /// Submits a challenge.
    ///
    /// The challenge is validated and added to the cache. Also, the difficulty is adjusted based
    /// on the number of active challenges.
    ///
    /// # Errors
    /// Returns an error if:
    /// * The challenge is expired.
    /// * The challenge is invalid.
    /// * The challenge was already used.
    /// * The account has already submitted a challenge.
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
    ) -> Result<(), InvalidMintRequest> {
        let challenge = Challenge::decode(challenge, self.secret)?;

        // Check timestamp validity
        if challenge.is_expired(timestamp) {
            return Err(InvalidMintRequest::ExpiredServerTimestamp(challenge.timestamp, timestamp));
        }

        // Validate the challenge
        let valid_account_id = account_id == challenge.account_id;
        let valid_api_key = *api_key == challenge.api_key;
        let valid_nonce = challenge.validate_pow(nonce);
        if !(valid_nonce && valid_account_id && valid_api_key) {
            return Err(InvalidMintRequest::InvalidPoW);
        }

        // Check if account has recently submitted a challenge.
        if self
            .challenge_cache
            .lock()
            .expect("challenge cache lock should not be poisoned")
            .has_challenge_for_account(account_id)
        {
            return Err(InvalidMintRequest::RateLimited);
        }

        // Check if the cache already contains the challenge. If not, it is inserted.
        if !self
            .challenge_cache
            .lock()
            .expect("challenge cache lock should not be poisoned")
            .insert_challenge(challenge)
        {
            return Err(InvalidMintRequest::ChallengeAlreadyUsed);
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
    /// Keeps track of recently submitted challenges.
    challenges: BTreeSet<Challenge>,
    /// Maps API key to the number of solved challenges.
    challenges_per_key: HashMap<ApiKey, usize>,
    /// Keeps track of accounts which have recently solved challenges.
    account_ids: BTreeSet<AccountId>,
}

impl ChallengeCache {
    /// Inserts a challenge into the cache and returns true if the challenge was not already
    /// submitted.
    pub fn insert_challenge(&mut self, challenge: Challenge) -> bool {
        let account_id = challenge.account_id;
        let api_key = challenge.api_key.clone();

        let is_new = self.challenges.insert(challenge);
        if is_new {
            self.challenges_per_key.entry(api_key).and_modify(|c| *c += 1).or_insert(1);
            self.account_ids.insert(account_id);
        }
        is_new
    }

    /// Checks if a challenge has been submitted for the given account
    pub fn has_challenge_for_account(&self, account_id: AccountId) -> bool {
        self.account_ids.contains(&account_id)
    }

    /// Returns the number of challenges submitted for the given API key.
    pub fn num_challenges_for_api_key(&self, key: &ApiKey) -> usize {
        self.challenges_per_key.get(key).copied().unwrap_or(0)
    }

    /// Cleanup expired challenges.
    ///
    /// Challenges are expired if they are older than [`CHALLENGE_LIFETIME_SECONDS`]
    /// seconds.
    pub fn cleanup_expired_challenges(&mut self) {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();

        self.challenges.retain(|challenge| {
            let expired = (current_time - challenge.timestamp) <= CHALLENGE_LIFETIME_SECONDS;
            if expired {
                self.challenges_per_key
                    .entry(challenge.api_key.clone())
                    .and_modify(|c| *c -= 1)
                    .or_insert(0);
                self.account_ids.remove(&challenge.account_id);
            }
            expired
        });
    }

    /// Run the cleanup task.
    ///
    /// The cleanup task is responsible for removing expired challenges from the cache.
    /// It runs every minute and removes challenges that are no longer valid because of their
    /// timestamp.
    pub async fn run_cleanup(cache: Arc<Mutex<Self>>) {
        let mut interval = interval(Duration::from_secs(CLEANUP_INTERVAL_SECONDS));

        loop {
            interval.tick().await;
            cache
                .lock()
                .expect("challenge cache lock should not be poisoned")
                .cleanup_expired_challenges();
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

    #[tokio::test]
    async fn test_pow_validation() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();

        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let challenge = pow
            .build_challenge(PowRequest { account_id, api_key: api_key.clone() })
            .unwrap();
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
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let api_key = ApiKey::generate();

        // Valid timestamp (current time)
        let challenge = Challenge::new(1, current_time, secret, account_id, api_key.clone());
        assert!(!challenge.is_expired(current_time));

        // Expired timestamp (too old)
        let old_timestamp = current_time - CHALLENGE_LIFETIME_SECONDS - 10;
        let challenge = Challenge::new(1, old_timestamp, secret, account_id, api_key);
        assert!(challenge.is_expired(current_time));
    }

    #[tokio::test]
    async fn account_id_is_rate_limited() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let api_key = ApiKey::generate();
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();

        // Solve first challenge
        let challenge = pow
            .build_challenge(PowRequest { account_id, api_key: api_key.clone() })
            .unwrap();
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result = pow.submit_challenge(
            challenge.timestamp,
            &challenge.encode(),
            nonce,
            account_id,
            &api_key,
        );
        assert!(result.is_ok());

        // Try to request a second challenge but should fail because of rate limiting
        let result = pow.build_challenge(PowRequest { account_id, api_key });
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(InvalidPowRequest::RateLimited)));
    }
}
