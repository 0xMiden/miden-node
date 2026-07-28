mod kms;

pub use miden_node_proto::domain::transaction_encryption::{
    NextTransactionEncryptionKey,
    TransactionEncryptionKeyInfo,
    TransactionEncryptionKeySchedule,
};
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_protocol::Word;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature, SigningKey};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
#[cfg(test)]
use miden_protocol::crypto::ies::SealingKey;
use miden_protocol::crypto::ies::{IesScheme, SealedMessage, UnsealingKey};
use miden_protocol::utils::serde::{Deserializable, Serializable};

pub use self::kms::{KmsSigner, decrypt_key_material};

// VALIDATOR SIGNER
// =================================================================================================

/// Signer that the Validator uses to sign blocks.
pub enum ValidatorSigner {
    Kms(KmsSigner),
    Local(SigningKey),
    #[cfg(test)]
    Failing(PublicKey),
    #[cfg(test)]
    Blocking(PublicKey),
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

    /// Constructs a signer which uses a local secret key.
    pub fn new_local(secret_key: SigningKey) -> Self {
        Self::Local(secret_key)
    }

    #[cfg(test)]
    pub(crate) fn new_failing(public_key: PublicKey) -> Self {
        Self::Failing(public_key)
    }

    #[cfg(test)]
    pub(crate) fn new_blocking(public_key: PublicKey) -> Self {
        Self::Blocking(public_key)
    }

    /// Returns the public key corresponding to the configured signer.
    pub fn public_key(&self) -> PublicKey {
        match self {
            Self::Kms(signer) => signer.public_key(),
            Self::Local(signer) => signer.public_key(),
            #[cfg(test)]
            Self::Failing(public_key) | Self::Blocking(public_key) => public_key.clone(),
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
            #[cfg(test)]
            Self::Failing(_) => anyhow::bail!("test signer unavailable"),
            #[cfg(test)]
            Self::Blocking(_) => std::future::pending::<Signature>().await,
        };

        Ok(signature)
    }
}

// TRANSACTION INPUT DECRYPTER
// =================================================================================================

/// Operation-only provider for transaction input encryption keys.
///
/// Implementations own key identifiers, scheduling, grace policy, and secret storage. The
/// validator only requests public schedule metadata and asks the provider to decrypt with the key
/// identifier carried by a submission.
#[tonic::async_trait]
pub trait TransactionInputDecrypter: Send + Sync {
    /// Returns the schedule effective at `chain_tip`.
    ///
    /// Providers must keep this schedule unchanged within an epoch. Manual schedule updates happen
    /// at epoch boundaries so an older attestation from the same epoch cannot suppress a newly
    /// announced key.
    async fn encryption_key_schedule(
        &self,
        chain_tip: BlockNumber,
    ) -> anyhow::Result<TransactionEncryptionKeySchedule>;

