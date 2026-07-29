use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use golden_ehtdh1::wire::{from_wire_bytes, to_wire_bytes};
use golden_ehtdh1::{Ciphertext, Combiner, DecryptionShare, SealingKey};
use golden_halo2curves::golden_group::Secp256k1GoldenGroup;
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::Serializable;
use rand_core_06::{CryptoRng, RngCore};
use zeroize::Zeroizing;

use crate::{GoldenOperatorKey, StorageKeyEpoch};

/// Schema version for the first private record format.
pub const PRIVATE_RECORD_SCHEMA_V1: u32 = 1;

const CONTEXT_DOMAIN_V1: &[u8] = b"miden-private-record-context-v1";
const CONTENT_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const XCHACHA20_POLY1305_ID: u16 = 1;

type StorageGroup = Secp256k1GoldenGroup;

/// Public record cipher metadata fixed by schema version 1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateRecordCipher {
    id: u16,
    name: &'static str,
    key_bytes: usize,
    nonce_bytes: usize,
    tag_bytes: usize,
}

impl PrivateRecordCipher {
    /// Returns the stable cipher identifier stored with the record.
    pub const fn id(self) -> u16 {
        self.id
    }

    /// Returns the cipher name used for diagnostics.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the content key size.
    pub const fn key_bytes(self) -> usize {
        self.key_bytes
    }

    /// Returns the nonce size.
    pub const fn nonce_bytes(self) -> usize {
        self.nonce_bytes
    }

    /// Returns the authentication tag size.
    pub const fn tag_bytes(self) -> usize {
        self.tag_bytes
    }
}

/// `XChaCha20Poly1305` metadata fixed by private record schema version 1.
pub const PRIVATE_RECORD_CIPHER_V1: PrivateRecordCipher = PrivateRecordCipher {
    id: XCHACHA20_POLY1305_ID,
    name: "XChaCha20Poly1305",
    key_bytes: CONTENT_KEY_BYTES,
    nonce_bytes: NONCE_BYTES,
    tag_bytes: TAG_BYTES,
};

/// Identifier for the chain that owns a private record.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrivateRecordChainId([u8; 32]);

impl PrivateRecordChainId {
    /// Creates a chain identifier from its canonical bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical chain identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Stable identifier for one stored private record.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrivateRecordId([u8; 32]);

impl PrivateRecordId {
    /// Creates a record identifier from its canonical bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Uses the transaction id for its single stored transaction input record.
    pub fn for_transaction_inputs(transaction_id: TransactionId) -> Self {
        let bytes = transaction_id.to_bytes();
        Self(bytes.try_into().expect("a Miden transaction id is always 32 bytes"))
    }

    /// Returns the canonical record identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Values bound to one private record and its Golden decryption shares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateRecordContext {
    chain_id: PrivateRecordChainId,
    key_epoch: StorageKeyEpoch,
    record_id: PrivateRecordId,
    transaction_id: TransactionId,
}

impl PrivateRecordContext {
    /// Creates a schema version 1 context.
    pub const fn new(
        chain_id: PrivateRecordChainId,
        key_epoch: StorageKeyEpoch,
        record_id: PrivateRecordId,
        transaction_id: TransactionId,
    ) -> Self {
        Self {
            chain_id,
            key_epoch,
            record_id,
            transaction_id,
        }
    }

    /// Returns the chain identifier.
    pub const fn chain_id(&self) -> PrivateRecordChainId {
        self.chain_id
    }

    /// Returns the storage key epoch.
    pub const fn key_epoch(&self) -> StorageKeyEpoch {
        self.key_epoch
    }

    /// Returns the record identifier.
    pub const fn record_id(&self) -> PrivateRecordId {
        self.record_id
    }

    /// Returns the transaction identifier.
    pub const fn transaction_id(&self) -> TransactionId {
        self.transaction_id
    }

    /// Returns the record schema version.
    pub const fn schema_version(&self) -> u32 {
        PRIVATE_RECORD_SCHEMA_V1
    }

    /// Returns the canonical context used by the record cipher and Golden.
    pub fn to_bytes(self) -> Vec<u8> {
        let transaction_id = self.transaction_id.to_bytes();
        let mut context = Vec::with_capacity(CONTEXT_DOMAIN_V1.len() + 4 * 32 + size_of::<u32>());
        context.extend_from_slice(CONTEXT_DOMAIN_V1);
        context.extend_from_slice(self.chain_id.as_bytes());
        context.extend_from_slice(self.key_epoch.as_bytes());
        context.extend_from_slice(self.record_id.as_bytes());
        context.extend_from_slice(&transaction_id);
        context.extend_from_slice(&PRIVATE_RECORD_SCHEMA_V1.to_be_bytes());
        context
    }
}

/// Exact record values that an operator must approve before issuing a share.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateRecordShareRequest {
    record_id: PrivateRecordId,
    key_epoch: StorageKeyEpoch,
    context: Vec<u8>,
}

