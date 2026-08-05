//! Sealing of transaction inputs against the validator set's shared encryption key, and
//! verification of the key schedule a validator serves.
//!
//! This module is the single definition of the associated-data transcript, so the sealing side
//! (clients and the node's own submitters) and the unsealing side (the validator) cannot drift.
//! A drift would not fail to compile: it would reject every submission at runtime with an opaque
//! AEAD error, so the transcript is pinned by a golden vector in the tests below.
//!
//! It is also the single definition of the attestation transcript. A validator serves a complete
//! key schedule (the key in effect plus an optionally scheduled replacement) under one signature,
//! so a relaying node can neither strip a scheduled rotation nor replay a schedule from an earlier
//! epoch.

use miden_protocol::Word;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as ValidatorPublicKey,
    Signature as ValidatorSignature,
};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::PublicKey as EncryptionPublicKey;
use miden_protocol::crypto::ies::SealingKey;
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::{Deserializable, Serializable};

use crate::generated as proto;

/// Domain tag prefixed to the associated data of sealed transaction inputs.
///
/// Separates this transcript from every other use of the same key material, in particular from the
/// key attestation signed with the validator's signing key.
pub const TX_INPUT_SEAL_DOMAIN: &[u8] = b"MIDEN_TX_INPUT_SEAL_V1";

/// Domain tag prefixed to the validator-signed key schedule payload.
///
/// The `V2` transcript covers a whole schedule and its attestation epoch, where `V1` covered a
/// single key. The tag distinguishes the two so a `V1` signature can never be replayed as a `V2`
/// one.
pub const ATTESTATION_DOMAIN: &[u8] = b"MIDEN_TX_ENCRYPTION_KEY_SCHEDULE_ATTESTATION_V2";

/// Upper bound on the length of an encryption key identifier.
///
/// Key identifiers are 4 bytes today (the leading bytes of the public key commitment). The bound
/// exists so that a hostile or misconfigured key endpoint cannot drive an unbounded allocation, and
/// so that the length cast in the transcript cannot overflow.
pub const MAX_KEY_ID_LEN: usize = 64;

/// Wire identifier of the only IES scheme the node currently supports.
const SCHEME_X25519_XCHACHA20_POLY1305: u32 = 1;

// ENCRYPTION KEY
// ================================================================================================

/// Encryption schemes supported by transaction input submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TransactionEncryptionScheme {
    /// X25519 key agreement with XChaCha20-Poly1305 authenticated encryption.
    X25519XChaCha20Poly1305 = SCHEME_X25519_XCHACHA20_POLY1305,
}

impl TransactionEncryptionScheme {
    /// Returns the integer used for this scheme on the wire and in signed transcripts.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Returns the protobuf enum value for this scheme.
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl TryFrom<i32> for TransactionEncryptionScheme {
    type Error = TransactionEncryptionKeyError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Err(TransactionEncryptionKeyError::UnspecifiedScheme),
            1 => Ok(Self::X25519XChaCha20Poly1305),
            other => Err(TransactionEncryptionKeyError::UnsupportedScheme(other)),
        }
    }
}

/// Public metadata for one provider-owned transaction encryption key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionEncryptionKeyInfo {
    /// Encryption scheme for this key.
    pub scheme: TransactionEncryptionScheme,
    /// Opaque identifier assigned by the key provider.
    pub key_id: Vec<u8>,
    /// Encoded public key.
    pub public_key: Vec<u8>,
}

/// A key scheduled to replace the current one at an epoch boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextTransactionEncryptionKey {
    /// The key that becomes current at `activation_block_num`.
    pub key: TransactionEncryptionKeyInfo,
    /// Epoch boundary at which the key takes effect.
    pub activation_block_num: BlockNumber,
}

/// The complete transaction encryption key schedule served by a validator.
///
/// Both keys are covered by one attestation, so the schedule is verified and validated as a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionEncryptionKeySchedule {
    /// The key currently in effect.
    pub current_key: TransactionEncryptionKeyInfo,
    /// Epoch boundary at which `current_key` took effect.
    pub current_key_activation_block_num: BlockNumber,
    /// Scheduled replacement key, when a rotation has been scheduled.
    pub next_key: Option<NextTransactionEncryptionKey>,
}

