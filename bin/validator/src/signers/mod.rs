mod kms;
pub use kms::{KmsSigner, decrypt_key_material};
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature, SigningKey};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::{
    KeyExchangeKey,
    PublicKey as EncryptionPublicKey,
};
#[cfg(test)]
use miden_protocol::crypto::ies::SealingKey;
use miden_protocol::crypto::ies::{IesScheme, SealedMessage, UnsealingKey};
use miden_protocol::utils::serde::{Deserializable, Serializable};

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

// TRANSACTION INPUT DECRYPTER
// =================================================================================================

/// Domain tag prefixed to the attestation payload, separating key attestations from block header
/// signatures made with the same validator key.
pub const ATTESTATION_DOMAIN: &[u8] = b"MIDEN_TX_ENCRYPTION_KEY_ATTESTATION_V1";

/// Decryption counterpart to [`ValidatorSigner`] for the shared transaction encryption
/// (submission) key.
///
/// Unlike the signing key, the key material behind an implementation must be identical across
/// every validator in the set. This lets any validator unseal an encrypted submission, regardless
/// of which validator attested the encryption key to the client.
///
/// The interface deliberately does not assume that secret key bytes exist in the validator
/// process: an implementation may hold a local secret (see
/// [`LocalX25519TransactionInputDecrypter`]) or delegate decryption to an external system such as
/// a TEE that only exposes a decrypt operation.
#[tonic::async_trait]
pub trait TransactionInputDecrypter: Send + Sync {
    /// Returns the public metadata of the current encryption key.
    async fn encryption_key(&self) -> anyhow::Result<TransactionEncryptionKeyInfo>;

    /// Decrypts transaction inputs sealed against the current encryption key.
    ///
    /// The ciphertext is a serialized [`SealedMessage`].
    async fn decrypt_transaction_inputs(
        &self,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<Vec<u8>>;
}

/// Public metadata of the shared transaction encryption key, in wire format.
///
/// These are the attested fields served by the `GetTransactionEncryptionKey` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionEncryptionKeyInfo {
    /// Wire identifier of the encryption scheme.
    pub scheme: u32,
    /// Opaque identifier of the current encryption key.
    pub key_id: Vec<u8>,
    /// Raw public key bytes of the shared encryption key.
    pub public_key: Vec<u8>,
    /// The next encryption key when a rotation is scheduled.
    pub next_key: Option<NextEncryptionKeyInfo>,
}

/// Public metadata of the next transaction encryption key, announced ahead of a scheduled rotation,
/// in wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextEncryptionKeyInfo {
    /// Wire identifier of the next key's encryption scheme.
    pub scheme: u32,
    /// Opaque identifier of the next encryption key.
    pub key_id: Vec<u8>,
    /// Raw public key bytes of the next encryption key.
    pub public_key: Vec<u8>,
    /// Block number at which the next key replaces the current one.
    pub rotation_block_num: u32,
}

impl NextEncryptionKeyInfo {
    /// Returns the commitment signed by a validator to attest the encryption key as the scheduled
    /// next one, binding the rotation block to the signature.
    pub fn attestation_commitment(&self, genesis_commitment: Word) -> Word {
        attestation_commitment(
            self.scheme,
            &self.key_id,
            genesis_commitment,
            &self.public_key,
            Some(self.rotation_block_num),
        )
    }
}

impl TransactionEncryptionKeyInfo {
    /// Returns the commitment signed by a validator to attest the encryption key as the current
    /// one.
    pub fn attestation_commitment(&self, genesis_commitment: Word) -> Word {
        attestation_commitment(
            self.scheme,
            &self.key_id,
            genesis_commitment,
            &self.public_key,
            None,
        )
    }

    /// Returns the key metadata that becomes effective once the scheduled rotation block is
    /// reached: the next key promoted to the current one, with no further rotation scheduled.
    ///
    /// Returns `None` when no rotation is scheduled.
    pub fn post_rotation_info(&self) -> Option<Self> {
        self.next_key.as_ref().map(|next| Self {
            scheme: next.scheme,
            key_id: next.key_id.clone(),
            public_key: next.public_key.clone(),
            next_key: None,
        })
    }
}