impl PrivateRecordShareRequest {
    /// Creates a request for one record, epoch, and canonical context.
    pub fn new(record_id: PrivateRecordId, key_epoch: StorageKeyEpoch, context: Vec<u8>) -> Self {
        Self { record_id, key_epoch, context }
    }

    /// Creates a request from one checked stored record.
    pub fn for_record(record: &StoredPrivateRecord) -> Self {
        let context = record.context();
        Self::new(context.record_id(), context.key_epoch(), context.to_bytes())
    }

    /// Returns the requested record identifier.
    pub const fn record_id(&self) -> PrivateRecordId {
        self.record_id
    }

    /// Returns the requested storage key epoch.
    pub const fn key_epoch(&self) -> StorageKeyEpoch {
        self.key_epoch
    }

    /// Returns the exact requested decryption context.
    pub fn context(&self) -> &[u8] {
        &self.context
    }
}

/// Decides whether an operator may issue a decryption share.
pub trait PrivateRecordSharePolicy {
    /// Returns `true` when the request may receive a share.
    fn allows(&self, request: &PrivateRecordShareRequest, record: &StoredPrivateRecord) -> bool;
}

impl<F> PrivateRecordSharePolicy for F
where
    F: Fn(&PrivateRecordShareRequest, &StoredPrivateRecord) -> bool,
{
    fn allows(&self, request: &PrivateRecordShareRequest, record: &StoredPrivateRecord) -> bool {
        self(request, record)
    }
}

/// Public Golden key used to seal private records for one epoch.
#[derive(Clone, Debug)]
pub struct PrivateRecordSealer {
    key_epoch: StorageKeyEpoch,
    setup_context_id: [u8; 32],
    sealing_key: SealingKey<StorageGroup>,
}

impl PrivateRecordSealer {
    /// Creates a sealer from a validated operator key without copying its secret share.
    pub fn from_operator_key(operator_key: &GoldenOperatorKey) -> Self {
        Self {
            key_epoch: operator_key.key_epoch(),
            setup_context_id: operator_key.setup_context_id(),
            sealing_key: operator_key.sealing_key().clone(),
        }
    }

    /// Returns the storage-key epoch used to seal records.
    pub fn key_epoch(&self) -> StorageKeyEpoch {
        self.key_epoch
    }

    /// Encrypts a record and wraps only its fresh content key with Golden.
    pub fn seal<R: RngCore + CryptoRng>(
        &self,
        rng: &mut R,
        context: PrivateRecordContext,
        plaintext: &[u8],
    ) -> Result<StoredPrivateRecord, PrivateRecordError> {
        if context.key_epoch() != self.key_epoch {
            return Err(PrivateRecordError::KeyEpochMismatch);
        }

        let context_bytes = context.to_bytes();
        let mut content_key = Zeroizing::new([0u8; CONTENT_KEY_BYTES]);
        let mut nonce = [0u8; NONCE_BYTES];
        rng.fill_bytes(content_key.as_mut());
        rng.fill_bytes(&mut nonce);

        let cipher = XChaCha20Poly1305::new_from_slice(content_key.as_ref())
            .map_err(|_| PrivateRecordError::RecordEncryption)?;
        let encrypted_record = cipher
            .encrypt(&XNonce::from(nonce), Payload { msg: plaintext, aad: &context_bytes })
            .map_err(|_| PrivateRecordError::RecordEncryption)?;
        let wrapped_content_key = self
            .sealing_key
            .seal_bytes_with_associated_data(rng, content_key.as_ref(), &context_bytes)
            .map_err(PrivateRecordError::ContentKeyEncryption)?;
        if wrapped_content_key.encrypted_payload.len() != CONTENT_KEY_BYTES {
            return Err(PrivateRecordError::InvalidWrappedContentKey);
        }

        Ok(StoredPrivateRecord {
            context,
            setup_context_id: self.setup_context_id,
            block_num: None,
            cipher: PRIVATE_RECORD_CIPHER_V1,
            nonce,
            encrypted_record,
            wrapped_content_key: to_wire_bytes(&wrapped_content_key),
        })
    }
}

/// Public Golden setup used to verify shares and open private records.
#[derive(Clone, Debug)]
pub struct PrivateRecordCombiner {
    key_epoch: StorageKeyEpoch,
    setup_context_id: [u8; 32],
    combiner: Combiner<StorageGroup>,
}