impl TransactionEncryptionKeySchedule {
    /// Returns the commitment a validator signs to attest this schedule for one network and epoch.
    ///
    /// The layout is `ATTESTATION_DOMAIN || genesis_commitment || attestation_epoch ||
    /// current_activation || current_key || next_key_present`, followed by `next_activation ||
    /// next_key` when a rotation is scheduled. Each key contributes `scheme || len(key_id) ||
    /// key_id || len(public_key) || public_key`, with the scheme, the epoch, the block numbers and
    /// the length prefixes encoded as 4 bytes little-endian.
    ///
    /// Each binding serves a purpose:
    /// - `genesis_commitment` ties the schedule to one network, so a schedule captured on one
    ///   network cannot replay onto another that shares the same insecure development key.
    /// - `attestation_epoch` ties it to one epoch, so a schedule attested before a rotation cannot
    ///   be replayed after it to keep clients sealing against a retired key.
    /// - `next_key_present` is an explicit one-byte discriminant, so a relaying node can neither
    ///   strip a scheduled rotation nor inject one.
    pub fn attestation_commitment(&self, genesis_commitment: Word, attestation_epoch: u16) -> Word {
        let mut payload = Vec::new();
        payload.extend_from_slice(ATTESTATION_DOMAIN);
        payload.extend_from_slice(&genesis_commitment.to_bytes());
        payload.extend_from_slice(&u32::from(attestation_epoch).to_le_bytes());
        payload.extend_from_slice(&self.current_key_activation_block_num.as_u32().to_le_bytes());
        encode_key(&mut payload, &self.current_key);

        match &self.next_key {
            None => payload.push(0),
            Some(next) => {
                payload.push(1);
                payload.extend_from_slice(&next.activation_block_num.as_u32().to_le_bytes());
                encode_key(&mut payload, &next.key);
            },
        }

        miden_protocol::Hasher::hash(&payload)
    }

    /// Validates the schedule's activation rules against a trusted chain tip.
    ///
    /// Keys activate only at epoch boundaries, the current key must already be active, and a
    /// scheduled key must still be in the future and distinct from the current one.
    pub fn validate_at(
        &self,
        trusted_chain_tip: BlockNumber,
    ) -> Result<(), TransactionEncryptionKeyError> {
        validate_epoch_boundary(self.current_key_activation_block_num, "current key activation")?;
        if self.current_key_activation_block_num > trusted_chain_tip {
            return Err(TransactionEncryptionKeyError::PrematureCurrentKey {
                activation: self.current_key_activation_block_num,
                trusted_chain_tip,
            });
        }

        if let Some(next) = &self.next_key {
            validate_epoch_boundary(next.activation_block_num, "next key activation")?;
            if next.activation_block_num <= trusted_chain_tip {
                return Err(TransactionEncryptionKeyError::NextKeyAlreadyActive {
                    activation: next.activation_block_num,
                    trusted_chain_tip,
                });
            }
            if next.activation_block_num <= self.current_key_activation_block_num {
                return Err(TransactionEncryptionKeyError::InvalidActivationOrder);
            }
            if next.key.key_id == self.current_key.key_id {
                return Err(TransactionEncryptionKeyError::DuplicateKeyId);
            }
        }

        Ok(())
    }
}

/// Trusted chain state used to verify a served transaction encryption key schedule.
///
/// The chain tip must come from a trusted source, because it is what bounds the attestation to the
/// current epoch and what decides whether a scheduled key is still in the future.
#[derive(Debug, Clone, Copy)]
pub struct TrustedTransactionEncryptionState<'a> {
    genesis_commitment: Word,
    chain_tip: BlockNumber,
    validator_signing_keys: &'a [ValidatorPublicKey],
}

impl<'a> TrustedTransactionEncryptionState<'a> {
    /// Creates trusted state from a genesis commitment, a trusted chain tip and the validator
    /// signing keys committed by the chain.
    pub const fn new(
        genesis_commitment: Word,
        chain_tip: BlockNumber,
        validator_signing_keys: &'a [ValidatorPublicKey],
    ) -> Self {
        Self {
            genesis_commitment,
            chain_tip,
            validator_signing_keys,
        }
    }
}

/// A single transaction encryption key whose attestation matched trusted chain state.
#[derive(Debug, Clone)]
pub struct VerifiedTransactionEncryptionKey {
    info: TransactionEncryptionKeyInfo,
    public_key: EncryptionPublicKey,
    genesis_commitment: Word,
}

