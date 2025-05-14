use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, extract::State, response::IntoResponse};
use miden_tx::utils::ToHex;
use rand::{Rng, rng};
use serde::Serialize;
use sha3::{Digest, Sha3_256};

use super::{Server, get_tokens::InvalidRequest};

const DIFFICULTY: u64 = 5;
const SERVER_TIMESTAMP_TOLERANCE_SECONDS: u64 = 30;

#[derive(Serialize)]
struct PoWResponse {
    seed: String,
    difficulty: u64,
    server_signature: String,
    timestamp: u64,
}

/// Get the server signature.
///
/// The server signature is the result of hashing the server salt, the seed, and the timestamp.
pub(crate) fn get_server_signature(server_salt: &str, seed: &str, timestamp: u64) -> String {
    let mut hasher = Sha3_256::new();
    hasher.update(server_salt);
    hasher.update(seed);
    hasher.update(timestamp.to_string().as_bytes());
    hasher.finalize().to_hex()
}

/// Generate a random hex string of specified length in nibbles.
fn random_hex_string(num_nibbles: usize) -> String {
    // Generate random bytes
    let mut rng = rng();
    let mut random_bytes = vec![0u8; num_nibbles / 2];
    rng.fill(&mut random_bytes[..]);

    // Convert bytes to hex string
    random_bytes.iter().fold(String::new(), |acc, byte| format!("{acc}{byte:02x}"))
}

/// Get a seed to be used by a client as the `PoW` seed.
///
/// The seed is a 64 character random hex string.
pub(crate) async fn get_pow_seed(State(server): State<Server>) -> impl IntoResponse {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    let random_seed = random_hex_string(32);

    let server_signature = get_server_signature(&server.pow_salt, &random_seed, timestamp);

    Json(PoWResponse {
        seed: random_seed,
        difficulty: DIFFICULTY,
        server_signature,
        timestamp,
    })
}

/// Check the server signature.
///
/// The server signature is the result of hashing the server salt and the seed.
pub(crate) fn check_server_signature(
    server_salt: &str,
    server_signature: &str,
    seed: &str,
    timestamp: u64,
) -> Result<(), InvalidRequest> {
    let hash = get_server_signature(server_salt, seed, timestamp);

    if hash != server_signature {
        return Err(InvalidRequest::ServerSignaturesDoNotMatch);
    }

    Ok(())
}

/// Check a `PoW` solution.
///
/// * `seed` - The seed to be used by the client as the `PoW` seed.
/// * `solution` - The solution to be checked.
///
/// The solution is valid if the hash of the seed and the solution has at least `DIFFICULTY`
/// leading zeros.
///
/// Returns `true` if the solution is valid, `false` otherwise.
pub(crate) fn check_pow_solution(seed: &str, solution: u64) -> Result<(), InvalidRequest> {
    let mut hasher = Sha3_256::new();
    hasher.update(seed);
    hasher.update(solution.to_string().as_bytes());
    let hash = &hasher.finalize().to_hex();

    let leading_zeros = hash.chars().take_while(|&c| c == '0').count();
    if leading_zeros < DIFFICULTY as usize {
        return Err(InvalidRequest::InvalidPoW);
    }

    Ok(())
}

/// Check the received timestamp.
///
/// The timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of the
/// current time.
pub(crate) fn check_server_timestamp(timestamp: u64) -> Result<(), InvalidRequest> {
    let server_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    if (server_timestamp - timestamp) > SERVER_TIMESTAMP_TOLERANCE_SECONDS {
        return Err(InvalidRequest::ExpiredServerTimestamp(timestamp, server_timestamp));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use sha3::{Digest, Sha3_256};

    use super::*;

    fn find_pow_solution(seed: &str) -> u64 {
        let mut solution = 0;
        loop {
            let mut hasher = Sha3_256::new();
            hasher.update(seed);
            hasher.update(solution.to_string().as_bytes());
            let hash = &hasher.finalize().to_hex();
            let leading_zeros = hash.chars().take_while(|&c| c == '0').count();
            if leading_zeros >= DIFFICULTY as usize {
                return solution;
            }

            solution += 1;
        }
    }

    #[test]
    fn test_check_server_signature() {
        let server_salt = "miden-faucet";
        let seed = "0x1234567890abcdef";
        let timestamp = 1_234_567_890;

        let mut hasher = Sha3_256::new();
        hasher.update(server_salt);
        hasher.update(seed);
        hasher.update(timestamp.to_string().as_bytes());
        let server_signature = hasher.finalize().to_hex();

        let result = check_server_signature(server_salt, &server_signature, seed, timestamp);
        assert!(result.is_ok());

        let solution = find_pow_solution(seed);
        let result = check_pow_solution(seed, solution);
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_server_signature_failure() {
        let server_salt = "miden-faucet";
        let seed = "0x1234567890abcdef";
        let timestamp = 1_234_567_890;
        let server_signature = "0x1234567890abcdef";

        let result = check_server_signature(server_salt, server_signature, seed, timestamp);
        assert!(result.is_err());

        let solution = 1_234_567_890;
        let result = check_pow_solution(seed, solution);
        assert!(result.is_err());
    }
}
