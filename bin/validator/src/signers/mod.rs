mod kms;
pub use kms::KmsSigner;
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature, SigningKey};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::{
    KeyExchangeKey,
    PublicKey as EncryptionPublicKey,
};
use miden_protocol::crypto::ies::{IesError, IesScheme, SealedMessage, SealingKey, UnsealingKey};
use miden_protocol::utils::serde::Serializable;

// VALIDATOR SIGNER
// =================================================================================================

/// Signer that the Validator uses to sign blocks.
pub enum ValidatorSigner {
    Kms(KmsSigner),
    Local(SigningKey),
}

impl ValidatorSigner {
    /// Constructs a signer which uses an AWS KMS key for signing.
    ///
    /// See [`KmsSigner`] for details as to env var configuration and AWS IAM policies
    /// required to use this functionality.
    pub async fn new_kms(key_id: impl Into<String>) -> anyhow::Result<Self> {
        let kms_signer = KmsSigner::new(key_id).await?;
        Ok(Self::Kms(kms_signer))
    }

    /// Constructs a signer which uses a local secret key for signing.
    pub fn new_local(secret_key: SigningKey) -> Self {
        Self::Local(secret_key)
    }

    /// Returns the public key corresponding to the configured signer.
    pub fn public_key(&self) -> PublicKey {
        match self {
            Self::Kms(signer) => signer.public_key(),
            Self::Local(signer) => signer.public_key(),
        }
    }

    /// Signs a commitment using the configured signer.
    pub async fn sign_commitment(&self, commitment: Word) -> anyhow::Result<Signature> {
        let signature = match self {
            Self::Kms(signer) => signer.sign(commitment).await?,
            Self::Local(signer) => spawn_blocking_in_current_span({
                let signer = signer.clone();
                move || signer.sign(commitment)
            })
            .await
            .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic())),
        };

        Ok(signature)
    }
}

// VALIDATOR ENCRYPTOR
// =================================================================================================

/// Encryption-key counterpart to [`ValidatorSigner`], wrapping the shared transaction encryption
/// (submission) key.
///
/// Unlike the signing key, the secret material behind this type must be identical across every
/// validator in the set. This lets any validator unseal an encrypted submission, regardless of
/// which validator attested the encryption key to the client.
pub enum ValidatorEncryptor {
    Local(KeyExchangeKey),
}

impl ValidatorEncryptor {
    /// The IES scheme used for transaction input encryption.
    pub const SCHEME: IesScheme = IesScheme::X25519XChaCha20Poly1305;

    /// Domain tag prefixed to the attestation payload, separating key attestations from block
    /// header signatures made with the same validator key.
    pub const ATTESTATION_DOMAIN: &[u8] = b"MIDEN_TX_ENCRYPTION_KEY_ATTESTATION_V1";

    /// Constructs an encryptor from a locally provisioned shared secret.
    pub fn new_local(secret_key: KeyExchangeKey) -> Self {
        Self::Local(secret_key)
    }

    /// Returns the wire representation of [`Self::SCHEME`].
    pub fn scheme_id() -> u32 {
        u32::from(u8::from(Self::SCHEME))
    }

    /// Returns the public key of the shared encryption key.
    pub fn public_key(&self) -> EncryptionPublicKey {
        match self {
            Self::Local(key) => key.public_key(),
        }
    }

    /// Returns the opaque identifier of the current encryption key.
    pub fn key_id(&self) -> u32 {
        let commitment = self.public_key().to_commitment().to_bytes();
        u32::from_le_bytes(commitment[..4].try_into().expect("commitment is at least 4 bytes"))
    }

    /// Returns the sealing key that clients use to encrypt messages to the validator set.
    pub fn sealing_key(&self) -> SealingKey {
        SealingKey::X25519XChaCha20Poly1305(self.public_key())
    }

    /// Returns the commitment signed by a validator to attest the encryption key.
    ///
    /// Computed as the Poseidon2 hash of
    /// `ATTESTATION_DOMAIN || scheme || key_id || genesis_commitment || public_key`, binding
    /// every field of the attested response to the signature. The scheme and key id are bound at
    /// their full wire width (4 bytes little-endian each) so no wire value maps to another
    /// payload. Including the genesis commitment ties the attestation to one chain, so it cannot
    /// be replayed on another network whose validator reuses the same signing key.
    pub fn attestation_commitment(&self, genesis_commitment: Word) -> Word {
        Self::attestation_commitment_of(
            Self::scheme_id(),
            self.key_id(),
            genesis_commitment,
            &self.public_key().to_bytes(),
        )
    }