impl VerifiedTransactionEncryptionKey {
    /// Returns the verified key metadata.
    pub const fn info(&self) -> &TransactionEncryptionKeyInfo {
        &self.info
    }

    /// Returns the decoded encryption public key.
    pub const fn public_key(&self) -> &EncryptionPublicKey {
        &self.public_key
    }

    /// Returns the network genesis commitment covered by the attestation.
    pub const fn genesis_commitment(&self) -> Word {
        self.genesis_commitment
    }
}

/// A key schedule whose attestation matched trusted chain state.
#[derive(Debug, Clone)]
pub struct VerifiedTransactionEncryptionSchedule {
    schedule: TransactionEncryptionKeySchedule,
    current_key: VerifiedTransactionEncryptionKey,
}

impl VerifiedTransactionEncryptionSchedule {
    /// Returns the verified schedule.
    pub const fn schedule(&self) -> &TransactionEncryptionKeySchedule {
        &self.schedule
    }

    /// Returns the key currently in effect.
    pub const fn current_key(&self) -> &VerifiedTransactionEncryptionKey {
        &self.current_key
    }

    /// Consumes the schedule, returning the key currently in effect.
    ///
    /// This is what a sealing client needs: it seals against the current key and refetches the
    /// schedule once the validator reports an unknown key ID.
    pub fn into_current_key(self) -> VerifiedTransactionEncryptionKey {
        self.current_key
    }
}

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

/// Failure to decode or verify a served transaction encryption key schedule.
#[derive(Debug, thiserror::Error)]
pub enum TransactionEncryptionKeyError {
    #[error("encryption key scheme is unspecified")]
    UnspecifiedScheme,
    #[error("unsupported encryption key scheme {0}")]
    UnsupportedScheme(i32),
    #[error("{field} is empty")]
    EmptyKeyId { field: &'static str },
    #[error("{field} is {len} bytes, which exceeds the maximum of {MAX_KEY_ID_LEN}")]
    KeyIdTooLong { field: &'static str, len: usize },
    #[error("invalid {field}")]
    InvalidEncryptionPublicKey {
        field: &'static str,
        #[source]
        source: miden_protocol::utils::serde::DeserializationError,
    },
    #[error("the schedule is missing its current key")]
    MissingCurrentKey,
    #[error("the schedule contains a next-key wrapper without a key")]
    MissingNextKey,
    #[error("the attestation epoch does not fit in the chain epoch type")]
    InvalidAttestationEpoch,
    #[error(
        "schedule attestation epoch {attestation_epoch} does not match trusted epoch {trusted_epoch}"
    )]
    StaleAttestation {
        attestation_epoch: u16,
        trusted_epoch: u16,
    },
    #[error("{name} block {block_num} is not an epoch boundary")]
    NotEpochBoundary {
        name: &'static str,
        block_num: BlockNumber,
    },
    #[error("current key activates at {activation}, after trusted chain tip {trusted_chain_tip}")]
    PrematureCurrentKey {
        activation: BlockNumber,
        trusted_chain_tip: BlockNumber,
    },
    #[error(
        "next key activated at {activation}, no later than trusted chain tip {trusted_chain_tip}"
    )]
    NextKeyAlreadyActive {
        activation: BlockNumber,
        trusted_chain_tip: BlockNumber,
    },
    #[error("next key activation must be after current key activation")]
    InvalidActivationOrder,
    #[error("current and next keys must have distinct provider-owned ids")]
    DuplicateKeyId,
    #[error("trusted validator signing keys are empty")]
    NoTrustedValidatorKeys,
    #[error("transaction encryption key schedule has no validator attestations")]
    NoAttestations,
    #[error("transaction encryption key schedule has no attestation from a trusted validator")]
    NoTrustedAttestation,
    #[error("trusted validator attestation does not cover the transaction encryption key schedule")]
    InvalidAttestation,
}