/// Computes the attestation commitment over explicit wire-format fields.
///
/// This is the single definition of the attestation payload. Verifiers (and tests) recompute the
/// commitment from response fields through this function, so any change to the payload layout
/// applies to both sides.
///
/// Computed as the Poseidon2 hash of `ATTESTATION_DOMAIN || scheme || len(key_id) || key_id ||
/// genesis_commitment || len(public_key) || public_key || rotation_transcript`, binding every
/// attested field to the signature. The scheme, the rotation block number, and the length
/// prefixes are encoded as 4 bytes little-endian, and the length prefixes on the variable-width
/// fields ensure no two field combinations map to the same payload. Including the genesis
/// commitment ties the attestation to one chain, so it cannot be replayed on another network
/// whose validator reuses the same signing key.
///
/// `rotation_transcript` is a single `0` byte when the key is attested as the current one
/// (`rotation_block_num` is `None`), or `1` followed by the rotation block number when the key is
/// attested as a scheduled next key, so a next-key attestation cannot be replayed as a current
/// key nor its rotation block altered.
pub fn attestation_commitment(
    scheme: u32,
    key_id: &[u8],
    genesis_commitment: Word,
    public_key: &[u8],
    rotation_block_num: Option<u32>,
) -> Word {
    let genesis_commitment = genesis_commitment.to_bytes();
    let mut payload = Vec::with_capacity(
        ATTESTATION_DOMAIN.len()
            + 4 * size_of::<u32>()
            + key_id.len()
            + genesis_commitment.len()
            + public_key.len()
            + 1,
    );
    payload.extend_from_slice(ATTESTATION_DOMAIN);
    payload.extend_from_slice(&scheme.to_le_bytes());
    extend_with_length_prefixed(&mut payload, key_id, "key id");
    payload.extend_from_slice(&genesis_commitment);
    extend_with_length_prefixed(&mut payload, public_key, "public key");
    match rotation_block_num {
        None => payload.push(0),
        Some(rotation_block_num) => {
            payload.push(1);
            payload.extend_from_slice(&rotation_block_num.to_le_bytes());
        },
    }
    miden_protocol::Hasher::hash(&payload)
}

/// Appends a field to the attestation payload prefixed with its length as 4 bytes little-endian.
///
/// The length prefixes on variable-width fields keep the transcript injective: no two field
/// combinations map to the same payload.
fn extend_with_length_prefixed(payload: &mut Vec<u8>, field: &[u8], name: &str) {
    let len = u32::try_from(field.len())
        .unwrap_or_else(|_| panic!("{name} length must fit in u32"))
        .to_le_bytes();
    payload.extend_from_slice(&len);
    payload.extend_from_slice(field);
}

/// [`TransactionInputDecrypter`] backed by a locally provisioned X25519 shared secret.
///
/// When a key rotation is scheduled (see [`Self::with_next_key`]) the decrypter also holds the
/// next shared secret. The next key is announced through [`TransactionEncryptionKeyInfo::next_key`]
/// ahead of the rotation, and submissions sealed against either key can be unsealed, so
/// submissions sealed just before the rotation block still decrypt after it.
pub struct LocalX25519TransactionInputDecrypter {
    secret_key: KeyExchangeKey,
    next_key: Option<NextKey>,
}

/// The next shared secret and its scheduled activation block.
struct NextKey {
    secret_key: KeyExchangeKey,
    rotation_block_num: u32,
}

impl LocalX25519TransactionInputDecrypter {
    /// The IES scheme used for transaction input encryption.
    pub const SCHEME: IesScheme = IesScheme::X25519XChaCha20Poly1305;

    /// Constructs a decrypter from a locally provisioned shared secret.
    pub fn new(secret_key: KeyExchangeKey) -> Self {
        Self { secret_key, next_key: None }
    }

