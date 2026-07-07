use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::{Felt, Word};
use rand::RngExt;

/// Creates a new [`RandomCoin`] with random seed.
pub fn get_random_coin<T: RngExt>(rng: &mut T) -> RandomCoin {
    let auth_seed: [u64; 4] = rng.random();
    let rng_seed = Word::from(auth_seed.map(Felt::new_unchecked));

    RandomCoin::new(rng_seed)
}