/// Failure to seal transaction inputs.
#[derive(Debug, thiserror::Error)]
pub enum TransactionInputSealError {
    #[error("failed to seal the transaction inputs")]
    Seal(#[source] miden_protocol::crypto::ies::IesError),
}

// ATTESTATION
// ================================================================================================

/// Verifies a served transaction encryption key schedule against trusted chain state.
///
/// The schedule is decoded and its activation rules validated before any signature is checked, so
/// a malformed or premature schedule is rejected on its own terms rather than on a signature
/// mismatch.
pub fn verify_transaction_encryption_key_schedule(
    response: &proto::transaction::TransactionEncryptionKeyResponse,
    trusted: TrustedTransactionEncryptionState<'_>,
) -> Result<VerifiedTransactionEncryptionSchedule, TransactionEncryptionKeyError> {
    if trusted.validator_signing_keys.is_empty() {
        return Err(TransactionEncryptionKeyError::NoTrustedValidatorKeys);
    }
    if response.attestations.is_empty() {
        return Err(TransactionEncryptionKeyError::NoAttestations);
    }

    let attestation_epoch = u16::try_from(response.attestation_epoch)
        .map_err(|_| TransactionEncryptionKeyError::InvalidAttestationEpoch)?;
    let trusted_epoch = trusted.chain_tip.block_epoch();
    if attestation_epoch != trusted_epoch {
        return Err(TransactionEncryptionKeyError::StaleAttestation {
            attestation_epoch,
            trusted_epoch,
        });
    }

    let (current_key, current_public_key) = response
        .current_key
        .as_ref()
        .ok_or(TransactionEncryptionKeyError::MissingCurrentKey)
        .and_then(|key| decode_key(key, "current encryption key"))?;
    let next_key = response
        .next_key
        .as_ref()
        .map(|next| {
            let (key, _) = next
                .key
                .as_ref()
                .ok_or(TransactionEncryptionKeyError::MissingNextKey)
                .and_then(|key| decode_key(key, "next encryption key"))?;
            Ok(NextTransactionEncryptionKey {
                key,
                activation_block_num: BlockNumber::from(next.activation_block_num),
            })
        })
        .transpose()?;

    let schedule = TransactionEncryptionKeySchedule {
        current_key,
        current_key_activation_block_num: BlockNumber::from(
            response.current_key_activation_block_num,
        ),
        next_key,
    };
    schedule.validate_at(trusted.chain_tip)?;

    let commitment = schedule.attestation_commitment(trusted.genesis_commitment, attestation_epoch);
    let mut found_trusted_signer = false;

    for attestation in &response.attestations {
        let Ok(validator_public_key) =
            ValidatorPublicKey::read_from_bytes(&attestation.validator_public_key)
        else {
            continue;
        };

        if !trusted.validator_signing_keys.contains(&validator_public_key) {
            continue;
        }
        found_trusted_signer = true;

        let Ok(signature) = ValidatorSignature::read_from_bytes(&attestation.signature) else {
            continue;
        };
        if signature.verify(commitment, &validator_public_key) {
            let current_key = VerifiedTransactionEncryptionKey {
                info: schedule.current_key.clone(),
                public_key: current_public_key,
                genesis_commitment: trusted.genesis_commitment,
            };
            return Ok(VerifiedTransactionEncryptionSchedule { schedule, current_key });
        }
    }

    if found_trusted_signer {
        Err(TransactionEncryptionKeyError::InvalidAttestation)
    } else {
        Err(TransactionEncryptionKeyError::NoTrustedAttestation)
    }
}

/// Appends one key to the attestation transcript.
fn encode_key(payload: &mut Vec<u8>, key: &TransactionEncryptionKeyInfo) {
    payload.extend_from_slice(&key.scheme.as_u32().to_le_bytes());
    extend_with_length_prefixed(payload, &key.key_id, "key id");
    extend_with_length_prefixed(payload, &key.public_key, "public key");
}

/// Appends a length-prefixed field to the attestation transcript.
fn extend_with_length_prefixed(payload: &mut Vec<u8>, field: &[u8], name: &str) {
    let len = u32::try_from(field.len())
        .unwrap_or_else(|_| panic!("{name} length must fit in u32"))
        .to_le_bytes();
    payload.extend_from_slice(&len);
    payload.extend_from_slice(field);
}

/// Rejects a block number which is not the first block of an epoch.
fn validate_epoch_boundary(
    block_num: BlockNumber,
    name: &'static str,
) -> Result<(), TransactionEncryptionKeyError> {
    if BlockNumber::from_epoch(block_num.block_epoch()) != block_num {
        return Err(TransactionEncryptionKeyError::NotEpochBoundary { name, block_num });
    }
    Ok(())
}

/// Validates a key identifier before it is used in a transcript or allocation.
fn validate_key_id(
    key_id: &[u8],
    field: &'static str,
) -> Result<(), TransactionEncryptionKeyError> {
    if key_id.is_empty() {
        return Err(TransactionEncryptionKeyError::EmptyKeyId { field });
    }
    if key_id.len() > MAX_KEY_ID_LEN {
        return Err(TransactionEncryptionKeyError::KeyIdTooLong { field, len: key_id.len() });
    }
    Ok(())
}

/// Decodes one key of the schedule, returning it alongside its decoded public key.
fn decode_key(
    key: &proto::transaction::TransactionEncryptionKey,
    field: &'static str,
) -> Result<(TransactionEncryptionKeyInfo, EncryptionPublicKey), TransactionEncryptionKeyError> {
    let scheme = TransactionEncryptionScheme::try_from(key.scheme)?;
    validate_key_id(&key.key_id, field)?;
    let public_key = EncryptionPublicKey::read_from_bytes(&key.public_key).map_err(|source| {
        TransactionEncryptionKeyError::InvalidEncryptionPublicKey { field, source }
    })?;

    Ok((
        TransactionEncryptionKeyInfo {
            scheme,
            key_id: key.key_id.clone(),
            public_key: key.public_key.clone(),
        },
        public_key,
    ))
}

// SEALER
// ================================================================================================

/// Seals transaction inputs against the validator set's shared encryption key.
///
/// Built from a verified transaction encryption key and reusable for any number of transactions.
/// Holding one avoids re-fetching the key per submission; callers should discard it when the
/// validator reports an unknown key ID.
#[derive(Debug, Clone)]
pub struct TransactionInputsSealer {
    scheme: TransactionEncryptionScheme,
    key_id: Vec<u8>,
    sealing_key: SealingKey,
    genesis_commitment: Word,
}

impl TransactionInputsSealer {
    /// Builds a sealer from a key whose validator attestation has already been verified.
    pub fn new(key: VerifiedTransactionEncryptionKey) -> Self {
        Self {
            scheme: key.info.scheme,
            key_id: key.info.key_id,
            sealing_key: SealingKey::X25519XChaCha20Poly1305(key.public_key),
            genesis_commitment: key.genesis_commitment,
        }
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
            self.scheme.as_u32(),
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
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;

    use super::*;

    const TEST_KEY_ID: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
    const CURRENT_ACTIVATION: u32 = 0;

    fn genesis() -> Word {
        Word::from([1u32, 2, 3, 4])
    }

    fn chain_tip() -> BlockNumber {
        BlockNumber::from(42)
    }

    fn next_activation() -> BlockNumber {
        BlockNumber::from_epoch(1)
    }

    fn tx_id(seed: u32) -> TransactionId {
        TransactionId::new(
            Word::from([seed, 0, 0, 0]),
            Word::from([0, seed, 0, 0]),
            Word::from([0, 0, seed, 0]),
            Word::from([0, 0, 0, seed]),
        )
    }

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::read_from_bytes(&[seed; 32]).expect("test signing key should decode")
    }