    /// Schedules a key rotation: `secret_key` replaces the current shared secret at
    /// `rotation_block_num`.
    ///
    /// Like the current secret, the next secret and the rotation block must be identical across
    /// every validator in the set.
    #[must_use]
    pub fn with_next_key(mut self, secret_key: KeyExchangeKey, rotation_block_num: u32) -> Self {
        self.next_key = Some(NextKey { secret_key, rotation_block_num });
        self
    }

    /// Returns the wire representation of [`Self::SCHEME`].
    pub fn scheme_id() -> u32 {
        u32::from(u8::from(Self::SCHEME))
    }

    /// Returns the public key of the shared encryption key.
    pub fn public_key(&self) -> EncryptionPublicKey {
        self.secret_key.public_key()
    }

    /// Returns the opaque identifier of the current encryption key: the first 4 bytes of the public
    /// key commitment.
    pub fn key_id(&self) -> Vec<u8> {
        key_id_of(&self.public_key())
    }

    /// Returns the sealing key that clients use to encrypt messages to the validator set.
    #[cfg(test)]
    pub fn sealing_key(&self) -> SealingKey {
        SealingKey::X25519XChaCha20Poly1305(self.public_key())
    }

    /// Returns the sealing key of the scheduled next encryption key, if any.
    #[cfg(test)]
    pub fn next_sealing_key(&self) -> Option<SealingKey> {
        self.next_key
            .as_ref()
            .map(|next| SealingKey::X25519XChaCha20Poly1305(next.secret_key.public_key()))
    }

    /// Attempts to unseal `message` with `secret_key`.
    fn unseal_with(
        secret_key: &KeyExchangeKey,
        message: SealedMessage,
        associated_data: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        use anyhow::Context;

        UnsealingKey::X25519XChaCha20Poly1305(secret_key.clone())
            .unseal_bytes_with_associated_data(message, associated_data)
            .context("failed to unseal the transaction inputs")
    }
}

/// Returns the opaque identifier of an encryption key: the first 4 bytes of the public key
/// commitment.
fn key_id_of(public_key: &EncryptionPublicKey) -> Vec<u8> {
    public_key.to_commitment().to_bytes()[..4].to_vec()
}

#[tonic::async_trait]
impl TransactionInputDecrypter for LocalX25519TransactionInputDecrypter {
    async fn encryption_key(&self) -> anyhow::Result<TransactionEncryptionKeyInfo> {
        Ok(TransactionEncryptionKeyInfo {
            scheme: Self::scheme_id(),
            key_id: self.key_id(),
            public_key: self.public_key().to_bytes(),
            next_key: self.next_key.as_ref().map(|next| {
                let public_key = next.secret_key.public_key();
                NextEncryptionKeyInfo {
                    scheme: Self::scheme_id(),
                    key_id: key_id_of(&public_key),
                    public_key: public_key.to_bytes(),
                    rotation_block_num: next.rotation_block_num,
                }
            }),
        })
    }

    async fn decrypt_transaction_inputs(
        &self,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        use anyhow::Context;

        let message = SealedMessage::read_from_bytes(ciphertext)
            .context("failed to deserialize the sealed message")?;
        // Submissions may be sealed against the current key or, around a scheduled rotation,
        // against the next key. The sealed message does not identify its key, so try the current
        // key first and fall back to the next one.
        match Self::unseal_with(&self.secret_key, message.clone(), associated_data) {
            Ok(plaintext) => Ok(plaintext),
            Err(err) => match &self.next_key {
                Some(next) => Self::unseal_with(&next.secret_key, message, associated_data),
                None => Err(err),
            },
        }
    }
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use miden_protocol::utils::serde::Deserializable;
    use rand::rng;

    use super::*;

    fn decrypter_from(secret: &[u8; 32]) -> LocalX25519TransactionInputDecrypter {
        LocalX25519TransactionInputDecrypter::new(KeyExchangeKey::read_from_bytes(secret).unwrap())
    }

