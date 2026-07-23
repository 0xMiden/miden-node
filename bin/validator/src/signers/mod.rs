mod kms;
pub use kms::{KmsSigner, decrypt_key_material};
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_protocol::Word;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature, SigningKey};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
use miden_protocol::crypto::hash::blake::Blake3_256;
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

/// Domain tag prefixed to the per-epoch key derivation payload, separating derived encryption key
/// seeds from any other use of the shared master secret.
pub const KEY_DERIVATION_DOMAIN: &[u8] = b"MIDEN_TX_ENCRYPTION_KEY_DERIVATION_V1";

/// Decryption counterpart to [`ValidatorSigner`] for the shared transaction encryption
/// (submission) key.
///
/// Unlike the signing key, the key material behind an implementation must be identical across
/// every validator in the set. This lets any validator unseal an encrypted submission, regardless
/// of which validator attested the encryption key to the client.
///
/// The encryption key rotates every epoch. An implementation derives the key for any epoch from
/// its shared key material, so all validators transition to the same new key at each epoch
/// boundary without coordination.
///
/// The interface deliberately does not assume that secret key bytes exist in the validator
/// process: an implementation may hold a local secret (see
/// [`LocalX25519TransactionInputDecrypter`]) or delegate decryption to an external system such as
/// a TEE that only exposes a decrypt operation.
#[tonic::async_trait]
pub trait TransactionInputDecrypter: Send + Sync {
    /// Returns the public metadata of the encryption key for the given epoch, together with the key
    /// scheduled to replace it at the next epoch boundary.
    async fn encryption_keys(&self, epoch: u16) -> anyhow::Result<EncryptionKeySet>;

    /// Decrypts transaction inputs sealed against the encryption key of the given epoch.
    ///
    /// Implementations should fall back to the previous epoch's key when unsealing with the
    /// given epoch's key fails, granting a one-epoch grace window to submissions sealed just
    /// before a rotation.
    ///
    /// The ciphertext is a serialized [`SealedMessage`].
    async fn decrypt_transaction_inputs(
        &self,
        epoch: u16,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<Vec<u8>>;

    /// Returns the secret key material of the given epoch's encryption key for archival, or `None`
    /// when the implementation cannot export secret key bytes (e.g. a TEE-held key) and handles its
    /// own archival.
    async fn export_secret_key(&self, epoch: u16) -> anyhow::Result<Option<Vec<u8>>>;
}

/// Public metadata of a shared transaction encryption key, in wire format.
///
/// These are the attested fields served by the `GetTransactionEncryptionKey` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionEncryptionKeyInfo {
    /// Wire identifier of the encryption scheme.
    pub scheme: u32,
    /// Opaque identifier of the encryption key.
    pub key_id: Vec<u8>,
    /// Raw public key bytes of the shared encryption key.
    pub public_key: Vec<u8>,
}

/// Public metadata of the next transaction encryption key, announced ahead of its scheduled
/// rotation, in wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextEncryptionKeyInfo {
    /// The key that replaces the current one at the rotation block.
    pub key: TransactionEncryptionKeyInfo,
    /// Block number at which the next key replaces the current one.
    pub rotation_block_num: u32,
}

/// The encryption key of one epoch together with its scheduled replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionKeySet {
    /// The key in effect during the epoch.
    pub current: TransactionEncryptionKeyInfo,
    /// The key that takes over at the next epoch boundary. `None` only when no next epoch exists
    /// (the epoch counter is saturated).
    pub next: Option<NextEncryptionKeyInfo>,
}

impl TransactionEncryptionKeyInfo {
    /// Returns the commitment signed by a validator to attest this key as the current encryption
    /// key.
    pub fn attestation_commitment(&self, genesis_commitment: Word) -> Word {
        attestation_commitment(
            self.scheme,
            &self.key_id,
            genesis_commitment,
            &self.public_key,
            None,
        )
    }
}

