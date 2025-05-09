use std::{
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{Json, response::IntoResponse};
use miden_tx::utils::ToHex;
use rand::{Rng, rng};
use serde::Serialize;
use sha3::{Digest, Sha3_256};

const DIFFICULTY: u64 = 5;
const SERVER_TIMESTAMP_TOLERANCE_SECONDS: u64 = 30;

static SERVER_SALT: LazyLock<String> = LazyLock::new(|| {
    std::env::var("MIDEN_FAUCET_SERVER_SALT").expect("MIDEN_FAUCET_SERVER_SALT must be set")
});

#[derive(Serialize)]
struct PoWResponse {
    seed: String,
    difficulty: u64,
    server_signature: String,
    timestamp: u64,
}

/// Generate a random hex string of specified length
fn random_hex_string(len: usize) -> String {
    const HEX_CHARS: &[u8] = b"0123456789abcdef";
    let mut rng = rng();
    let random_bytes: Vec<u8> = (0..len)
        .map(|_| HEX_CHARS[rng.random_range(0..HEX_CHARS.len())] as char)
        .collect::<String>()
        .into_bytes();

    let hex_string = String::from_utf8(random_bytes).unwrap();
    format!("0x{hex_string}")
}

/// Get a seed to be used by a client as the `PoW` seed.
///
/// The seed is a 64 character random hex string.
pub(crate) async fn get_pow_seed() -> impl IntoResponse {
    // Generate a 64 character random hex string
    let random_seed = random_hex_string(64);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();
    let mut hasher = Sha3_256::new();
    hasher.update(SERVER_SALT.as_bytes());
    hasher.update(&random_seed);
    hasher.update(timestamp.to_string().as_bytes());
    let server_signature = hasher.finalize().to_hex();

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
pub(crate) fn check_server_signature(server_signature: &str, seed: &str, timestamp: u64) -> bool {
    let mut hasher = Sha3_256::new();
    hasher.update(SERVER_SALT.as_bytes());
    hasher.update(seed);
    hasher.update(timestamp.to_string().as_bytes());
    let hash = &hasher.finalize().to_hex();

    hash == server_signature
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
pub(crate) fn check_pow_solution(seed: &str, solution: u64) -> bool {
    let mut hasher = Sha3_256::new();
    hasher.update(seed);
    hasher.update(solution.to_string().as_bytes());
    let hash = &hasher.finalize().to_hex()[2..];

    let leading_zeros = hash.chars().take(DIFFICULTY as usize).filter(|&c| c == '0').count();
    leading_zeros >= DIFFICULTY as usize
}

/// Check the received timestamp.
///
/// The timestamp is valid if it is within `SERVER_TIMESTAMP_TOLERANCE_SECONDS` seconds of the
/// current time.
pub(crate) fn check_server_timestamp(timestamp: u64) -> bool {
    let server_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    (server_timestamp - timestamp) <= SERVER_TIMESTAMP_TOLERANCE_SECONDS
}