    fn key(seed: u8) -> TransactionEncryptionKeyInfo {
        TransactionEncryptionKeyInfo {
            scheme: TransactionEncryptionScheme::X25519XChaCha20Poly1305,
            key_id: vec![seed; 4],
            public_key: KeyExchangeKey::read_from_bytes(&[seed; 32])
                .unwrap()
                .public_key()
                .to_bytes(),
        }
    }

    /// A schedule with the test key id, optionally scheduling a rotation.
    fn schedule(next: bool) -> TransactionEncryptionKeySchedule {
        TransactionEncryptionKeySchedule {
            current_key: TransactionEncryptionKeyInfo { key_id: TEST_KEY_ID.to_vec(), ..key(7) },
            current_key_activation_block_num: BlockNumber::from(CURRENT_ACTIVATION),
            next_key: next.then(|| NextTransactionEncryptionKey {
                key: key(8),
                activation_block_num: next_activation(),
            }),
        }
    }

    fn encode(key: &TransactionEncryptionKeyInfo) -> proto::transaction::TransactionEncryptionKey {
        proto::transaction::TransactionEncryptionKey {
            scheme: key.scheme.as_i32(),
            key_id: key.key_id.clone(),
            public_key: key.public_key.clone(),
        }
    }

    /// An unattested wire schedule, used to check that attestations cannot simply be omitted.
    fn unsigned_response(
        schedule: &TransactionEncryptionKeySchedule,
        attestation_epoch: u16,
    ) -> proto::transaction::TransactionEncryptionKeyResponse {
        proto::transaction::TransactionEncryptionKeyResponse {
            current_key: Some(encode(&schedule.current_key)),
            next_key: schedule.next_key.as_ref().map(|next| {
                proto::transaction::NextTransactionEncryptionKey {
                    key: Some(encode(&next.key)),
                    activation_block_num: next.activation_block_num.as_u32(),
                }
            }),
            current_key_activation_block_num: schedule.current_key_activation_block_num.as_u32(),
            attestation_epoch: u32::from(attestation_epoch),
            attestations: Vec::new(),
        }
    }

