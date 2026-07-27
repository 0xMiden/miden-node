//! Sealing of transaction inputs against the validator set's shared encryption key.
//!
//! This module is the single definition of the associated-data transcript, so the sealing side
//! (clients and the node's own submitters) and the unsealing side (the validator) cannot drift.
//! A drift would not fail to compile: it would reject every submission at runtime with an opaque
//! AEAD error, so the transcript is pinned by a golden vector in the tests below.

use miden_protocol::Word;
use miden_protocol::crypto::dsa::eddsa_25519_sha512::PublicKey as EncryptionPublicKey;
use miden_protocol::crypto::ies::{IesScheme, SealingKey};
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::{Deserializable, Serializable};

use crate::generated as proto;

/// Domain tag prefixed to the associated data of sealed transaction inputs.
///
/// Separates this transcript from every other use of the same key material, in particular from the
/// key attestation signed with the validator's signing key.
pub const TX_INPUT_SEAL_DOMAIN: &[u8] = b"MIDEN_TX_INPUT_SEAL_V1";

/// Upper bound on the length of an encryption key identifier.
///
/// Key identifiers are 4 bytes today (the leading bytes of the public key commitment). The bound
/// exists so that a hostile or misconfigured key endpoint cannot drive an unbounded allocation, and
/// so that the length cast in the transcript cannot overflow.
pub const MAX_KEY_ID_LEN: usize = 64;

/// Wire identifier of the only IES scheme the node currently supports.
const SCHEME_X25519_XCHACHA20_POLY1305: u32 = 1;

// ASSOCIATED DATA
// ================================================================================================

/// Builds the associated data authenticating a sealed set of transaction inputs.
///
/// This is the single definition of the transcript. Both sides derive it independently and it is
/// never transmitted, so a mismatch surfaces as an authentication failure rather than as accepted
/// but unauthenticated data.
///
/// The layout is `TX_INPUT_SEAL_DOMAIN || scheme || len(key_id) || key_id || genesis_commitment ||
/// transaction_id`, where the scheme and the length prefix are 4 bytes little-endian. The domain tag
/// is a fixed-width constant, `scheme` is fixed-width, `key_id` is length-prefixed and the two
/// trailing fields are a fixed 32 bytes each, so no two distinct inputs produce the same transcript.
///
/// Each binding serves a purpose:
/// - `scheme` and `key_id` tie the blob to one key, so inputs sealed against a retired key fail to
///   authenticate rather than silently decrypting.
/// - `genesis_commitment` ties the blob to one network. This matters in practice because every
///   development stack shares the same insecure default key, so without it a blob captured on one
///   network would replay onto another.
/// - `transaction_id` ties the blob to one transaction, so a captured blob cannot be replayed onto a
///   different transaction.
///
/// Deliberately absent is the serialized transaction. The RPC rebuilds `ProvenTransaction` with
/// output-note decorators stripped before forwarding a submission, so binding those bytes would
/// reject every relayed transaction. The transaction id is invariant under that rebuild, which is
/// why it is bound instead.
pub fn transaction_inputs_associated_data(
    scheme: u32,
    key_id: &[u8],
    genesis_commitment: Word,
    tx_id: TransactionId,
) -> Vec<u8> {
    let genesis_commitment = genesis_commitment.to_bytes();
    let tx_id = tx_id.as_word().to_bytes();
    let mut transcript = Vec::with_capacity(
        TX_INPUT_SEAL_DOMAIN.len()
            + 2 * size_of::<u32>()
            + key_id.len()
            + genesis_commitment.len()
            + tx_id.len(),
    );
    transcript.extend_from_slice(TX_INPUT_SEAL_DOMAIN);
    transcript.extend_from_slice(&scheme.to_le_bytes());
    // Callers bound `key_id` to MAX_KEY_ID_LEN, so this cast cannot realistically fail. Saturate
    // rather than panic anyway: this runs inside a request handler on the validator.
    let key_id_len = u32::try_from(key_id.len()).unwrap_or(u32::MAX);
    transcript.extend_from_slice(&key_id_len.to_le_bytes());
    transcript.extend_from_slice(key_id);
    transcript.extend_from_slice(&genesis_commitment);
    transcript.extend_from_slice(&tx_id);
    transcript
}

// ERRORS
// ================================================================================================

