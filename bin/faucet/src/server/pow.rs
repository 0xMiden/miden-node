use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use miden_objects::account::AccountId;
use tokio::time::{Duration, interval};

use super::challenge::{CHALLENGE_LIFETIME_SECONDS, Challenge};
use crate::{
    REQUESTS_QUEUE_SIZE,
    server::{ApiKey, get_pow::PowRequest, get_tokens::InvalidMintRequest},
};

/// The maximum difficulty of the `PoW`.
///
/// The difficulty is the number of leading zeros in the hash of the seed and the solution.
const MAX_DIFFICULTY: usize = 24;

/// The number of active requests to increase the difficulty by 1.
const ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY: usize = REQUESTS_QUEUE_SIZE / MAX_DIFFICULTY;

/// The interval at which the challenge cache is cleaned up.
const CLEANUP_INTERVAL_SECONDS: u64 = 2;

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

    /// Generates a new challenge.
    pub fn build_challenge(&self, request: PowRequest) -> Challenge {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();
        let difficulty = self.get_difficulty(&request.api_key);

        Challenge::new(difficulty, timestamp, request.account_id, request.api_key, self.secret)
    }

    /// Returns the difficulty for the given API key.
    ///
    /// The difficulty is adjusted based on the number of active requests per API key. The
    /// difficulty is increased by 1 for every `ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY` active
    /// requests, and it is clamped between 1 and `MAX_DIFFICULTY`.
    fn get_difficulty(&self, api_key: &ApiKey) -> usize {
        let num_challenges = self
            .challenge_cache
            .lock()
            .expect("challenge cache lock should not be poisoned")
            .num_challenges_for_api_key(api_key);
        (num_challenges / ACTIVE_REQUESTS_TO_INCREASE_DIFFICULTY).clamp(1, MAX_DIFFICULTY)
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
    /// * The account has already submitted a challenge recently (within the last
    ///   [`CHALLENGE_LIFETIME_SECONDS`] seconds).
    ///
    /// # Panics
    /// Panics if the challenge cache lock is poisoned.
    pub(crate) fn submit_challenge(
        &self,
        account_id: AccountId,
        api_key: &ApiKey,
        challenge: &str,
        nonce: u64,
        current_time: u64,
    ) -> Result<(), InvalidMintRequest> {
        let challenge = Challenge::decode(challenge, self.secret)?;

        // Check timestamp validity
        if challenge.is_expired(current_time) {
            return Err(InvalidMintRequest::ExpiredServerTimestamp(
                challenge.timestamp,
                current_time,
            ));
        }

        // Validate the challenge
        let valid_account_id = account_id == challenge.account_id;
        let valid_api_key = *api_key == challenge.api_key;
        let valid_nonce = challenge.validate_pow(nonce);
        if !(valid_nonce && valid_account_id && valid_api_key) {
            return Err(InvalidMintRequest::InvalidPoW);
        }

        let mut challenge_cache = self
            .challenge_cache
            .lock()
            .expect("challenge cache lock should not be poisoned");

        // Check if account has recently submitted a challenge.
        if challenge_cache.has_challenge_for_account(account_id) {
            return Err(InvalidMintRequest::RateLimited);
        }

        // Check if the cache already contains the challenge. If not, it is inserted.
        if !challenge_cache.insert_challenge(&challenge) {
            return Err(InvalidMintRequest::ChallengeAlreadyUsed);
        }

        Ok(())
    }
}

// CHALLENGE CACHE
// ================================================================================================

/// A cache that keeps track of the submitted challenges.
///
/// The cache is used to check if a challenge has already been submitted for a given account and API
/// key. It also keeps track of the number of challenges submitted for each API key.
///
/// The cache is cleaned up periodically, removing expired challenges.
#[derive(Clone, Default)]
struct ChallengeCache {
    /// Maps challenge timestamp to a tuple of `AccountId` and `ApiKey`.
    challenges: BTreeMap<u64, Vec<(AccountId, ApiKey)>>,
    /// Maps API key to the number of submitted challenges.
    challenges_per_key: HashMap<ApiKey, usize>,
    /// Maps account id to the number of submitted challenges.
    account_ids: BTreeMap<AccountId, usize>,
}

impl ChallengeCache {
    /// Inserts a challenge into the cache, updating the number of challenges submitted for the
    /// account and the API key.
    ///
    /// Returns true if the challenge was newly inserted.
    pub fn insert_challenge(&mut self, challenge: &Challenge) -> bool {
        let account_id = challenge.account_id;
        let api_key = challenge.api_key.clone();

        // check if (timestamp, account_id, api_key) is already in the cache
        let issuers = self.challenges.entry(challenge.timestamp).or_default();
        if issuers.iter().any(|(id, key)| id == &account_id && key == &api_key) {
            return false;
        }

        issuers.push((account_id, api_key.clone()));
        self.challenges_per_key
            .entry(api_key)
            .and_modify(|c| *c = c.saturating_add(1))
            .or_insert(1);
        self.account_ids
            .entry(account_id)
            .and_modify(|c| *c = c.saturating_add(1))
            .or_insert(1);
        true
    }