    /// Computes the attestation commitment over explicit wire-format fields.
    ///
    /// This is the single definition of the attestation payload. Verifiers (and tests) recompute
    /// the commitment from response fields through this function, so any change to the payload
    /// layout applies to both sides.
    pub fn attestation_commitment_of(
        scheme: u32,
        key_id: u32,
        genesis_commitment: Word,
        public_key: &[u8],
    ) -> Word {
        let genesis_commitment = genesis_commitment.to_bytes();
        let mut payload = Vec::with_capacity(
            Self::ATTESTATION_DOMAIN.len()
                + 2 * size_of::<u32>()
                + genesis_commitment.len()
                + public_key.len(),
        );
        payload.extend_from_slice(Self::ATTESTATION_DOMAIN);
        payload.extend_from_slice(&scheme.to_le_bytes());
        payload.extend_from_slice(&key_id.to_le_bytes());
        payload.extend_from_slice(&genesis_commitment);
        payload.extend_from_slice(public_key);
        miden_protocol::Hasher::hash(&payload)
    }

    /// Unseals a message encrypted against the shared encryption key.
    pub fn unseal_bytes_with_associated_data(
        &self,
        message: SealedMessage,
        associated_data: &[u8],
    ) -> Result<Vec<u8>, IesError> {
        match self {
            Self::Local(key) => UnsealingKey::X25519XChaCha20Poly1305(key.clone())
                .unseal_bytes_with_associated_data(message, associated_data),
        }
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::utils::serde::Deserializable;
    use rand::rng;

    use super::*;

    /// Loading the same shared secret must yield the same public key, key id, and attestation
    /// commitment on every validator instance.
    #[test]
    fn same_secret_yields_same_public_material() {
        let secret = [7u8; 32];
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let key_a = KeyExchangeKey::read_from_bytes(&secret).unwrap();
        let key_b = KeyExchangeKey::read_from_bytes(&secret).unwrap();
        let encryptor_a = ValidatorEncryptor::new_local(key_a);
        let encryptor_b = ValidatorEncryptor::new_local(key_b);

        assert_eq!(encryptor_a.public_key(), encryptor_b.public_key());
        assert_eq!(encryptor_a.key_id(), encryptor_b.key_id());
        assert_eq!(
            encryptor_a.attestation_commitment(genesis),
            encryptor_b.attestation_commitment(genesis)
        );
    }

    /// Different secrets must yield different public keys and key ids.
    #[test]
    fn different_secrets_yield_different_public_material() {
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let key_a = KeyExchangeKey::read_from_bytes(&[7u8; 32]).unwrap();
        let key_b = KeyExchangeKey::read_from_bytes(&[8u8; 32]).unwrap();
        let encryptor_a = ValidatorEncryptor::new_local(key_a);
        let encryptor_b = ValidatorEncryptor::new_local(key_b);

        assert_ne!(encryptor_a.public_key(), encryptor_b.public_key());
        assert_ne!(encryptor_a.key_id(), encryptor_b.key_id());
        assert_ne!(
            encryptor_a.attestation_commitment(genesis),
            encryptor_b.attestation_commitment(genesis)
        );
    }

    /// A message sealed against the encryptor's sealing key must unseal to the original plaintext,
    /// and unsealing must reject a mismatched associated data or a mismatched key.
    #[test]
    fn seal_unseal_roundtrip() {
        let mut rng = rng();
        let encryptor =
            ValidatorEncryptor::new_local(KeyExchangeKey::read_from_bytes(&[7u8; 32]).unwrap());
        let plaintext = b"transaction inputs";
        let associated_data = b"scheme|key_id|chain|tx";

        let sealed = encryptor
            .sealing_key()
            .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
            .unwrap();
        let opened = encryptor
            .unseal_bytes_with_associated_data(sealed.clone(), associated_data)
            .unwrap();
        assert_eq!(opened.as_slice(), plaintext);

        // Mismatched associated data must fail authentication.
        assert!(
            encryptor
                .unseal_bytes_with_associated_data(sealed.clone(), b"wrong associated data")
                .is_err()
        );

        // A different shared secret must fail to unseal.
        let other =
            ValidatorEncryptor::new_local(KeyExchangeKey::read_from_bytes(&[8u8; 32]).unwrap());
        assert!(other.unseal_bytes_with_associated_data(sealed, associated_data).is_err());
    }
}