impl PrivateRecordCombiner {
    /// Creates a combiner from one validated operator key without copying its secret share.
    pub fn from_operator_key(operator_key: &GoldenOperatorKey) -> Result<Self, PrivateRecordError> {
        let combiner = Combiner::new(
            operator_key.public_key_set().clone(),
            operator_key.setup_context().clone(),
        )
        .map_err(PrivateRecordError::InvalidCombinerSetup)?;
        Ok(Self {
            key_epoch: operator_key.key_epoch(),
            setup_context_id: operator_key.setup_context_id(),
            combiner,
        })
    }

    /// Verifies exact threshold share bytes and opens the private record.
    pub fn open(
        &self,
        request: &PrivateRecordShareRequest,
        record: &StoredPrivateRecord,
        share_bytes: &[Vec<u8>],
    ) -> Result<Zeroizing<Vec<u8>>, PrivateRecordError> {
        record.validate_share_request(request, self.key_epoch, self.setup_context_id)?;
        let ciphertext = record.decode_wrapped_content_key()?;
        let shares = share_bytes
            .iter()
            .map(|bytes| {
                from_wire_bytes::<DecryptionShare<StorageGroup>>(bytes)
                    .map_err(PrivateRecordError::InvalidDecryptionShare)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let context = request.context();
        let content_key = Zeroizing::new(
            self.combiner
                .combine_exact_with_associated_data(&ciphertext, context, context, &shares)
                .map_err(PrivateRecordError::ShareCombination)?,
        );
        if content_key.len() != CONTENT_KEY_BYTES {
            return Err(PrivateRecordError::InvalidWrappedContentKey);
        }

        let cipher = XChaCha20Poly1305::new_from_slice(content_key.as_ref())
            .map_err(|_| PrivateRecordError::RecordDecryption)?;
        cipher
            .decrypt(
                &XNonce::from(*record.nonce()),
                Payload {
                    msg: record.encrypted_record(),
                    aad: context,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| PrivateRecordError::RecordDecryption)
    }
}

#[cfg(test)]
pub(crate) fn test_private_record_sealer(
    key_epoch: StorageKeyEpoch,
    setup_context_id: [u8; 32],
) -> PrivateRecordSealer {
    use golden_core::{GoldenGroup, GoldenScalar};
    use golden_halo2curves::golden_group::Secp256k1Scalar;

    let scalar = Secp256k1Scalar::from_u64(11).expect("test scalar is valid");
    PrivateRecordSealer {
        key_epoch,
        setup_context_id,
        sealing_key: SealingKey::new(StorageGroup::mul_generator(&scalar))
            .expect("test sealing key is valid"),
    }
}

/// Database fields for one versioned private record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateRecordStorageFields {
    /// Values bound into both encryption layers.
    pub context: PrivateRecordContext,
    /// Record format version stored as a separate lookup field.
    pub schema_version: u32,
    /// Golden setup context identifier.
    pub setup_context_id: [u8; 32],
    /// Block number after transaction inclusion.
    pub block_num: Option<BlockNumber>,
    /// Stable public record cipher identifier.
    pub cipher_id: u16,
    /// Public record cipher nonce.
    pub nonce: Vec<u8>,
    /// Authenticated record ciphertext.
    pub encrypted_record: Vec<u8>,
    /// Canonical Golden ciphertext for the content key.
    pub wrapped_content_key: Vec<u8>,
}

/// Versioned encrypted private record stored by the validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPrivateRecord {
    context: PrivateRecordContext,
    setup_context_id: [u8; 32],
    block_num: Option<BlockNumber>,
    cipher: PrivateRecordCipher,
    nonce: [u8; NONCE_BYTES],
    encrypted_record: Vec<u8>,
    wrapped_content_key: Vec<u8>,
}

impl StoredPrivateRecord {
    /// Rebuilds a record after validating its stored fields.
    pub fn from_storage_fields(
        fields: PrivateRecordStorageFields,
    ) -> Result<Self, PrivateRecordError> {
        if fields.schema_version != PRIVATE_RECORD_SCHEMA_V1 {
            return Err(PrivateRecordError::UnsupportedSchema(fields.schema_version));
        }
        if fields.cipher_id != PRIVATE_RECORD_CIPHER_V1.id() {
            return Err(PrivateRecordError::UnsupportedCipher(fields.cipher_id));
        }
        let nonce = fields.nonce.try_into().map_err(|nonce: Vec<u8>| {
            PrivateRecordError::InvalidNonceLength { actual: nonce.len() }
        })?;
        if fields.encrypted_record.len() < TAG_BYTES {
            return Err(PrivateRecordError::InvalidRecordCiphertext);
        }

        let record = Self {
            context: fields.context,
            setup_context_id: fields.setup_context_id,
            block_num: fields.block_num,
            cipher: PRIVATE_RECORD_CIPHER_V1,
            nonce,
            encrypted_record: fields.encrypted_record,
            wrapped_content_key: fields.wrapped_content_key,
        };
        record.verify_wrapped_content_key()?;
        Ok(record)
    }