impl NextEncryptionKeyInfo {
    /// Returns the commitment signed by a validator to attest this key as the next encryption key,
    /// taking effect at the rotation block.
    pub fn attestation_commitment(&self, genesis_commitment: Word) -> Word {
        attestation_commitment(
            self.key.scheme,
            &self.key.key_id,
            genesis_commitment,
            &self.key.public_key,
            Some(self.rotation_block_num),
        )
    }
}

/// Computes the attestation commitment over explicit wire-format fields.
///
/// This is the single definition of the attestation payload. Verifiers (and tests) recompute the
/// commitment from response fields through this function, so any change to the payload layout
/// applies to both sides.
///
/// Computed as the Poseidon2 hash of `ATTESTATION_DOMAIN || scheme || len(key_id) || key_id ||
/// genesis_commitment || len(public_key) || public_key || role_suffix`, binding every field of
/// the attested key to the signature. The scheme and the length prefixes are encoded as 4 bytes
/// little-endian, and the length prefixes on the variable-width fields ensure no two field
/// combinations map to the same payload. Including the genesis commitment ties the attestation
/// to one chain, so it cannot be replayed on another network whose validator reuses the same
/// signing key.
///
/// `role_suffix` is a single `0` byte when the key is attested as the current key, or a `1` byte
/// followed by the rotation block number (4 bytes little-endian) when the key is attested as a
/// scheduled next key. Separating the roles means a next-key attestation cannot be presented as
/// a current-key attestation (or vice versa), and the rotation block cannot be altered without
/// invalidating the signature.
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

/// [`TransactionInputDecrypter`] backed by a locally provisioned shared master secret, from which
/// the per-epoch X25519 keys are derived.
pub struct LocalX25519TransactionInputDecrypter {
    master_secret: [u8; 32],
}

impl LocalX25519TransactionInputDecrypter {
    /// The IES scheme used for transaction input encryption.
    pub const SCHEME: IesScheme = IesScheme::X25519XChaCha20Poly1305;

    /// Constructs a decrypter from a locally provisioned shared master secret.
    ///
    /// The master secret is never used as an encryption key directly. The key for each epoch is
    /// derived from it, so every validator provisioned with the same secret derives the same
    /// per-epoch keys.
    pub fn new(master_secret: [u8; 32]) -> Self {
        Self { master_secret }
    }

    /// Returns the wire representation of [`Self::SCHEME`].
    pub fn scheme_id() -> u32 {
        u32::from(u8::from(Self::SCHEME))
    }

    /// Derives the encryption key for the given epoch.
    ///
    /// The key seed is `blake3(KEY_DERIVATION_DOMAIN || master_secret || epoch)`, with the epoch
    /// encoded as 2 bytes little-endian.
    pub fn key_for_epoch(&self, epoch: u16) -> KeyExchangeKey {
        let mut payload =
            Vec::with_capacity(KEY_DERIVATION_DOMAIN.len() + self.master_secret.len() + 2);
        payload.extend_from_slice(KEY_DERIVATION_DOMAIN);
        payload.extend_from_slice(&self.master_secret);
        payload.extend_from_slice(&epoch.to_le_bytes());
        let seed = Blake3_256::hash(&payload);
        KeyExchangeKey::read_from_bytes(seed.as_bytes())
            .expect("a 32-byte seed always forms a valid key exchange key")
    }

    /// Returns the public metadata of the encryption key for the given epoch.
    ///
    /// The key id is the first 4 bytes of the public key commitment.
    pub fn key_info_for_epoch(&self, epoch: u16) -> TransactionEncryptionKeyInfo {
        let public_key = self.key_for_epoch(epoch).public_key();
        TransactionEncryptionKeyInfo {
            scheme: Self::scheme_id(),
            key_id: public_key.to_commitment().to_bytes()[..4].to_vec(),
            public_key: public_key.to_bytes(),
        }
    }

