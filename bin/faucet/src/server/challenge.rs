use miden_objects::{
    account::AccountId,
    utils::{Deserializable, Serializable},
};
use miden_tx::utils::{ToHex, hex_to_bytes};
use serde::{Serialize, Serializer};
use sha3::{Digest, Sha3_256};

use super::get_tokens::InvalidMintRequest;
use crate::server::ApiKey;

/// The lifetime of a challenge.
///
/// A challenge is valid if it is within `CHALLENGE_LIFETIME_SECONDS` seconds of the current time.
pub(crate) const CHALLENGE_LIFETIME_SECONDS: u64 = 30;

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
        secret: [u8; 32],
        account_id: AccountId,
        api_key: ApiKey,
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
    pub fn decode(value: &str, secret: [u8; 32]) -> Result<Self, InvalidMintRequest> {
        // Parse the hex-encoded challenge string
        let bytes: [u8; 95] =
            hex_to_bytes(value).map_err(|_| InvalidMintRequest::MissingPowParameters)?;

        let difficulty = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let timestamp = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let account_id = AccountId::read_from_bytes(&bytes[16..31]).unwrap();
        let api_key_bytes: [u8; 32] = bytes[31..63].try_into().unwrap();
        let api_key = ApiKey::new(api_key_bytes);
        let signature: [u8; 32] = bytes[63..95].try_into().unwrap();

        // Verify the signature
        let expected_signature =
            Self::compute_signature(secret, difficulty, timestamp, account_id, &api_key_bytes);
        if signature == expected_signature {
            Ok(Self::from_parts(difficulty, timestamp, account_id, api_key, signature))
        } else {
            Err(InvalidMintRequest::ServerSignaturesDoNotMatch)
        }
    }

    /// Encodes the challenge into a hex string.
    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(63);
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
    pub fn is_expired(&self, current_time: u64) -> bool {
        (current_time - self.timestamp) > CHALLENGE_LIFETIME_SECONDS
    }

    /// Computes the signature for a challenge.
    fn compute_signature(
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

    use super::*;

    #[test]
    fn challenge_serialize_and_deserialize_json() {
        let secret = [1u8; 32];
        let account_id = [0u8; AccountId::SERIALIZED_SIZE].try_into().unwrap();
        let challenge = Challenge::new(2, 1_234_567_890, secret, account_id, ApiKey::generate());

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
        let mut secret = [0u8; 32];
        secret[..12].copy_from_slice(b"miden-faucet");
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

        assert_eq!(challenge, decoded);
    }
}
