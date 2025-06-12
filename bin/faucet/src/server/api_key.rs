use base64::{Engine, prelude::BASE64_STANDARD};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use crate::server::get_tokens::InvalidRequest;

// API KEY
// ================================================================================================

const API_KEY_PREFIX: &str = "miden_faucet_";

/// The API key is a random 32-byte array.
#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct ApiKey([u8; 32]);

impl ApiKey {
    /// Generates a random API key.
    pub fn generate() -> Self {
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let mut api_key = [0u8; 32];
        rng.fill(&mut api_key);
        Self(api_key)
    }

    /// Creates a new API key from a byte array.
    pub fn new(api_key: [u8; 32]) -> Self {
        Self(api_key)
    }

    /// Encodes the API key into a base64 string, prefixed with `API_KEY_PREFIX`.
    pub fn encode(&self) -> String {
        format!("{API_KEY_PREFIX}{}", BASE64_STANDARD.encode(self.0))
    }

    /// Returns the inner bytes of the API key.
    pub fn inner(&self) -> [u8; 32] {
        self.0
    }

    /// Decodes the API key from a string option. If the string is `None`, returns a default API
    /// key.
    pub fn decode(api_key_str: Option<String>) -> Result<Self, InvalidRequest> {
        let Some(api_key_str) = api_key_str else {
            return Ok(Self::default());
        };
        let api_key_str = api_key_str.trim_start_matches(API_KEY_PREFIX).to_string();
        let bytes = BASE64_STANDARD
            .decode(api_key_str.as_bytes())
            .map_err(|_| InvalidRequest::InvalidApiKey(api_key_str.clone()))?;

        let api_key =
            Self::new(bytes.try_into().map_err(|_| InvalidRequest::InvalidApiKey(api_key_str))?);
        Ok(api_key)
    }
}

#[cfg(test)]
mod tests {
    use crate::server::{ApiKey, api_key::API_KEY_PREFIX};

    #[test]
    fn api_key_encode_and_decode() {
        let api_key = ApiKey::generate();

        let encoded_key = api_key.encode();
        assert!(encoded_key.starts_with(API_KEY_PREFIX));

        let decoded_key = ApiKey::decode(Some(encoded_key)).unwrap();
        assert_eq!(decoded_key.inner().len(), 32);
        assert_eq!(decoded_key.inner(), api_key.inner());
    }
}