    /// Splits a checked record into the fields stored by the database.
    pub fn into_storage_fields(self) -> PrivateRecordStorageFields {
        PrivateRecordStorageFields {
            context: self.context,
            schema_version: self.context.schema_version(),
            setup_context_id: self.setup_context_id,
            block_num: self.block_num,
            cipher_id: self.cipher.id(),
            nonce: self.nonce.to_vec(),
            encrypted_record: self.encrypted_record,
            wrapped_content_key: self.wrapped_content_key,
        }
    }

    /// Returns the values bound into both encryption layers.
    pub const fn context(&self) -> PrivateRecordContext {
        self.context
    }

    /// Returns the Golden setup context identifier.
    pub const fn setup_context_id(&self) -> &[u8; 32] {
        &self.setup_context_id
    }

    /// Returns the block number after the transaction is included.
    pub const fn block_num(&self) -> Option<BlockNumber> {
        self.block_num
    }

    /// Sets lookup data without changing the decryption context.
    pub fn set_block_num(&mut self, block_num: BlockNumber) {
        self.block_num = Some(block_num);
    }

    /// Returns the public cipher metadata.
    pub const fn cipher(&self) -> PrivateRecordCipher {
        self.cipher
    }

    /// Returns the public record cipher nonce.
    pub const fn nonce(&self) -> &[u8; NONCE_BYTES] {
        &self.nonce
    }

    /// Returns the authenticated record ciphertext.
    pub fn encrypted_record(&self) -> &[u8] {
        &self.encrypted_record
    }

    /// Returns the canonical Golden ciphertext for the content key.
    pub fn wrapped_content_key(&self) -> &[u8] {
        &self.wrapped_content_key
    }

    /// Checks the canonical Golden content key ciphertext and its context.
    pub fn verify_wrapped_content_key(&self) -> Result<(), PrivateRecordError> {
        self.decode_wrapped_content_key().map(drop)
    }

    /// Decodes and checks the canonical Golden content key ciphertext.
    pub(crate) fn decode_wrapped_content_key(
        &self,
    ) -> Result<Ciphertext<StorageGroup>, PrivateRecordError> {
        let ciphertext: Ciphertext<StorageGroup> = from_wire_bytes(&self.wrapped_content_key)
            .map_err(PrivateRecordError::InvalidGoldenEncoding)?;
        if ciphertext.encrypted_payload.len() != CONTENT_KEY_BYTES {
            return Err(PrivateRecordError::InvalidWrappedContentKey);
        }
        ciphertext
            .verify_with_associated_data(&self.context.to_bytes())
            .map_err(PrivateRecordError::InvalidGoldenEncoding)?;
        Ok(ciphertext)
    }