    /// Returns the sealing key that clients use to encrypt messages to the validator set during the
    /// given epoch.
    #[cfg(test)]
    pub fn sealing_key_for_epoch(&self, epoch: u16) -> SealingKey {
        SealingKey::X25519XChaCha20Poly1305(self.key_for_epoch(epoch).public_key())
    }

    /// Attempts to unseal a message with the key of a single epoch.
    fn unseal_with_epoch_key(
        &self,
        epoch: u16,
        message: SealedMessage,
        associated_data: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        use anyhow::Context;

        UnsealingKey::X25519XChaCha20Poly1305(self.key_for_epoch(epoch))
            .unseal_bytes_with_associated_data(message, associated_data)
            .context("failed to unseal the transaction inputs")
    }
}

#[tonic::async_trait]
impl TransactionInputDecrypter for LocalX25519TransactionInputDecrypter {
    async fn encryption_keys(&self, epoch: u16) -> anyhow::Result<EncryptionKeySet> {
        let next = epoch.checked_add(1).map(|next_epoch| NextEncryptionKeyInfo {
            key: self.key_info_for_epoch(next_epoch),
            rotation_block_num: BlockNumber::from_epoch(next_epoch).as_u32(),
        });

        Ok(EncryptionKeySet {
            current: self.key_info_for_epoch(epoch),
            next,
        })
    }

    async fn decrypt_transaction_inputs(
        &self,
        epoch: u16,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        use anyhow::Context;

        let message = SealedMessage::read_from_bytes(ciphertext)
            .context("failed to deserialize the sealed message")?;

        match self.unseal_with_epoch_key(epoch, message.clone(), associated_data) {
            Ok(plaintext) => Ok(plaintext),
            Err(err) => match epoch.checked_sub(1) {
                Some(previous_epoch) => {
                    self.unseal_with_epoch_key(previous_epoch, message, associated_data)
                },
                None => Err(err),
            },
        }
    }

