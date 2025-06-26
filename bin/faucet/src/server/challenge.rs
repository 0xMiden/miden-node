use miden_objects::{
    account::AccountId,
    utils::{Deserializable, Serializable},
};
use miden_tx::utils::{ToHex, hex_to_bytes};
use serde::{Serialize, Serializer};
use sha3::{Digest, Sha3_256};

use super::get_tokens::MintRequestError;
use crate::server::ApiKey;

/// The size of the encoded challenge in bytes.
const CHALLENGE_ENCODED_SIZE: usize = 95;

/// A challenge for proof-of-work validation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Challenge {
    pub(crate) difficulty: usize,
    pub(crate) timestamp: u64,
    pub(crate) account_id: AccountId,
    pub(crate) api_key: ApiKey,
    pub(crate) signature: [u8; 32],
}

impl Serialize for Challenge {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Challenge", 3)?;
        state.serialize_field("challenge", &self.encode())?;
        state.serialize_field("difficulty", &self.difficulty)?;
        state.serialize_field("timestamp", &self.timestamp)?;
        state.end()
    }
}

impl Challenge {
    /// Creates a new challenge with the given parameters.
    /// The signature is computed internally using the provided secret.
    pub fn new(
        difficulty: usize,
        timestamp: u64,
        account_id: AccountId,
        api_key: ApiKey,
        secret: [u8; 32],
    ) -> Self {
        let signature =
            Self::compute_signature(secret, difficulty, timestamp, account_id, &api_key.inner());
        Self {
            difficulty,
            timestamp,
            account_id,
            api_key,
            signature,
        }
    }

    /// Creates a challenge from existing parts (used for decoding).
    pub fn from_parts(
        difficulty: usize,
        timestamp: u64,
        account_id: AccountId,
        api_key: ApiKey,
        signature: [u8; 32],
    ) -> Self {
        Self {
            difficulty,
            timestamp,
            account_id,
            api_key,
            signature,
        }
    }

    /// Decodes the challenge and verifies that the signature part of the challenge is valid
    /// in the context of the specified secret.
    pub fn decode(value: &str, secret: [u8; 32]) -> Result<Self, MintRequestError> {
        // Parse the hex-encoded challenge string
        let bytes: [u8; CHALLENGE_ENCODED_SIZE] =
            hex_to_bytes(value).map_err(|_| MintRequestError::MissingPowParameters)?;

        // SAFETY: Length of the bytes is enforced above.
        let difficulty = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let timestamp = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let account_id = AccountId::read_from_bytes(&bytes[16..31]).unwrap();
        let api_key_bytes: [u8; 32] = bytes[31..63].try_into().unwrap();
        let api_key = ApiKey::new(api_key_bytes);
        let signature: [u8; 32] = bytes[63..CHALLENGE_ENCODED_SIZE].try_into().unwrap();

        // Verify the signature
        let expected_signature =
            Self::compute_signature(secret, difficulty, timestamp, account_id, &api_key_bytes);
        if signature == expected_signature {
            Ok(Self::from_parts(difficulty, timestamp, account_id, api_key, signature))
        } else {
            Err(MintRequestError::ServerSignaturesDoNotMatch)
        }
    }

    /// Encodes the challenge into a hex string.
    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(CHALLENGE_ENCODED_SIZE);
        bytes.extend_from_slice(&(self.difficulty as u64).to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(&self.account_id.to_bytes());
        bytes.extend_from_slice(&self.api_key.inner());
        bytes.extend_from_slice(&self.signature);
        bytes.to_hex_with_prefix()
    }

    /// Checks whether the provided nonce satisfies the difficulty requirement encoded in the
    /// challenge.
    pub fn validate_pow(&self, nonce: u64) -> bool {
        let mut hasher = Sha3_256::new();
        hasher.update(self.encode());
        hasher.update(nonce.to_le_bytes());
        let hash = hasher.finalize();

        let leading_zeros = hash.iter().take_while(|&b| *b == 0).count();

        leading_zeros >= self.difficulty
    }

    /// Checks if the challenge timestamp is expired.
    ///
    /// # Arguments
    /// * `current_time` - The current timestamp in seconds since the UNIX epoch.
    pub fn is_expired(&self, current_time: u64, challenge_expiration: u64) -> bool {
        let diff = current_time.checked_sub(self.timestamp).unwrap_or(u64::MAX);
        diff > challenge_expiration
    }

    /// Computes the signature for a challenge.
    pub fn compute_signature(
        secret: [u8; 32],
        difficulty: usize,
        timestamp: u64,
        account_id: AccountId,
        api_key: &[u8],
    ) -> [u8; 32] {
        let mut hasher = Sha3_256::new();
        hasher.update(secret);
        hasher.update(difficulty.to_le_bytes());
        hasher.update(timestamp.to_le_bytes());
        let account_id_bytes: [u8; AccountId::SERIALIZED_SIZE] = account_id.into();
        hasher.update(account_id_bytes);
        hasher.update(api_key);
        hasher.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    use super::*;

    fn create_test_secret() -> [u8; 32] {
        let mut secret = [0u8; 32];
        secret[..12].copy_from_slice(b"miden-faucet");
        secret
    }

    #[test]
    fn challenge_serialize_and_deserialize_json() {
        let secret = [1u8; 32];
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);
        let challenge = Challenge::new(2, 1_234_567_890, account_id, api_key, secret);

        // Test that it serializes to the expected JSON format
        let json = serde_json::to_string(&challenge).unwrap();

        // Should contain the expected fields
        assert!(json.contains("\"challenge\":"));
        assert!(json.contains("\"difficulty\":2"));
        assert!(json.contains("\"timestamp\":1234567890"));

        // Parse back to verify structure
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("challenge").is_some());
        assert!(parsed.get("difficulty").is_some());
        assert!(parsed.get("timestamp").is_some());
        assert_eq!(parsed["difficulty"], 2);
        assert_eq!(parsed["timestamp"], 1_234_567_890);
    }

    #[test]
    fn test_challenge_encode_decode() {
        let secret = create_test_secret();
        let difficulty = 3;
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);

        let challenge = Challenge::new(difficulty, current_time, account_id, api_key, secret);

        let encoded = challenge.encode();
        let decoded = Challenge::decode(&encoded, secret).unwrap();

        assert_eq!(challenge, decoded);
    }

    #[test]
    fn test_timestamp_validation() {
        let secret = create_test_secret();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let expiration_time = 30;
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);

        // Valid timestamp (current time)
        let challenge = Challenge::new(1, current_time, account_id, api_key.clone(), secret);
        assert!(!challenge.is_expired(current_time, expiration_time));

        // Expired timestamp (too old)
        let old_timestamp = current_time - expiration_time - 10;
        let challenge = Challenge::new(1, old_timestamp, account_id, api_key, secret);
        assert!(challenge.is_expired(current_time, expiration_time));
    }
}
