use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::server::get_tokens::MintRequestError;

// API KEY
// ================================================================================================

const API_KEY_PREFIX: &str = "miden_faucet_";

/// The API key is a random 32-byte array.
///
/// It can be encoded as a string using the `encode` method and decoded back to bytes using the
/// `decode` method.
#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ApiKey([u8; 32]);

impl ApiKey {
    /// Generates a random API key.
    pub fn generate(rng: &mut impl Rng) -> Self {
        let mut api_key = [0u8; 32];
        rng.fill(&mut api_key);
        Self(api_key)
    }

    /// Creates an API key from a byte array.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Encodes the API key into a base64 string, prefixed with `API_KEY_PREFIX`.
    pub fn encode(&self) -> String {
        format!("{API_KEY_PREFIX}{}", BASE64_STANDARD.encode(self.0))
    }

    /// Returns the inner bytes of the API key.
    pub fn inner(&self) -> [u8; 32] {
        self.0
    }

    /// Decodes the API key from a string.
    pub fn decode(api_key_str: &str) -> Result<Self, MintRequestError> {
        let api_key_str = api_key_str.trim_start_matches(API_KEY_PREFIX).to_string();
        let bytes = BASE64_STANDARD
            .decode(api_key_str.as_bytes())
            .map_err(|_| MintRequestError::InvalidApiKey(api_key_str.clone()))?;

        let api_key =
            Self(bytes.try_into().map_err(|_| MintRequestError::InvalidApiKey(api_key_str))?);
        Ok(api_key)
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    use crate::server::ApiKey;
    use crate::server::api_key::API_KEY_PREFIX;

    #[test]
    fn api_key_encode_and_decode() {
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let api_key = ApiKey::generate(&mut rng);

        let encoded_key = api_key.encode();
        assert!(encoded_key.starts_with(API_KEY_PREFIX));

        let decoded_key = ApiKey::decode(&encoded_key).unwrap();
        assert_eq!(decoded_key.inner().len(), 32);
        assert_eq!(decoded_key.inner(), api_key.inner());
    }
}