    fn signed_response(
        schedule: &TransactionEncryptionKeySchedule,
        attestation_epoch: u16,
        genesis_commitment: Word,
        signer: &SigningKey,
    ) -> proto::transaction::TransactionEncryptionKeyResponse {
        let mut response = unsigned_response(schedule, attestation_epoch);
        let signature = signer
            .sign(schedule.attestation_commitment(genesis_commitment, attestation_epoch))
            .to_bytes();
        response.attestations = vec![proto::transaction::ValidatorKeyAttestation {
            validator_public_key: signer.public_key().to_bytes(),
            signature,
        }];
        response
    }

    /// A schedule signed by the validator committed in trusted chain state verifies.
    #[test]
    fn verifies_schedule_without_rotation() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let schedule = schedule(false);
        let response = signed_response(&schedule, 0, genesis(), &signer);

        let verified = verify_transaction_encryption_key_schedule(
            &response,
            TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys),
        )
        .unwrap();

        assert_eq!(verified.schedule(), &schedule);
        assert_eq!(verified.current_key().info().key_id, TEST_KEY_ID);
        assert_eq!(
            verified.current_key().info().scheme,
            TransactionEncryptionScheme::X25519XChaCha20Poly1305
        );
        assert_eq!(verified.current_key().genesis_commitment(), genesis());
    }

    /// A scheduled rotation is carried through verification intact.
    #[test]
    fn verifies_schedule_with_next_key() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let schedule = schedule(true);
        let response = signed_response(&schedule, 0, genesis(), &signer);

        let verified = verify_transaction_encryption_key_schedule(
            &response,
            TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys),
        )
        .unwrap();

        assert_eq!(verified.schedule(), &schedule);
    }

    /// The single signature covers whether a rotation is scheduled at all, so a relaying node can
    /// neither strip nor inject one.
    #[test]
    fn one_signature_covers_optional_next_presence() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let trusted = TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys);

        let mut stripped = signed_response(&schedule(true), 0, genesis(), &signer);
        stripped.next_key = None;
        assert_matches!(
            verify_transaction_encryption_key_schedule(&stripped, trusted),
            Err(TransactionEncryptionKeyError::InvalidAttestation)
        );

        let mut injected = signed_response(&schedule(false), 0, genesis(), &signer);
        injected.next_key = signed_response(&schedule(true), 0, genesis(), &signer).next_key;
        assert_matches!(
            verify_transaction_encryption_key_schedule(&injected, trusted),
            Err(TransactionEncryptionKeyError::InvalidAttestation)
        );
    }

    /// A schedule attested in an earlier epoch cannot be replayed to keep a retired key in use.
    #[test]
    fn rejects_stale_schedule_replay() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let response = signed_response(&schedule(false), 0, genesis(), &signer);

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &response,
                TrustedTransactionEncryptionState::new(
                    genesis(),
                    BlockNumber::from_epoch(1),
                    &trusted_keys,
                ),
            ),
            Err(TransactionEncryptionKeyError::StaleAttestation {
                attestation_epoch: 0,
                trusted_epoch: 1,
            })
        );
    }

    /// A current key which has not activated yet is rejected.
    #[test]
    fn rejects_premature_current_key() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let mut schedule = schedule(false);
        schedule.current_key_activation_block_num = BlockNumber::from_epoch(1);
        let response = signed_response(&schedule, 0, genesis(), &signer);

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &response,
                TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys),
            ),
            Err(TransactionEncryptionKeyError::PrematureCurrentKey { .. })
        );
    }

    /// Keys may activate only at epoch boundaries.
    #[test]
    fn rejects_non_boundary_activation() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let mut schedule = schedule(true);
        schedule.next_key.as_mut().unwrap().activation_block_num =
            BlockNumber::from(next_activation().as_u32() + 1);
        let response = signed_response(&schedule, 0, genesis(), &signer);

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &response,
                TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys),
            ),
            Err(TransactionEncryptionKeyError::NotEpochBoundary { .. })
        );
    }

    /// A scheduled key which the chain tip has already passed is rejected, as is one that reuses
    /// the current key's id.
    #[test]
    fn rejects_invalid_next_key_schedule() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let trusted = TrustedTransactionEncryptionState::new(
            genesis(),
            BlockNumber::from_epoch(1),
            &trusted_keys,
        );

        let mut already_active = schedule(true);
        already_active.current_key_activation_block_num = BlockNumber::from_epoch(1);
        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &signed_response(&already_active, 1, genesis(), &signer),
                trusted,
            ),
            Err(TransactionEncryptionKeyError::NextKeyAlreadyActive { .. })
        );

        let mut duplicate_id = schedule(true);
        duplicate_id.current_key_activation_block_num = BlockNumber::from_epoch(1);
        duplicate_id.next_key.as_mut().unwrap().activation_block_num = BlockNumber::from_epoch(2);
        duplicate_id.next_key.as_mut().unwrap().key.key_id =
            duplicate_id.current_key.key_id.clone();
        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &signed_response(&duplicate_id, 1, genesis(), &signer),
                trusted,
            ),
            Err(TransactionEncryptionKeyError::DuplicateKeyId)
        );
    }

    /// An untrusted RPC cannot omit or rely on a malformed validator attestation.
    #[test]
    fn rejects_missing_and_malformed_attestations() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let trusted = TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys);

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &unsigned_response(&schedule(false), 0),
                trusted,
            ),
            Err(TransactionEncryptionKeyError::NoAttestations)
        );

        let mut malformed_key = signed_response(&schedule(false), 0, genesis(), &signer);
        malformed_key.attestations[0].validator_public_key.clear();
        assert_matches!(
            verify_transaction_encryption_key_schedule(&malformed_key, trusted),
            Err(TransactionEncryptionKeyError::NoTrustedAttestation)
        );

        let mut malformed_signature = signed_response(&schedule(false), 0, genesis(), &signer);
        malformed_signature.attestations[0].signature.clear();
        assert_matches!(
            verify_transaction_encryption_key_schedule(&malformed_signature, trusted),
            Err(TransactionEncryptionKeyError::InvalidAttestation)
        );
    }

    /// A malformed attestation does not hide a later valid attestation.
    #[test]
    fn skips_malformed_attestations() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let mut response = signed_response(&schedule(false), 0, genesis(), &signer);
        response.attestations.insert(
            0,
            proto::transaction::ValidatorKeyAttestation {
                validator_public_key: Vec::new(),
                signature: Vec::new(),
            },
        );

        verify_transaction_encryption_key_schedule(
            &response,
            TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys),
        )
        .unwrap();
    }

    /// A valid signature does not help when its signer is absent from trusted chain state.
    #[test]
    fn rejects_untrusted_validator_attestation() {
        let trusted_signer = signing_key(1);
        let untrusted_signer = signing_key(2);
        let trusted_keys = [trusted_signer.public_key()];

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &signed_response(&schedule(false), 0, genesis(), &untrusted_signer),
                TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys),
            ),
            Err(TransactionEncryptionKeyError::NoTrustedAttestation)
        );
    }

    /// Verification requires trusted signing keys to check against at all.
    #[test]
    fn rejects_empty_trusted_validator_keys() {
        let signer = signing_key(1);

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &signed_response(&schedule(false), 0, genesis(), &signer),
                TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &[]),
            ),
            Err(TransactionEncryptionKeyError::NoTrustedValidatorKeys)
        );
    }

    /// Every attested schedule field and the network identity are covered by the signature.
    #[test]
    fn rejects_changed_attested_fields() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let trusted = TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys);
        let response = signed_response(&schedule(false), 0, genesis(), &signer);

        let mut changed_key_id = response.clone();
        changed_key_id.current_key.as_mut().unwrap().key_id[0] ^= 1;
        let mut changed_public_key = response.clone();
        changed_public_key.current_key.as_mut().unwrap().public_key =
            KeyExchangeKey::read_from_bytes(&[9u8; 32]).unwrap().public_key().to_bytes();

        for changed in [changed_key_id, changed_public_key] {
            assert_matches!(
                verify_transaction_encryption_key_schedule(&changed, trusted),
                Err(TransactionEncryptionKeyError::InvalidAttestation)
            );
        }

        assert_matches!(
            verify_transaction_encryption_key_schedule(
                &response,
                TrustedTransactionEncryptionState::new(
                    Word::from([9u32, 9, 9, 9]),
                    chain_tip(),
                    &trusted_keys,
                ),
            ),
            Err(TransactionEncryptionKeyError::InvalidAttestation)
        );
    }

    /// Key metadata is bounded and decoded before it can become domain state.
    #[test]
    fn rejects_invalid_key_metadata() {
        let signer = signing_key(1);
        let trusted_keys = [signer.public_key()];
        let trusted = TrustedTransactionEncryptionState::new(genesis(), chain_tip(), &trusted_keys);

        let mut unspecified_scheme = signed_response(&schedule(false), 0, genesis(), &signer);
        unspecified_scheme.current_key.as_mut().unwrap().scheme = 0;
        assert_matches!(
            verify_transaction_encryption_key_schedule(&unspecified_scheme, trusted),
            Err(TransactionEncryptionKeyError::UnspecifiedScheme)
        );

        let mut empty_key_id = signed_response(&schedule(false), 0, genesis(), &signer);
        empty_key_id.current_key.as_mut().unwrap().key_id.clear();
        assert_matches!(
            verify_transaction_encryption_key_schedule(&empty_key_id, trusted),
            Err(TransactionEncryptionKeyError::EmptyKeyId { .. })
        );

        let mut oversized_key_id = signed_response(&schedule(false), 0, genesis(), &signer);
        oversized_key_id.current_key.as_mut().unwrap().key_id = vec![0; MAX_KEY_ID_LEN + 1];
        assert_matches!(
            verify_transaction_encryption_key_schedule(&oversized_key_id, trusted),
            Err(TransactionEncryptionKeyError::KeyIdTooLong { .. })
        );

        let mut invalid_public_key = signed_response(&schedule(false), 0, genesis(), &signer);
        invalid_public_key.current_key.as_mut().unwrap().public_key.clear();
        assert_matches!(
            verify_transaction_encryption_key_schedule(&invalid_public_key, trusted),
            Err(TransactionEncryptionKeyError::InvalidEncryptionPublicKey { .. })
        );

        let mut missing_current_key = signed_response(&schedule(false), 0, genesis(), &signer);
        missing_current_key.current_key = None;
        assert_matches!(
            verify_transaction_encryption_key_schedule(&missing_current_key, trusted),
            Err(TransactionEncryptionKeyError::MissingCurrentKey)
        );

        let mut missing_next_key = signed_response(&schedule(true), 0, genesis(), &signer);
        missing_next_key.next_key.as_mut().unwrap().key = None;
        assert_matches!(
            verify_transaction_encryption_key_schedule(&missing_next_key, trusted),
            Err(TransactionEncryptionKeyError::MissingNextKey)
        );
    }

    /// Pins the attestation transcript byte-for-byte, which also pins *which* fields it binds.
    #[test]
    fn attestation_transcript_is_stable() {
        let schedule = schedule(true);
        let commitment = schedule.attestation_commitment(genesis(), 3);

        let current = &schedule.current_key;
        let next = schedule.next_key.as_ref().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(b"MIDEN_TX_ENCRYPTION_KEY_SCHEDULE_ATTESTATION_V2");
        expected.extend_from_slice(&genesis().to_bytes());
        expected.extend_from_slice(&3u32.to_le_bytes());
        expected.extend_from_slice(&CURRENT_ACTIVATION.to_le_bytes());
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&current.key_id);
        expected.extend_from_slice(&32u32.to_le_bytes());
        expected.extend_from_slice(&current.public_key);
        expected.push(1);
        expected.extend_from_slice(&next_activation().as_u32().to_le_bytes());
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&next.key.key_id);
        expected.extend_from_slice(&32u32.to_le_bytes());
        expected.extend_from_slice(&next.key.public_key);

        assert_eq!(commitment, miden_protocol::Hasher::hash(&expected));
    }

    /// Pins the sealing transcript byte-for-byte, which also pins *which* fields it binds.
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
}
