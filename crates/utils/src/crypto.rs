use miden_objects::crypto::rand::RpoRandomCoin;
use miden_objects::{Felt, Word};
use rand::Rng;

/// Creates a new RPO Random Coin with random seed
pub fn get_rpo_random_coin<T: Rng>(rng: &mut T) -> RpoRandomCoin {
    let auth_seed: [u64; 4] = rng.random();
    let rng_seed = Word::from(auth_seed.map(Felt::new));

    RpoRandomCoin::new(rng_seed)
}
