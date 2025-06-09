use miden_tx::utils::{ToHex, hex_to_bytes};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};

use super::get_tokens::InvalidRequest;

/// The tolerance for the server timestamp.
///
/// The server timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of
/// the current time.
pub(crate) const SERVER_TIMESTAMP_TOLERANCE_SECONDS: u64 = 30;

/// A challenge for proof-of-work validation.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct Challenge {
    pub(crate) difficulty: usize,
    pub(crate) timestamp: u64,
    pub(crate) signature: [u8; 32],
}

impl Challenge {
    /// Creates a new challenge with the given parameters.
    /// The signature is computed internally using the provided secret.
    pub fn new(difficulty: usize, timestamp: u64, secret: [u8; 32]) -> Self {
        let signature = Self::compute_signature(secret, difficulty, timestamp);
        Self { difficulty, timestamp, signature }
    }

    /// Creates a challenge from existing parts (used for decoding).
    pub fn from_parts(difficulty: usize, timestamp: u64, signature: [u8; 32]) -> Self {
        Self { difficulty, timestamp, signature }
    }

    /// Decodes the challenge and verifies that the signature part of the challenge is valid
    /// in the context of the specified secret.
    pub fn decode(value: &str, secret: [u8; 32]) -> Result<Self, InvalidRequest> {
        // Add the "0x" prefix to the hex string, otherwise the hex_to_bytes function will fail.
        let value = "0x".to_string() + value;
        // Parse the hex-encoded challenge string
        let bytes = hex_to_bytes::<48>(&value).map_err(|_| InvalidRequest::MissingPowParameters)?;

        if bytes.len() != 48 {
            // 8 + 8 + 32 bytes
            return Err(InvalidRequest::MissingPowParameters);
        }

        // SAFETY: Length of 48 is enforced above.
        let difficulty = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let timestamp = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let signature: [u8; 32] = bytes[16..48].try_into().unwrap();

        // Verify the signature
        let expected_signature = Self::compute_signature(secret, difficulty, timestamp);
        if signature == expected_signature {
            Ok(Self::from_parts(difficulty, timestamp, signature))
        } else {
            Err(InvalidRequest::ServerSignaturesDoNotMatch)
        }
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

    /// Checks if the challenge timestamp is not expired.
    pub fn is_not_expired(&self, current_time: u64) -> bool {
        (current_time - self.timestamp) <= SERVER_TIMESTAMP_TOLERANCE_SECONDS
    }

    /// Computes the signature for a challenge.
    pub fn compute_signature(secret: [u8; 32], difficulty: usize, timestamp: u64) -> [u8; 32] {
        let mut hasher = Sha3_256::new();
        hasher.update(secret);
        hasher.update(difficulty.to_be_bytes());
        hasher.update(timestamp.to_be_bytes());
        hasher.finalize().into()
    }
}