    /// Loading the same shared secret must yield the same key metadata and attestation commitment
    /// on every validator instance.
    #[tokio::test]
    async fn same_secret_yields_same_public_material() {
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let info_a = decrypter_from(&[7u8; 32]).encryption_key().await.unwrap();
        let info_b = decrypter_from(&[7u8; 32]).encryption_key().await.unwrap();

        assert_eq!(info_a, info_b);
        assert_eq!(info_a.attestation_commitment(genesis), info_b.attestation_commitment(genesis));
    }

    /// Different secrets must yield different public keys and key ids.
    #[tokio::test]
    async fn different_secrets_yield_different_public_material() {
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let info_a = decrypter_from(&[7u8; 32]).encryption_key().await.unwrap();
        let info_b = decrypter_from(&[8u8; 32]).encryption_key().await.unwrap();

        assert_eq!(info_a.scheme, info_b.scheme);
        assert_ne!(info_a.public_key, info_b.public_key);
        assert_ne!(info_a.key_id, info_b.key_id);
        assert_ne!(info_a.attestation_commitment(genesis), info_b.attestation_commitment(genesis));
    }

    /// Around a scheduled rotation, messages sealed against either the current or the next key must
    /// decrypt, while an unrelated key must still be rejected.
    #[tokio::test]
    async fn decrypts_messages_sealed_against_either_key() {
        let mut rng = rng();
        let next_secret = KeyExchangeKey::read_from_bytes(&[8u8; 32]).unwrap();
        let decrypter = decrypter_from(&[7u8; 32]).with_next_key(next_secret, 42);
        let plaintext = b"transaction inputs";
        let associated_data = b"scheme|key_id|chain|tx";

        let sealed_current = decrypter
            .sealing_key()
            .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
            .unwrap()
            .to_bytes();
        assert_eq!(
            decrypter
                .decrypt_transaction_inputs(&sealed_current, associated_data)
                .await
                .unwrap()
                .as_slice(),
            plaintext,
        );

        let sealed_next = decrypter
            .next_sealing_key()
            .expect("a scheduled rotation must expose the next sealing key")
            .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
            .unwrap()
            .to_bytes();
        assert_eq!(
            decrypter
                .decrypt_transaction_inputs(&sealed_next, associated_data)
                .await
                .unwrap()
                .as_slice(),
            plaintext,
        );

        // A message sealed against an unrelated key must be rejected by both keyring entries.
        let sealed_other = decrypter_from(&[9u8; 32])
            .sealing_key()
            .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
            .unwrap()
            .to_bytes();
        assert!(
            decrypter
                .decrypt_transaction_inputs(&sealed_other, associated_data)
                .await
                .is_err()
        );

        // Mismatched associated data must fail for either key.
        assert!(
            decrypter
                .decrypt_transaction_inputs(&sealed_next, b"wrong associated data")
                .await
                .is_err()
        );
    }

    /// A message sealed against the decrypter's sealing key must decrypt to the original plaintext,
    /// and decryption must reject a mismatched associated data or a mismatched key.
    #[tokio::test]
    async fn seal_decrypt_roundtrip() {
        let mut rng = rng();
        let decrypter = decrypter_from(&[7u8; 32]);
        let plaintext = b"transaction inputs";
        let associated_data = b"scheme|key_id|chain|tx";

        let sealed = decrypter
            .sealing_key()
            .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
            .unwrap()
            .to_bytes();
        let opened = decrypter.decrypt_transaction_inputs(&sealed, associated_data).await.unwrap();
        assert_eq!(opened.as_slice(), plaintext);

        // Mismatched associated data must fail authentication.
        assert!(
            decrypter
                .decrypt_transaction_inputs(&sealed, b"wrong associated data")
                .await
                .is_err()
        );

        // A different shared secret must fail to decrypt.
        let other = decrypter_from(&[8u8; 32]);
        assert!(other.decrypt_transaction_inputs(&sealed, associated_data).await.is_err());

        // Garbage ciphertext must fail to deserialize.
        assert!(
            decrypter
                .decrypt_transaction_inputs(b"not a sealed message", associated_data)
                .await
                .is_err()
        );
    }
}