    /// Checks if a challenge has been submitted for the given account
    pub fn has_challenge_for_account(&self, account_id: AccountId) -> bool {
        self.account_ids.contains_key(&account_id)
    }

    /// Returns the number of challenges submitted for the given API key.
    pub fn num_challenges_for_api_key(&self, key: &ApiKey) -> usize {
        self.challenges_per_key.get(key).copied().unwrap_or(0)
    }

    /// Cleanup expired challenges and update the number of challenges submitted per API key and
    /// account id.
    ///
    /// Challenges are expired if they are older than [`CHALLENGE_LIFETIME_SECONDS`] seconds.
    fn cleanup_expired_challenges(&mut self, current_time: u64) {
        let limit_timestamp = current_time - CHALLENGE_LIFETIME_SECONDS;

        for issuers in self.challenges.split_off(&limit_timestamp).into_values() {
            for (account_id, api_key) in issuers {
                let remove_api_key = self
                    .challenges_per_key
                    .get_mut(&api_key)
                    .map(|c| {
                        *c = c.saturating_sub(1);
                        *c == 0
                    })
                    .expect("challenge should have had a key entry");
                if remove_api_key {
                    self.challenges_per_key.remove(&api_key);
                }

                let remove_account_id = self
                    .account_ids
                    .get_mut(&account_id)
                    .map(|c| {
                        *c = c.saturating_sub(1);
                        *c == 0
                    })
                    .expect("challenge should have had an account entry");
                if remove_account_id {
                    self.account_ids.remove(&account_id);
                }
            }
        }
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
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("current timestamp should be greater than unix epoch")
                .as_secs();
            cache
                .lock()
                .expect("challenge cache lock should not be poisoned")
                .cleanup_expired_challenges(current_time);
        }
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

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
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        let account_id = 0_u128.try_into().unwrap();
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        // Submit challenge with wrong nonce - should fail
        let result = pow.submit_challenge(
            account_id,
            &api_key,
            &challenge.encode(),
            nonce + 1,
            current_time,
        );
        assert!(result.is_err());

        // Submit challenge with correct nonce - should succeed
        let result =
            pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time);
        assert!(result.is_ok());

        // Try to use the same challenge again with another account - should fail
        let account_id = 1_u128.try_into().unwrap();
        let result =
            pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time);
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

    #[tokio::test]
    async fn test_timestamp_validation() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        // Submit challenge with expired timestamp - should fail
        let result = pow.submit_challenge(
            account_id,
            &api_key,
            &challenge.encode(),
            nonce,
            current_time + CHALLENGE_LIFETIME_SECONDS + 1,
        );
        assert!(result.is_err());

        // Submit challenge with correct timestamp - should succeed
        let result =
            pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn account_id_is_rate_limited() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        // Solve first challenge
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result =
            pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time);
        assert!(result.is_ok());

        // Try to submit second challenge - should fail because of rate limiting
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        let result =
            pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time);
        assert!(result.is_err());
        assert!(matches!(result.err(), Some(InvalidMintRequest::RateLimited)));
    }

    #[tokio::test]
    async fn submit_challenge_and_check_difficulty() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");
        pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time)
            .unwrap();

        assert_eq!(pow.challenge_cache.lock().unwrap().num_challenges_for_api_key(&api_key), 1);
        assert_eq!(pow.get_difficulty(&api_key), 1);
    }

    #[tokio::test]
    async fn test_cleanup_expired_challenges() {
        let secret = create_test_secret();
        let pow = PoW::new(secret);
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        // build challenge manually with past timestamp to ensure that is expired
        let challenge = Challenge::from_parts(
            1,
            current_time - CHALLENGE_LIFETIME_SECONDS,
            account_id,
            api_key.clone(),
            Challenge::compute_signature(
                secret,
                1,
                current_time - CHALLENGE_LIFETIME_SECONDS,
                account_id,
                &api_key.inner(),
            ),
        );
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");

        pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time)
            .unwrap();

        // wait for cleanup
        tokio::time::sleep(Duration::from_secs(CLEANUP_INTERVAL_SECONDS + 1)).await;

        // check that the challenge is removed from the cache
        assert!(!pow.challenge_cache.lock().unwrap().has_challenge_for_account(account_id));
        assert_eq!(pow.challenge_cache.lock().unwrap().num_challenges_for_api_key(&api_key), 0);

        // submit second challenge - should succeed
        let challenge = pow.build_challenge(PowRequest { account_id, api_key: api_key.clone() });
        let nonce = find_pow_solution(&challenge, 10000).expect("Should find solution");
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        pow.submit_challenge(account_id, &api_key, &challenge.encode(), nonce, current_time)
            .unwrap();

        assert!(pow.challenge_cache.lock().unwrap().has_challenge_for_account(account_id));
        assert_eq!(pow.challenge_cache.lock().unwrap().num_challenges_for_api_key(&api_key), 1);
    }
}