    /// Decrypts inputs using the caller-supplied opaque key identifier.
    ///
    /// The provider must distinguish an announced key that is not active yet, an expired grace
    /// key, and an identifier it does not own.
    async fn decrypt_transaction_inputs(
        &self,
        key_id: &[u8],
        chain_tip: BlockNumber,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, TransactionInputDecryptionError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TransactionInputDecryptionError {
    #[error("transaction encryption key {key_id} does not activate until block {activation}")]
    PrematureKey { key_id: String, activation: BlockNumber },
    #[error("transaction encryption key {key_id} expired at block {expired_at}")]
    ExpiredKey { key_id: String, expired_at: BlockNumber },
    #[error("unknown transaction encryption key {key_id}")]
    UnknownKey { key_id: String },
    #[error("failed to deserialize the sealed transaction inputs")]
    InvalidCiphertext(#[source] anyhow::Error),
    #[error("failed to decrypt the transaction inputs")]
    DecryptionFailed(#[source] anyhow::Error),
}

#[derive(Clone)]
struct LocalEncryptionKey {
    secret_key: KeyExchangeKey,
    info: TransactionEncryptionKeyInfo,
}

impl LocalEncryptionKey {
    fn new(secret_key: KeyExchangeKey) -> Self {
        let public_key = secret_key.public_key();
        let info = TransactionEncryptionKeyInfo {
            scheme: LocalX25519TransactionInputDecrypter::scheme_id(),
            key_id: public_key.to_commitment().to_bytes(),
            public_key: public_key.to_bytes(),
        };
        Self { secret_key, info }
    }
}

#[derive(Clone)]
struct ScheduledLocalEncryptionKey {
    key: LocalEncryptionKey,
    activation_block_num: BlockNumber,
}

/// Local X25519 provider with an optional manually scheduled replacement key.
///
/// Constructing this provider does not derive keys or choose a rotation cadence. When a next key
/// is configured, its declared epoch-boundary activation is enforced from the trusted chain tip.
/// One previous key may be retained for decryption through the current key's activation epoch.
pub struct LocalX25519TransactionInputDecrypter {
    previous: Option<ScheduledLocalEncryptionKey>,
    current: ScheduledLocalEncryptionKey,
    next: Option<ScheduledLocalEncryptionKey>,
}

impl LocalX25519TransactionInputDecrypter {
    /// The IES scheme used for transaction input encryption.
    pub const SCHEME: IesScheme = IesScheme::X25519XChaCha20Poly1305;

    /// Constructs a provider with one key active since genesis and no scheduled rotation.
    pub fn new(secret_key: KeyExchangeKey) -> Self {
        Self {
            previous: None,
            current: ScheduledLocalEncryptionKey {
                key: LocalEncryptionKey::new(secret_key),
                activation_block_num: BlockNumber::GENESIS,
            },
            next: None,
        }
    }

    /// Constructs a complete manual key schedule for startup.
    pub fn from_schedule(
        previous: Option<(KeyExchangeKey, BlockNumber)>,
        current: (KeyExchangeKey, BlockNumber),
        next: Option<(KeyExchangeKey, BlockNumber)>,
    ) -> anyhow::Result<Self> {
        let previous = previous
            .map(|(key, activation_block_num)| scheduled_local_key(key, activation_block_num))
            .transpose()?;
        let current = scheduled_local_key(current.0, current.1)?;
        let next = next
            .map(|(key, activation_block_num)| scheduled_local_key(key, activation_block_num))
            .transpose()?;

        if let Some(previous) = &previous {
            anyhow::ensure!(
                previous.activation_block_num < current.activation_block_num,
                "previous key activation must be before current key activation"
            );
            anyhow::ensure!(
                previous.key.info.key_id != current.key.info.key_id,
                "previous and current keys must have distinct ids"
            );
        }
        if let Some(next) = &next {
            anyhow::ensure!(
                current.activation_block_num < next.activation_block_num,
                "next key activation must be after current key activation"
            );
            anyhow::ensure!(
                current.key.info.key_id != next.key.info.key_id,
                "current and next keys must have distinct ids"
            );
            anyhow::ensure!(
                previous
                    .as_ref()
                    .is_none_or(|previous| previous.key.info.key_id != next.key.info.key_id),
                "previous and next keys must have distinct ids"
            );
        }

        Ok(Self { previous, current, next })
    }

    /// Adds a manually chosen replacement key at a future epoch boundary.
    pub fn with_scheduled_rotation(
        mut self,
        secret_key: KeyExchangeKey,
        activation_block_num: BlockNumber,
    ) -> anyhow::Result<Self> {
        ensure_epoch_boundary(activation_block_num)?;
        anyhow::ensure!(
            activation_block_num > self.current.activation_block_num,
            "next key activation must be after current key activation"
        );
        let next = LocalEncryptionKey::new(secret_key);
        anyhow::ensure!(
            next.info.key_id != self.current.key.info.key_id,
            "current and next keys must have distinct ids"
        );
        anyhow::ensure!(
            self.previous
                .as_ref()
                .is_none_or(|previous| previous.key.info.key_id != next.info.key_id),
            "previous and next keys must have distinct ids"
        );
        self.next = Some(ScheduledLocalEncryptionKey { key: next, activation_block_num });
        Ok(self)
    }

    /// Returns the wire representation of [`Self::SCHEME`].
    pub fn scheme_id() -> u32 {
        u32::from(u8::from(Self::SCHEME))
    }

    #[cfg(test)]
    fn sealing_key(&self) -> SealingKey {
        SealingKey::X25519XChaCha20Poly1305(self.current.key.secret_key.public_key())
    }

    #[cfg(test)]
    fn previous_sealing_key(&self) -> Option<SealingKey> {
        self.previous.as_ref().map(|previous| {
            SealingKey::X25519XChaCha20Poly1305(previous.key.secret_key.public_key())
        })
    }

    #[cfg(test)]
    fn next_sealing_key(&self) -> Option<SealingKey> {
        self.next
            .as_ref()
            .map(|next| SealingKey::X25519XChaCha20Poly1305(next.key.secret_key.public_key()))
    }

    fn unseal(
        key: &LocalEncryptionKey,
        message: SealedMessage,
        associated_data: &[u8],
    ) -> Result<Vec<u8>, TransactionInputDecryptionError> {
        use anyhow::Context;

        UnsealingKey::X25519XChaCha20Poly1305(key.secret_key.clone())
            .unseal_bytes_with_associated_data(message, associated_data)
            .context("failed to unseal with the selected transaction encryption key")
            .map_err(TransactionInputDecryptionError::DecryptionFailed)
    }

    fn key_for_decryption(
        &self,
        key_id: &[u8],
        chain_tip: BlockNumber,
    ) -> Result<&LocalEncryptionKey, TransactionInputDecryptionError> {
        if let Some(previous) = &self.previous
            && key_id == previous.key.info.key_id
        {
            if chain_tip < previous.activation_block_num {
                return Err(TransactionInputDecryptionError::PrematureKey {
                    key_id: hex::encode(key_id),
                    activation: previous.activation_block_num,
                });
            }
            if let Some(grace_expiry) = self
                .current
                .activation_block_num
                .block_epoch()
                .checked_add(1)
                .map(BlockNumber::from_epoch)
                && chain_tip >= grace_expiry
            {
                return Err(TransactionInputDecryptionError::ExpiredKey {
                    key_id: hex::encode(key_id),
                    expired_at: grace_expiry,
                });
            }
            return Ok(&previous.key);
        }

        if key_id == self.current.key.info.key_id {
            if chain_tip < self.current.activation_block_num {
                return Err(TransactionInputDecryptionError::PrematureKey {
                    key_id: hex::encode(key_id),
                    activation: self.current.activation_block_num,
                });
            }

            if let Some(next) = &self.next
                && chain_tip >= next.activation_block_num
                && let Some(grace_expiry) = next
                    .activation_block_num
                    .block_epoch()
                    .checked_add(1)
                    .map(BlockNumber::from_epoch)
                && chain_tip >= grace_expiry
            {
                return Err(TransactionInputDecryptionError::ExpiredKey {
                    key_id: hex::encode(key_id),
                    expired_at: grace_expiry,
                });
            }

            return Ok(&self.current.key);
        }

        if let Some(next) = &self.next
            && key_id == next.key.info.key_id
        {
            if chain_tip < next.activation_block_num {
                return Err(TransactionInputDecryptionError::PrematureKey {
                    key_id: hex::encode(key_id),
                    activation: next.activation_block_num,
                });
            }
            return Ok(&next.key);
        }

        Err(TransactionInputDecryptionError::UnknownKey { key_id: hex::encode(key_id) })
    }
}

#[tonic::async_trait]
impl TransactionInputDecrypter for LocalX25519TransactionInputDecrypter {
    async fn encryption_key_schedule(
        &self,
        chain_tip: BlockNumber,
    ) -> anyhow::Result<TransactionEncryptionKeySchedule> {
        let previous_is_required = self.current.activation_block_num != BlockNumber::GENESIS
            && self
                .current
                .activation_block_num
                .block_epoch()
                .checked_add(1)
                .map(BlockNumber::from_epoch)
                .is_none_or(|grace_expiry| chain_tip < grace_expiry);
        anyhow::ensure!(
            !previous_is_required || self.previous.is_some(),
            "a previous key is required through the current key's activation epoch"
        );

        if chain_tip < self.current.activation_block_num {
            let previous = self.previous.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "current key does not activate until block {} and no previous key is configured",
                    self.current.activation_block_num
                )
            })?;
            return Ok(TransactionEncryptionKeySchedule {
                current_key: previous.key.info.clone(),
                current_key_activation_block_num: previous.activation_block_num,
                next_key: Some(NextTransactionEncryptionKey {
                    key: self.current.key.info.clone(),
                    activation_block_num: self.current.activation_block_num,
                }),
            });
        }

        if let Some(next) = &self.next
            && chain_tip >= next.activation_block_num
        {
            return Ok(TransactionEncryptionKeySchedule {
                current_key: next.key.info.clone(),
                current_key_activation_block_num: next.activation_block_num,
                next_key: None,
            });
        }

        Ok(TransactionEncryptionKeySchedule {
            current_key: self.current.key.info.clone(),
            current_key_activation_block_num: self.current.activation_block_num,
            next_key: self.next.as_ref().map(|next| NextTransactionEncryptionKey {
                key: next.key.info.clone(),
                activation_block_num: next.activation_block_num,
            }),
        })
    }

    async fn decrypt_transaction_inputs(
        &self,
        key_id: &[u8],
        chain_tip: BlockNumber,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, TransactionInputDecryptionError> {
        use anyhow::Context;

        let message = SealedMessage::read_from_bytes(ciphertext)
            .context("failed to deserialize the sealed message")
            .map_err(TransactionInputDecryptionError::InvalidCiphertext)?;
        let key = self.key_for_decryption(key_id, chain_tip)?;
        Self::unseal(key, message, associated_data)
    }
}

fn scheduled_local_key(
    secret_key: KeyExchangeKey,
    activation_block_num: BlockNumber,
) -> anyhow::Result<ScheduledLocalEncryptionKey> {
    ensure_epoch_boundary(activation_block_num)?;
    Ok(ScheduledLocalEncryptionKey {
        key: LocalEncryptionKey::new(secret_key),
        activation_block_num,
    })
}

fn ensure_epoch_boundary(block_num: BlockNumber) -> anyhow::Result<()> {
    anyhow::ensure!(
        BlockNumber::from_epoch(block_num.block_epoch()) == block_num,
        "key activation block must be an epoch boundary"
    );
    Ok(())
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use rand::rng;

    use super::*;

    fn key(seed: u8) -> KeyExchangeKey {
        KeyExchangeKey::read_from_bytes(&[seed; 32]).unwrap()
    }

    fn decrypter(seed: u8) -> LocalX25519TransactionInputDecrypter {
        LocalX25519TransactionInputDecrypter::new(key(seed))
    }

    fn scheduled_decrypter() -> LocalX25519TransactionInputDecrypter {
        decrypter(7)
            .with_scheduled_rotation(key(8), BlockNumber::from_epoch(1))
            .unwrap()
    }

    fn repeated_rotation_decrypter() -> LocalX25519TransactionInputDecrypter {
        LocalX25519TransactionInputDecrypter::from_schedule(
            Some((key(7), BlockNumber::GENESIS)),
            (key(8), BlockNumber::from_epoch(1)),
            Some((key(9), BlockNumber::from_epoch(2))),
        )
        .unwrap()
    }

    fn seal(sealing_key: &SealingKey, plaintext: &[u8], associated_data: &[u8]) -> Vec<u8> {
        sealing_key
            .seal_bytes_with_associated_data(&mut rng(), plaintext, associated_data)
            .unwrap()
            .to_bytes()
    }

    fn repeated_rotation_ciphertexts(
        decrypter: &LocalX25519TransactionInputDecrypter,
        associated_data: &[u8],
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            seal(&decrypter.previous_sealing_key().unwrap(), b"previous", associated_data),
            seal(&decrypter.sealing_key(), b"current", associated_data),
            seal(&decrypter.next_sealing_key().unwrap(), b"next", associated_data),
        )
    }

    async fn assert_decrypts(
        decrypter: &LocalX25519TransactionInputDecrypter,
        key_id: &[u8],
        chain_tip: BlockNumber,
        ciphertext: &[u8],
        associated_data: &[u8],
        expected: &[u8],
    ) {
        assert_eq!(
            decrypter
                .decrypt_transaction_inputs(key_id, chain_tip, ciphertext, associated_data,)
                .await
                .unwrap(),
            expected
        );
    }

    async fn assert_premature(
        decrypter: &LocalX25519TransactionInputDecrypter,
        key_id: &[u8],
        chain_tip: BlockNumber,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) {
        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(key_id, chain_tip, ciphertext, associated_data)
                .await,
            Err(TransactionInputDecryptionError::PrematureKey { .. })
        ));
    }

    async fn assert_expired(
        decrypter: &LocalX25519TransactionInputDecrypter,
        key_id: &[u8],
        chain_tip: BlockNumber,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) {
        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(key_id, chain_tip, ciphertext, associated_data)
                .await,
            Err(TransactionInputDecryptionError::ExpiredKey { .. })
        ));
    }

    #[tokio::test]
    async fn same_key_yields_same_public_material() {
        let a = decrypter(7).encryption_key_schedule(BlockNumber::from(1)).await.unwrap();
        let b = decrypter(7).encryption_key_schedule(BlockNumber::from_epoch(4)).await.unwrap();

        assert_eq!(a, b);
        assert_eq!(a.current_key.key_id.len(), 32);
    }

    #[tokio::test]
    async fn different_keys_yield_different_provider_owned_ids() {
        let a = decrypter(7).encryption_key_schedule(BlockNumber::GENESIS).await.unwrap();
        let b = decrypter(8).encryption_key_schedule(BlockNumber::GENESIS).await.unwrap();

        assert_ne!(a.current_key.public_key, b.current_key.public_key);
        assert_ne!(a.current_key.key_id, b.current_key.key_id);
    }

    #[tokio::test]
    async fn scheduled_key_activates_only_at_declared_epoch_boundary() {
        let decrypter = scheduled_decrypter();
        let before = decrypter
            .encryption_key_schedule(BlockNumber::from((1 << 16) - 1))
            .await
            .unwrap();
        let at = decrypter.encryption_key_schedule(BlockNumber::from_epoch(1)).await.unwrap();

        assert!(before.next_key.is_some());
        assert_eq!(before.next_key.unwrap().activation_block_num, BlockNumber::from_epoch(1));
        assert_eq!(at.current_key.key_id, decrypter.next.as_ref().unwrap().key.info.key_id);
        assert_eq!(at.current_key_activation_block_num, BlockNumber::from_epoch(1));
        assert!(at.next_key.is_none());
    }

    #[test]
    fn scheduled_rotation_rejects_non_boundary_activation() {
        let result = decrypter(7).with_scheduled_rotation(key(8), BlockNumber::from((1 << 16) + 1));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn decrypts_with_current_key_id() {
        let decrypter = decrypter(7);
        let schedule = decrypter.encryption_key_schedule(BlockNumber::from(42)).await.unwrap();
        let associated_data = b"scheme|key_id|chain|tx";
        let ciphertext = seal(&decrypter.sealing_key(), b"transaction inputs", associated_data);

        let plaintext = decrypter
            .decrypt_transaction_inputs(
                &schedule.current_key.key_id,
                BlockNumber::from(42),
                &ciphertext,
                associated_data,
            )
            .await
            .unwrap();
        assert_eq!(plaintext, b"transaction inputs");
    }

    #[tokio::test]
    async fn enforces_premature_grace_expired_and_unknown_key_ids() {
        let decrypter = scheduled_decrypter();
        let before = decrypter.encryption_key_schedule(BlockNumber::GENESIS).await.unwrap();
        let current_id = before.current_key.key_id;
        let next_id = before.next_key.unwrap().key.key_id;
        let associated_data = b"associated";
        let current_ciphertext = seal(&decrypter.sealing_key(), b"current", associated_data);
        let next_ciphertext =
            seal(&decrypter.next_sealing_key().unwrap(), b"next", associated_data);

        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(
                    &next_id,
                    BlockNumber::from((1 << 16) - 1),
                    &next_ciphertext,
                    associated_data,
                )
                .await,
            Err(TransactionInputDecryptionError::PrematureKey { .. })
        ));

        let activation = BlockNumber::from_epoch(1);
        assert_eq!(
            decrypter
                .decrypt_transaction_inputs(
                    &next_id,
                    activation,
                    &next_ciphertext,
                    associated_data,
                )
                .await
                .unwrap(),
            b"next"
        );
        assert_eq!(
            decrypter
                .decrypt_transaction_inputs(
                    &current_id,
                    activation,
                    &current_ciphertext,
                    associated_data,
                )
                .await
                .unwrap(),
            b"current"
        );

        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(
                    &current_id,
                    BlockNumber::from_epoch(2),
                    &current_ciphertext,
                    associated_data,
                )
                .await,
            Err(TransactionInputDecryptionError::ExpiredKey { .. })
        ));
        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(
                    b"not-a-provider-key",
                    activation,
                    &next_ciphertext,
                    associated_data,
                )
                .await,
            Err(TransactionInputDecryptionError::UnknownKey { .. })
        ));
    }

    #[tokio::test]
    async fn two_manual_rotations_enforce_activation_and_grace() {
        let decrypter = repeated_rotation_decrypter();
        let previous_id = decrypter.previous.as_ref().unwrap().key.info.key_id.clone();
        let current_id = decrypter.current.key.info.key_id.clone();
        let next_id = decrypter.next.as_ref().unwrap().key.info.key_id.clone();
        let associated_data = b"associated";
        let (previous_ciphertext, current_ciphertext, next_ciphertext) =
            repeated_rotation_ciphertexts(&decrypter, associated_data);

        let before_first = BlockNumber::from((1 << 16) - 1);
        let before_schedule = decrypter.encryption_key_schedule(before_first).await.unwrap();
        assert_eq!(before_schedule.current_key.key_id, previous_id);
        assert_eq!(before_schedule.next_key.unwrap().key.key_id, current_id);
        assert_premature(
            &decrypter,
            &current_id,
            before_first,
            &current_ciphertext,
            associated_data,
        )
        .await;
        assert_premature(&decrypter, &next_id, before_first, &next_ciphertext, associated_data)
            .await;

        let first_activation = BlockNumber::from_epoch(1);
        let first_schedule = decrypter.encryption_key_schedule(first_activation).await.unwrap();
        assert_eq!(first_schedule.current_key.key_id, current_id);
        assert_eq!(first_schedule.next_key.unwrap().key.key_id, next_id);
        assert_decrypts(
            &decrypter,
            &previous_id,
            first_activation,
            &previous_ciphertext,
            associated_data,
            b"previous",
        )
        .await;

        let second_activation = BlockNumber::from_epoch(2);
        let second_schedule = decrypter.encryption_key_schedule(second_activation).await.unwrap();
        assert_eq!(second_schedule.current_key.key_id, next_id);
        assert!(second_schedule.next_key.is_none());
        assert_decrypts(
            &decrypter,
            &current_id,
            second_activation,
            &current_ciphertext,
            associated_data,
            b"current",
        )
        .await;
        assert_expired(
            &decrypter,
            &previous_id,
            second_activation,
            &previous_ciphertext,
            associated_data,
        )
        .await;
        assert_decrypts(
            &decrypter,
            &next_id,
            second_activation,
            &next_ciphertext,
            associated_data,
            b"next",
        )
        .await;

        assert_expired(
            &decrypter,
            &current_id,
            BlockNumber::from_epoch(3),
            &current_ciphertext,
            associated_data,
        )
        .await;
        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(
                    b"unknown",
                    second_activation,
                    &next_ciphertext,
                    associated_data,
                )
                .await,
            Err(TransactionInputDecryptionError::UnknownKey { .. })
        ));
    }

    #[tokio::test]
    async fn previous_key_can_be_dropped_after_grace_expiry() {
        let decrypter = LocalX25519TransactionInputDecrypter::from_schedule(
            None,
            (key(8), BlockNumber::from_epoch(1)),
            None,
        )
        .unwrap();

        assert!(decrypter.encryption_key_schedule(BlockNumber::from_epoch(1)).await.is_err());

        let schedule = decrypter.encryption_key_schedule(BlockNumber::from_epoch(2)).await.unwrap();
        assert_eq!(schedule.current_key.key_id, decrypter.current.key.info.key_id);
    }

    #[tokio::test]
    async fn rejects_wrong_associated_data_and_malformed_ciphertext() {
        let decrypter = decrypter(7);
        let key_id = decrypter.current.key.info.key_id.clone();
        let ciphertext = seal(&decrypter.sealing_key(), b"inputs", b"correct");

        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(&key_id, BlockNumber::GENESIS, &ciphertext, b"wrong",)
                .await,
            Err(TransactionInputDecryptionError::DecryptionFailed(_))
        ));
        assert!(matches!(
            decrypter
                .decrypt_transaction_inputs(
                    &key_id,
                    BlockNumber::GENESIS,
                    b"not a sealed message",
                    b"correct",
                )
                .await,
            Err(TransactionInputDecryptionError::InvalidCiphertext(_))
        ));
    }

    struct OperationOnlyProvider {
        schedule: TransactionEncryptionKeySchedule,
    }

    #[tonic::async_trait]
    impl TransactionInputDecrypter for OperationOnlyProvider {
        async fn encryption_key_schedule(
            &self,
            _chain_tip: BlockNumber,
        ) -> anyhow::Result<TransactionEncryptionKeySchedule> {
            Ok(self.schedule.clone())
        }

        async fn decrypt_transaction_inputs(
            &self,
            _key_id: &[u8],
            _chain_tip: BlockNumber,
            ciphertext: &[u8],
            _associated_data: &[u8],
        ) -> Result<Vec<u8>, TransactionInputDecryptionError> {
            Ok(ciphertext.to_vec())
        }
    }

    #[tokio::test]
    async fn provider_contract_does_not_require_secret_export() {
        let schedule = decrypter(7).encryption_key_schedule(BlockNumber::GENESIS).await.unwrap();
        let provider = OperationOnlyProvider { schedule };

        assert_eq!(
            provider
                .decrypt_transaction_inputs(
                    b"opaque",
                    BlockNumber::GENESIS,
                    b"plaintext from hardware",
                    b"associated",
                )
                .await
                .unwrap(),
            b"plaintext from hardware"
        );
    }
}