    async fn export_secret_key(&self, epoch: u16) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(Some(self.key_for_epoch(epoch).to_bytes()))
    }
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use rand::rng;

    use super::*;

    fn decrypter_from(secret: &[u8; 32]) -> LocalX25519TransactionInputDecrypter {
        LocalX25519TransactionInputDecrypter::new(*secret)
    }

    /// Loading the same master secret must yield the same per-epoch key metadata and attestation
    /// commitments on every validator instance.
    #[tokio::test]
    async fn same_secret_yields_same_public_material() {
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let keys_a = decrypter_from(&[7u8; 32]).encryption_keys(3).await.unwrap();
        let keys_b = decrypter_from(&[7u8; 32]).encryption_keys(3).await.unwrap();

        assert_eq!(keys_a, keys_b);
        assert_eq!(
            keys_a.current.attestation_commitment(genesis),
            keys_b.current.attestation_commitment(genesis)
        );
        assert_eq!(
            keys_a.next.as_ref().unwrap().attestation_commitment(genesis),
            keys_b.next.as_ref().unwrap().attestation_commitment(genesis)
        );
    }

    /// Different master secrets must yield different public keys and key ids.
    #[tokio::test]
    async fn different_secrets_yield_different_public_material() {
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let keys_a = decrypter_from(&[7u8; 32]).encryption_keys(3).await.unwrap();
        let keys_b = decrypter_from(&[8u8; 32]).encryption_keys(3).await.unwrap();

        assert_eq!(keys_a.current.scheme, keys_b.current.scheme);
        assert_ne!(keys_a.current.public_key, keys_b.current.public_key);
        assert_ne!(keys_a.current.key_id, keys_b.current.key_id);
        assert_ne!(
            keys_a.current.attestation_commitment(genesis),
            keys_b.current.attestation_commitment(genesis)
        );
    }

    /// Different epochs must yield different keys under the same master secret, and the next key of
    /// one epoch must equal the current key of the following epoch.
    #[tokio::test]
    async fn epochs_yield_distinct_but_consistent_keys() {
        let decrypter = decrypter_from(&[7u8; 32]);
        let keys_3 = decrypter.encryption_keys(3).await.unwrap();
        let keys_4 = decrypter.encryption_keys(4).await.unwrap();

        assert_ne!(keys_3.current.public_key, keys_4.current.public_key);
        assert_ne!(keys_3.current.key_id, keys_4.current.key_id);

        let next_3 = keys_3.next.unwrap();
        assert_eq!(next_3.key, keys_4.current);
        assert_eq!(next_3.rotation_block_num, BlockNumber::from_epoch(4).as_u32());
    }

    /// The current-key and next-key attestation commitments over the same key material must differ,
    /// and the next-key commitment must bind the rotation block.
    #[test]
    fn attestation_commitment_binds_role_and_rotation_block() {
        let genesis = Word::try_from([1u64, 2, 3, 4]).unwrap();
        let key = decrypter_from(&[7u8; 32]).key_info_for_epoch(3);

        let as_current = key.attestation_commitment(genesis);
        let as_next = NextEncryptionKeyInfo {
            key: key.clone(),
            rotation_block_num: BlockNumber::from_epoch(4).as_u32(),
        }
        .attestation_commitment(genesis);
        let as_next_other_block = NextEncryptionKeyInfo {
            key,
            rotation_block_num: BlockNumber::from_epoch(5).as_u32(),
        }
        .attestation_commitment(genesis);

        assert_ne!(as_current, as_next);
        assert_ne!(as_next, as_next_other_block);
    }

    /// At the final epoch no next epoch exists, so no next key can be announced.
    #[tokio::test]
    async fn final_epoch_has_no_next_key() {
        let keys = decrypter_from(&[7u8; 32]).encryption_keys(u16::MAX).await.unwrap();
        assert!(keys.next.is_none());
    }

    /// A message sealed against an epoch's sealing key must decrypt to the original plaintext, and
    /// decryption must reject a mismatched associated data or a mismatched key.
    #[tokio::test]
    async fn seal_decrypt_roundtrip() {
        let mut rng = rng();
        let decrypter = decrypter_from(&[7u8; 32]);
        let epoch = 3;
        let plaintext = b"transaction inputs";
        let associated_data = b"scheme|key_id|chain|tx";

        let sealed = decrypter
            .sealing_key_for_epoch(epoch)
            .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
            .unwrap()
            .to_bytes();
        let opened = decrypter
            .decrypt_transaction_inputs(epoch, &sealed, associated_data)
            .await
            .unwrap();
        assert_eq!(opened.as_slice(), plaintext);

        // Mismatched associated data must fail authentication.
        assert!(
            decrypter
                .decrypt_transaction_inputs(epoch, &sealed, b"wrong associated data")
                .await
                .is_err()
        );

        // A different master secret must fail to decrypt.
        let other = decrypter_from(&[8u8; 32]);
        assert!(other.decrypt_transaction_inputs(epoch, &sealed, associated_data).await.is_err());

        // Garbage ciphertext must fail to deserialize.
        assert!(
            decrypter
                .decrypt_transaction_inputs(epoch, b"not a sealed message", associated_data)
                .await
                .is_err()
        );
    }

    /// A message sealed during epoch `e` must remain decryptable during epoch `e + 1` (grace
    /// window) but not during epoch `e + 2`.
    #[tokio::test]
    async fn previous_epoch_key_grants_grace_window() {
        let mut rng = rng();
        let decrypter = decrypter_from(&[7u8; 32]);
        let associated_data = b"associated data";

        let sealed = decrypter
            .sealing_key_for_epoch(3)
            .seal_bytes_with_associated_data(&mut rng, b"inputs", associated_data)
            .unwrap()
            .to_bytes();

        assert!(decrypter.decrypt_transaction_inputs(4, &sealed, associated_data).await.is_ok());
        assert!(decrypter.decrypt_transaction_inputs(5, &sealed, associated_data).await.is_err());
    }
}