    /// Checks that a share request, stored record, and active Golden key name the same values.
    pub(crate) fn validate_share_request(
        &self,
        request: &PrivateRecordShareRequest,
        key_epoch: StorageKeyEpoch,
        setup_context_id: [u8; 32],
    ) -> Result<(), PrivateRecordError> {
        if request.record_id() != self.context.record_id() {
            return Err(PrivateRecordError::RecordIdMismatch);
        }
        if request.key_epoch() != self.context.key_epoch() || request.key_epoch() != key_epoch {
            return Err(PrivateRecordError::KeyEpochMismatch);
        }
        if self.setup_context_id != setup_context_id {
            return Err(PrivateRecordError::SetupContextMismatch);
        }
        if request.context() != self.context.to_bytes() {
            return Err(PrivateRecordError::DecryptionContextMismatch);
        }
        Ok(())
    }
}

/// Error raised while sealing or reading a private record.
#[derive(Debug, thiserror::Error)]
pub enum PrivateRecordError {
    /// The record, request, and active storage key name different epochs.
    #[error("private record key epoch does not match")]
    KeyEpochMismatch,
    /// The share request names a different record.
    #[error("private record share request names a different record")]
    RecordIdMismatch,
    /// The operator and record name different Golden setups.
    #[error("private record Golden setup does not match the operator")]
    SetupContextMismatch,
    /// The request does not carry the record's exact canonical context.
    #[error("private record decryption context does not match the record")]
    DecryptionContextMismatch,
    /// Policy denied the share request.
    #[error("private record share request was denied")]
    ShareDenied,
    /// The authenticated record cipher failed.
    #[error("failed to encrypt private record")]
    RecordEncryption,
    /// Golden failed to seal the content key.
    #[error("failed to encrypt private record content key")]
    ContentKeyEncryption(#[source] golden_ehtdh1::Error),
    /// The canonical Golden value could not be decoded or verified.
    #[error("invalid Golden content key ciphertext")]
    InvalidGoldenEncoding(#[source] golden_ehtdh1::Error),
    /// The public Golden setup cannot create a combiner.
    #[error("invalid Golden combiner setup")]
    InvalidCombinerSetup(#[source] golden_ehtdh1::Error),
    /// A canonical decryption share could not be decoded.
    #[error("invalid Golden decryption share")]
    InvalidDecryptionShare(#[source] golden_ehtdh1::Error),
    /// Golden could not issue a decryption share.
    #[error("failed to issue Golden decryption share")]
    ShareGeneration(#[source] golden_ehtdh1::Error),
    /// Golden rejected the provided share set.
    #[error("failed to combine Golden decryption shares")]
    ShareCombination(#[source] golden_ehtdh1::CombineError),
    /// The Golden ciphertext does not wrap one schema version 1 content key.
    #[error("Golden ciphertext has the wrong content key size")]
    InvalidWrappedContentKey,
    /// The stored schema version is not supported.
    #[error("unsupported private record schema version {0}")]
    UnsupportedSchema(u32),
    /// The stored cipher identifier is not supported.
    #[error("unsupported private record cipher {0}")]
    UnsupportedCipher(u16),
    /// The stored nonce has the wrong size.
    #[error("private record nonce has {actual} bytes, expected {NONCE_BYTES}")]
    InvalidNonceLength { actual: usize },
    /// The stored record ciphertext cannot contain an authentication tag.
    #[error("private record ciphertext is shorter than its authentication tag")]
    InvalidRecordCiphertext,
    /// The authenticated record cipher rejected the content key, context, or ciphertext.
    #[error("failed to decrypt private record")]
    RecordDecryption,
}

#[cfg(test)]
mod tests {
    use golden_ehtdh1::DecryptionShare;
    use miden_protocol::Word;
    use miden_protocol::account::auth::AuthScheme;
    use miden_protocol::transaction::TransactionInputs;
    use miden_protocol::utils::serde::Deserializable;
    use miden_testing::{Auth, MockChainBuilder};
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;

    use super::*;
    use crate::storage_key::tests::operator_keys;

    const CHAIN_ID: PrivateRecordChainId = PrivateRecordChainId::new([1; 32]);
    const EPOCH: StorageKeyEpoch = StorageKeyEpoch::new([2; 32]);
    const RECORD_ID: PrivateRecordId = PrivateRecordId::new([3; 32]);

    fn transaction_id() -> TransactionId {
        TransactionId::from_raw(Word::from([4u32, 5, 6, 7]))
    }

    fn context() -> PrivateRecordContext {
        PrivateRecordContext::new(CHAIN_ID, EPOCH, RECORD_ID, transaction_id())
    }

    fn sealer() -> PrivateRecordSealer {
        test_private_record_sealer(EPOCH, [8; 32])
    }

    fn allow_all(_request: &PrivateRecordShareRequest, _record: &StoredPrivateRecord) -> bool {
        true
    }

    fn deny_all(_request: &PrivateRecordShareRequest, _record: &StoredPrivateRecord) -> bool {
        false
    }

    fn threshold_record(
        operator_key: &GoldenOperatorKey,
        record_id: PrivateRecordId,
        transaction_id: TransactionId,
        seed: u8,
        plaintext: &[u8],
    ) -> StoredPrivateRecord {
        let context = PrivateRecordContext::new(
            CHAIN_ID,
            operator_key.key_epoch(),
            record_id,
            transaction_id,
        );
        let mut rng = ChaCha20Rng::from_seed([seed; 32]);
        PrivateRecordSealer::from_operator_key(operator_key)
            .seal(&mut rng, context, plaintext)
            .unwrap()
    }

    fn issue_share(
        operator_key: &GoldenOperatorKey,
        request: &PrivateRecordShareRequest,
        record: &StoredPrivateRecord,
        seed: u8,
    ) -> Vec<u8> {
        let mut rng = ChaCha20Rng::from_seed([seed; 32]);
        operator_key
            .issue_private_record_share(&mut rng, request, record, &allow_all)
            .unwrap()
    }

    fn transaction_inputs() -> TransactionInputs {
        let mut builder = MockChainBuilder::new();
        let account = builder
            .add_existing_wallet(Auth::BasicAuth {
                auth_scheme: AuthScheme::Falcon512Poseidon2,
            })
            .unwrap();
        builder.build().unwrap().get_transaction_inputs(&account, &[], &[]).unwrap()
    }

    #[test]
    fn context_has_one_fixed_canonical_encoding() {
        let bytes = context().to_bytes();
        let transaction_id = transaction_id().to_bytes();

        assert_eq!(&bytes[..CONTEXT_DOMAIN_V1.len()], CONTEXT_DOMAIN_V1);
        assert_eq!(
            &bytes[CONTEXT_DOMAIN_V1.len()..CONTEXT_DOMAIN_V1.len() + 32],
            CHAIN_ID.as_bytes(),
        );
        assert_eq!(
            &bytes[CONTEXT_DOMAIN_V1.len() + 32..CONTEXT_DOMAIN_V1.len() + 64],
            EPOCH.as_bytes(),
        );
        assert_eq!(
            &bytes[CONTEXT_DOMAIN_V1.len() + 64..CONTEXT_DOMAIN_V1.len() + 96],
            RECORD_ID.as_bytes(),
        );
        assert_eq!(
            &bytes[CONTEXT_DOMAIN_V1.len() + 96..CONTEXT_DOMAIN_V1.len() + 128],
            transaction_id,
        );
        assert_eq!(
            &bytes[CONTEXT_DOMAIN_V1.len() + 128..],
            &PRIVATE_RECORD_SCHEMA_V1.to_be_bytes(),
        );
    }

    #[test]
    fn transaction_input_record_id_is_stable() {
        let transaction_id = transaction_id();

        assert_eq!(
            PrivateRecordId::for_transaction_inputs(transaction_id).as_bytes(),
            transaction_id.to_bytes().as_slice(),
        );
    }

    #[test]
    fn record_cipher_metadata_is_fixed() {
        assert_eq!(PRIVATE_RECORD_CIPHER_V1.id(), 1);
        assert_eq!(PRIVATE_RECORD_CIPHER_V1.name(), "XChaCha20Poly1305");
        assert_eq!(PRIVATE_RECORD_CIPHER_V1.key_bytes(), 32);
        assert_eq!(PRIVATE_RECORD_CIPHER_V1.nonce_bytes(), 24);
        assert_eq!(PRIVATE_RECORD_CIPHER_V1.tag_bytes(), 16);
    }

    #[test]
    fn seal_uses_a_fresh_key_and_nonce_and_wraps_only_the_key() {
        let plaintext = b"private transaction inputs";
        let mut expected_rng = ChaCha20Rng::from_seed([9; 32]);
        let mut expected_content_key = Zeroizing::new([0u8; CONTENT_KEY_BYTES]);
        let mut expected_nonce = [0u8; NONCE_BYTES];
        expected_rng.fill_bytes(expected_content_key.as_mut());
        expected_rng.fill_bytes(&mut expected_nonce);

        let mut rng = ChaCha20Rng::from_seed([9; 32]);
        let first = sealer().seal(&mut rng, context(), plaintext).unwrap();
        let second = sealer().seal(&mut rng, context(), plaintext).unwrap();

        assert_eq!(first.nonce(), &expected_nonce);
        assert_ne!(first.nonce(), second.nonce());
        assert_ne!(first.encrypted_record(), second.encrypted_record());
        assert_ne!(first.wrapped_content_key(), second.wrapped_content_key());
        assert_eq!(first.encrypted_record().len(), plaintext.len() + TAG_BYTES);
        assert_eq!(first.context(), context());
        assert_eq!(first.setup_context_id(), &[8; 32]);
        assert_eq!(first.block_num(), None);
        assert_eq!(first.cipher(), PRIVATE_RECORD_CIPHER_V1);

        let cipher = XChaCha20Poly1305::new_from_slice(expected_content_key.as_ref()).unwrap();
        let opened = cipher
            .decrypt(
                &XNonce::from(*first.nonce()),
                Payload {
                    msg: first.encrypted_record(),
                    aad: &context().to_bytes(),
                },
            )
            .unwrap();
        assert_eq!(opened, plaintext);
        assert!(
            cipher
                .decrypt(
                    &XNonce::from(*first.nonce()),
                    Payload {
                        msg: first.encrypted_record(),
                        aad: b"wrong context",
                    },
                )
                .is_err(),
        );

        let wrapped = first.decode_wrapped_content_key().unwrap();
        assert_eq!(wrapped.encrypted_payload.len(), CONTENT_KEY_BYTES);
        assert_eq!(wrapped.associated_data(), context().to_bytes());
    }

    #[test]
    fn block_number_does_not_change_the_context() {
        let mut rng = ChaCha20Rng::from_seed([10; 32]);
        let mut record = sealer().seal(&mut rng, context(), b"record").unwrap();
        let context_bytes = record.context().to_bytes();

        record.set_block_num(BlockNumber::from(12));

        assert_eq!(record.block_num(), Some(BlockNumber::from(12)));
        assert_eq!(record.context().to_bytes(), context_bytes);
        record.verify_wrapped_content_key().unwrap();
    }

    #[test]
    fn storage_fields_round_trip() {
        let mut rng = ChaCha20Rng::from_seed([12; 32]);
        let mut expected = sealer().seal(&mut rng, context(), b"record").unwrap();
        expected.set_block_num(BlockNumber::from(13));

        let actual =
            StoredPrivateRecord::from_storage_fields(expected.clone().into_storage_fields())
                .unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn storage_fields_reject_invalid_metadata() {
        let mut rng = ChaCha20Rng::from_seed([13; 32]);
        let record = sealer().seal(&mut rng, context(), b"record").unwrap();

        let mut wrong_schema = record.clone().into_storage_fields();
        wrong_schema.schema_version = 2;
        assert!(matches!(
            StoredPrivateRecord::from_storage_fields(wrong_schema),
            Err(PrivateRecordError::UnsupportedSchema(2)),
        ));

        let mut wrong_cipher = record.clone().into_storage_fields();
        wrong_cipher.cipher_id = 2;
        assert!(matches!(
            StoredPrivateRecord::from_storage_fields(wrong_cipher),
            Err(PrivateRecordError::UnsupportedCipher(2)),
        ));

        let mut wrong_nonce = record.clone().into_storage_fields();
        wrong_nonce.nonce.pop();
        assert!(matches!(
            StoredPrivateRecord::from_storage_fields(wrong_nonce),
            Err(PrivateRecordError::InvalidNonceLength { actual: 23 }),
        ));

        let mut short_ciphertext = record.into_storage_fields();
        short_ciphertext.encrypted_record.truncate(TAG_BYTES - 1);
        assert!(matches!(
            StoredPrivateRecord::from_storage_fields(short_ciphertext),
            Err(PrivateRecordError::InvalidRecordCiphertext),
        ));
    }

    #[test]
    fn seal_rejects_a_different_epoch() {
        let mut rng = ChaCha20Rng::from_seed([11; 32]);
        let wrong_context = PrivateRecordContext::new(
            CHAIN_ID,
            StorageKeyEpoch::new([99; 32]),
            RECORD_ID,
            transaction_id(),
        );

        assert!(matches!(
            sealer().seal(&mut rng, wrong_context, b"record"),
            Err(PrivateRecordError::KeyEpochMismatch),
        ));
    }

    #[test]
    fn two_of_three_canonical_shares_open_the_record() {
        let operator_keys = operator_keys();
        let inputs = transaction_inputs();
        let plaintext = inputs.to_bytes();
        let record =
            threshold_record(&operator_keys[0], RECORD_ID, transaction_id(), 20, &plaintext);
        let request = PrivateRecordShareRequest::for_record(&record);
        let mut denied_rng = ChaCha20Rng::from_seed([21; 32]);

        assert!(matches!(
            operator_keys[0].issue_private_record_share(
                &mut denied_rng,
                &request,
                &record,
                &deny_all,
            ),
            Err(PrivateRecordError::ShareDenied),
        ));

        let shares = [
            issue_share(&operator_keys[0], &request, &record, 22),
            issue_share(&operator_keys[1], &request, &record, 23),
        ];
        for bytes in &shares {
            let share = from_wire_bytes::<DecryptionShare<StorageGroup>>(bytes).unwrap();
            assert_eq!(to_wire_bytes(&share), *bytes);
        }

        let opened = PrivateRecordCombiner::from_operator_key(&operator_keys[2])
            .unwrap()
            .open(&request, &record, &shares)
            .unwrap();
        assert_eq!(TransactionInputs::read_from_bytes(&opened).unwrap(), inputs);
    }

    #[test]
    fn shares_for_independently_sealed_records_do_not_combine() {
        let operator_keys = operator_keys();
        let plaintext = transaction_inputs().to_bytes();
        let first_record =
            threshold_record(&operator_keys[0], RECORD_ID, transaction_id(), 31, &plaintext);
        let second_record =
            threshold_record(&operator_keys[0], RECORD_ID, transaction_id(), 32, &plaintext);
        assert_ne!(first_record.wrapped_content_key(), second_record.wrapped_content_key());

        let first_request = PrivateRecordShareRequest::for_record(&first_record);
        let second_request = PrivateRecordShareRequest::for_record(&second_record);
        assert_eq!(first_request, second_request);
        let shares = [
            issue_share(&operator_keys[0], &first_request, &first_record, 33),
            issue_share(&operator_keys[1], &second_request, &second_record, 34),
        ];

        let result = PrivateRecordCombiner::from_operator_key(&operator_keys[2]).unwrap().open(
            &first_request,
            &first_record,
            &shares,
        );
        assert!(matches!(result, Err(PrivateRecordError::ShareCombination(_))));
    }

    #[test]
    fn share_requests_bind_record_epoch_context_and_setup() {
        let operator_keys = operator_keys();
        let record =
            threshold_record(&operator_keys[0], RECORD_ID, transaction_id(), 24, b"record");
        let request = PrivateRecordShareRequest::for_record(&record);

        let wrong_record = PrivateRecordShareRequest::new(
            PrivateRecordId::new([99; 32]),
            request.key_epoch(),
            request.context().to_vec(),
        );
        let mut rng = ChaCha20Rng::from_seed([25; 32]);
        assert!(matches!(
            operator_keys[0].issue_private_record_share(
                &mut rng,
                &wrong_record,
                &record,
                &allow_all,
            ),
            Err(PrivateRecordError::RecordIdMismatch),
        ));

        let wrong_epoch = PrivateRecordShareRequest::new(
            request.record_id(),
            StorageKeyEpoch::new([99; 32]),
            request.context().to_vec(),
        );
        assert!(matches!(
            operator_keys[0].issue_private_record_share(
                &mut rng,
                &wrong_epoch,
                &record,
                &allow_all,
            ),
            Err(PrivateRecordError::KeyEpochMismatch),
        ));

        let mut wrong_context_bytes = request.context().to_vec();
        wrong_context_bytes[0] ^= 1;
        let wrong_context = PrivateRecordShareRequest::new(
            request.record_id(),
            request.key_epoch(),
            wrong_context_bytes,
        );
        assert!(matches!(
            operator_keys[0].issue_private_record_share(
                &mut rng,
                &wrong_context,
                &record,
                &allow_all,
            ),
            Err(PrivateRecordError::DecryptionContextMismatch),
        ));

        let mut wrong_setup_fields = record.into_storage_fields();
        wrong_setup_fields.setup_context_id = [99; 32];
        let wrong_setup = StoredPrivateRecord::from_storage_fields(wrong_setup_fields).unwrap();
        assert!(matches!(
            operator_keys[0].issue_private_record_share(
                &mut rng,
                &request,
                &wrong_setup,
                &allow_all,
            ),
            Err(PrivateRecordError::SetupContextMismatch),
        ));
    }

    #[test]
    fn combiner_rejects_bad_shares_and_damaged_ciphertext() {
        let operator_keys = operator_keys();
        let record =
            threshold_record(&operator_keys[0], RECORD_ID, transaction_id(), 26, b"record A");
        let request = PrivateRecordShareRequest::for_record(&record);
        let other_record = threshold_record(
            &operator_keys[0],
            PrivateRecordId::new([7; 32]),
            TransactionId::from_raw(Word::from([8u32, 9, 10, 11])),
            27,
            b"record B",
        );
        let other_request = PrivateRecordShareRequest::for_record(&other_record);
        let first = issue_share(&operator_keys[0], &request, &record, 28);
        let second = issue_share(&operator_keys[1], &request, &record, 29);
        let mixed = issue_share(&operator_keys[1], &other_request, &other_record, 30);
        let combiner = PrivateRecordCombiner::from_operator_key(&operator_keys[2]).unwrap();

        assert!(matches!(
            combiner.open(&request, &record, std::slice::from_ref(&first)),
            Err(PrivateRecordError::ShareCombination(_)),
        ));
        assert!(matches!(
            combiner.open(&request, &record, &[first.clone(), first.clone()]),
            Err(PrivateRecordError::ShareCombination(_)),
        ));
        assert!(matches!(
            combiner.open(&request, &record, &[vec![0], second.clone()]),
            Err(PrivateRecordError::InvalidDecryptionShare(_)),
        ));
        assert!(matches!(
            combiner.open(&request, &record, &[first.clone(), mixed]),
            Err(PrivateRecordError::ShareCombination(_)),
        ));

        let mut damaged_fields = record.into_storage_fields();
        damaged_fields.encrypted_record[0] ^= 1;
        let damaged = StoredPrivateRecord::from_storage_fields(damaged_fields).unwrap();
        assert!(matches!(
            combiner.open(&request, &damaged, &[first, second]),
            Err(PrivateRecordError::RecordDecryption),
        ));
    }
}