/// Failure to build a sealer from a served encryption key, or to seal with it.
#[derive(Debug, thiserror::Error)]
pub enum TransactionInputSealError {
    #[error("encryption key scheme is unspecified")]
    UnspecifiedScheme,
    #[error("unsupported encryption key scheme {0}")]
    UnsupportedScheme(i32),
    #[error("encryption key id is {len} bytes, which exceeds the maximum of {MAX_KEY_ID_LEN}")]
    KeyIdTooLong { len: usize },
    #[error("invalid encryption public key")]
    InvalidPublicKey(#[source] miden_protocol::utils::serde::DeserializationError),
    #[error("failed to seal the transaction inputs")]
    Seal(#[source] miden_protocol::crypto::ies::IesError),
}

// SEALER
// ================================================================================================

/// Seals transaction inputs against the validator set's shared encryption key.
///
/// Built from a `GetTransactionEncryptionKey` response and reusable for any number of transactions.
/// Holding one avoids re-fetching the key per submission; callers should discard it when the
/// validator reports an unknown key id, which is how a key change is detected.
#[derive(Debug, Clone)]
pub struct TransactionInputSealer {
    scheme: u32,
    key_id: Vec<u8>,
    sealing_key: SealingKey,
    genesis_commitment: Word,
}

impl TransactionInputSealer {
    /// Builds a sealer from a served encryption key and the genesis commitment of the network the
    /// inputs will be submitted to.
    pub fn new(
        key: proto::transaction::TransactionEncryptionKey,
        genesis_commitment: Word,
    ) -> Result<Self, TransactionInputSealError> {
        // Match the wire value explicitly. The proto enum reserves 0 for "unspecified" while
        // `IesScheme` uses 0 for K256, so converting straight from the raw value would silently
        // select a scheme the node does not serve instead of reporting an error.
        let scheme = match key.scheme {
            0 => return Err(TransactionInputSealError::UnspecifiedScheme),
            1 => SCHEME_X25519_XCHACHA20_POLY1305,
            other => return Err(TransactionInputSealError::UnsupportedScheme(other)),
        };
        debug_assert_eq!(
            u32::from(u8::from(IesScheme::X25519XChaCha20Poly1305)),
            scheme,
            "the wire scheme identifier must match the crypto discriminant",
        );

        if key.key_id.len() > MAX_KEY_ID_LEN {
            return Err(TransactionInputSealError::KeyIdTooLong { len: key.key_id.len() });
        }

        let public_key = EncryptionPublicKey::read_from_bytes(&key.public_key)
            .map_err(TransactionInputSealError::InvalidPublicKey)?;

        Ok(Self {
            scheme,
            key_id: key.key_id,
            sealing_key: SealingKey::X25519XChaCha20Poly1305(public_key),
            genesis_commitment,
        })
    }

    /// The identifier of the key this sealer seals against.
    pub fn key_id(&self) -> &[u8] {
        &self.key_id
    }

    /// Seals `transaction_inputs` for the transaction identified by `tx_id`.
    ///
    /// `transaction_inputs` must be the encoding of
    /// [`miden_protocol::transaction::TransactionInputs::to_bytes`].
    ///
    /// Each call draws a fresh ephemeral key, so sealing the same inputs twice is safe and yields
    /// different ciphertexts.
    pub fn seal(
        &self,
        tx_id: TransactionId,
        transaction_inputs: &[u8],
    ) -> Result<proto::transaction::SealedTransactionInputs, TransactionInputSealError> {
        let associated_data = transaction_inputs_associated_data(
            self.scheme,
            &self.key_id,
            self.genesis_commitment,
            tx_id,
        );
        let sealed = self
            .sealing_key
            .seal_bytes_with_associated_data(&mut rand::rng(), transaction_inputs, &associated_data)
            .map_err(TransactionInputSealError::Seal)?;

        Ok(proto::transaction::SealedTransactionInputs {
            key_id: self.key_id.clone(),
            ciphertext: sealed.to_bytes(),
        })
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;

    use super::*;

    const TEST_KEY_ID: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

    fn genesis() -> Word {
        Word::from([1u32, 2, 3, 4])
    }

    fn tx_id(seed: u32) -> TransactionId {
        TransactionId::new(
            Word::from([seed, 0, 0, 0]),
            Word::from([0, seed, 0, 0]),
            Word::from([0, 0, seed, 0]),
            Word::from([0, 0, 0, seed]),
        )
    }

    /// Pins the transcript byte-for-byte, which also pins *which* fields it binds.
    ///
    /// Both sides derive the transcript through this one function, so a change to it would pass
    /// every other test in the workspace and surface only as every submission on the network failing
    /// to authenticate. This vector is the only thing that catches that.
    #[test]
    fn associated_data_is_stable() {
        let ad = transaction_inputs_associated_data(1, &TEST_KEY_ID, genesis(), tx_id(10));

        let mut expected = Vec::new();
        expected.extend_from_slice(b"MIDEN_TX_INPUT_SEAL_V1");
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&TEST_KEY_ID);
        expected.extend_from_slice(&genesis().to_bytes());
        expected.extend_from_slice(&tx_id(10).as_word().to_bytes());

        assert_eq!(ad, expected);
        // 22-byte tag + 4 scheme + 4 length + 4 key id + 32 genesis + 32 transaction id.
        assert_eq!(ad.len(), 98);
    }

    /// Scheme 0 means "unspecified" on the wire but K256 in `IesScheme`, so converting the raw value
    /// would silently select a scheme the node does not serve instead of erroring.
    #[test]
    fn sealer_rejects_unspecified_scheme() {
        let key = proto::transaction::TransactionEncryptionKey {
            scheme: 0,
            key_id: TEST_KEY_ID.to_vec(),
            public_key: KeyExchangeKey::read_from_bytes(&[7u8; 32])
                .unwrap()
                .public_key()
                .to_bytes(),
            attestations: Vec::new(),
            next_key: None,
        };

        assert_matches!(
            TransactionInputSealer::new(key, genesis()),
            Err(TransactionInputSealError::UnspecifiedScheme)
        );
    }
}
