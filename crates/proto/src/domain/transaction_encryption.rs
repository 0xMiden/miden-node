use miden_protocol::Word;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::PublicKey as EncryptionPublicKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};

use crate::generated as proto;

/// Domain tag for signatures over complete transaction encryption key schedules.
pub const ATTESTATION_DOMAIN: &[u8] = b"MIDEN_TX_ENCRYPTION_KEY_SCHEDULE_ATTESTATION_V2";

/// Public metadata for one provider-owned transaction encryption key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionEncryptionKeyInfo {
    /// Wire identifier of the encryption scheme.
    pub scheme: u32,
    /// Opaque identifier assigned by the provider.
    pub key_id: Vec<u8>,
    /// Serialized public key.
    pub public_key: Vec<u8>,
}

/// A key scheduled to become current at an epoch boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextTransactionEncryptionKey {
    pub key: TransactionEncryptionKeyInfo,
    pub activation_block_num: BlockNumber,
}

/// The complete transaction encryption key schedule served by a validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionEncryptionKeySchedule {
    pub current_key: TransactionEncryptionKeyInfo,
    pub current_key_activation_block_num: BlockNumber,
    pub next_key: Option<NextTransactionEncryptionKey>,
}

impl TransactionEncryptionKeySchedule {
    /// Computes the commitment signed by validators for this schedule in `attestation_epoch`.
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

    /// Validates activation rules against a trusted chain tip.
    pub fn validate_at(
        &self,
        trusted_chain_tip: BlockNumber,
    ) -> Result<(), TransactionEncryptionKeyScheduleError> {
        validate_epoch_boundary(self.current_key_activation_block_num, "current key activation")?;
        if self.current_key_activation_block_num > trusted_chain_tip {
            return Err(TransactionEncryptionKeyScheduleError::PrematureCurrentKey {
                activation: self.current_key_activation_block_num,
                trusted_chain_tip,
            });
        }

        validate_key(&self.current_key, "current")?;

        if let Some(next) = &self.next_key {
            validate_epoch_boundary(next.activation_block_num, "next key activation")?;
            if next.activation_block_num <= trusted_chain_tip {
                return Err(TransactionEncryptionKeyScheduleError::NextKeyAlreadyActive {
                    activation: next.activation_block_num,
                    trusted_chain_tip,
                });
            }
            if next.activation_block_num <= self.current_key_activation_block_num {
                return Err(TransactionEncryptionKeyScheduleError::InvalidActivationOrder);
            }
            validate_key(&next.key, "next")?;
            if next.key.key_id == self.current_key.key_id {
                return Err(TransactionEncryptionKeyScheduleError::DuplicateKeyId);
            }
        }

        Ok(())
    }
}

/// Trusted chain information required to verify a served key schedule.
pub struct TrustedChainState<'a> {
    pub genesis_commitment: Word,
    pub chain_tip: BlockNumber,
    pub validator_keys: &'a [PublicKey],
}

/// Parses and verifies a transaction encryption key schedule against trusted chain state.
pub fn verify_transaction_encryption_key_schedule(
    response: &proto::transaction::TransactionEncryptionKeyResponse,
    trusted: &TrustedChainState<'_>,
) -> Result<TransactionEncryptionKeySchedule, TransactionEncryptionKeyScheduleError> {
    let attestation_epoch = u16::try_from(response.attestation_epoch)
        .map_err(|_| TransactionEncryptionKeyScheduleError::InvalidAttestationEpoch)?;
    let trusted_epoch = trusted.chain_tip.block_epoch();
    if attestation_epoch != trusted_epoch {
        return Err(TransactionEncryptionKeyScheduleError::StaleAttestation {
            attestation_epoch,
            trusted_epoch,
        });
    }

    let current_key = response
        .current_key
        .as_ref()
        .ok_or(TransactionEncryptionKeyScheduleError::MissingCurrentKey)
        .and_then(|key| decode_key(key, "current"))?;
    let next_key = response
        .next_key
        .as_ref()
        .map(|next| {
            let key = next
                .key
                .as_ref()
                .ok_or(TransactionEncryptionKeyScheduleError::MissingNextKey)
                .and_then(|key| decode_key(key, "next"))?;
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
    let verified = response.attestations.iter().any(|attestation| {
        let Ok(validator_key) = PublicKey::read_from_bytes(&attestation.validator_public_key)
        else {
            return false;
        };
        if !trusted.validator_keys.contains(&validator_key) {
            return false;
        }
        let Ok(signature) = Signature::read_from_bytes(&attestation.signature) else {
            return false;
        };
        signature.verify(commitment, &validator_key)
    });
    if !verified {
        return Err(TransactionEncryptionKeyScheduleError::NoTrustedAttestation);
    }

    Ok(schedule)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransactionEncryptionKeyScheduleError {
    #[error("the response is missing its current key")]
    MissingCurrentKey,
    #[error("the response contains a next-key wrapper without a key")]
    MissingNextKey,
    #[error("the {0} key uses an unsupported encryption scheme")]
    UnsupportedScheme(&'static str),
    #[error("the {0} key id is empty")]
    EmptyKeyId(&'static str),
    #[error("the {0} public key is invalid")]
    InvalidPublicKey(&'static str),
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
    #[error("the response has no valid attestation from a trusted validator")]
    NoTrustedAttestation,
}

fn encode_key(payload: &mut Vec<u8>, key: &TransactionEncryptionKeyInfo) {
    payload.extend_from_slice(&key.scheme.to_le_bytes());
    extend_with_length_prefixed(payload, &key.key_id, "key id");
    extend_with_length_prefixed(payload, &key.public_key, "public key");
}

fn extend_with_length_prefixed(payload: &mut Vec<u8>, field: &[u8], name: &str) {
    let len = u32::try_from(field.len())
        .unwrap_or_else(|_| panic!("{name} length must fit in u32"))
        .to_le_bytes();
    payload.extend_from_slice(&len);
    payload.extend_from_slice(field);
}

fn validate_epoch_boundary(
    block_num: BlockNumber,
    name: &'static str,
) -> Result<(), TransactionEncryptionKeyScheduleError> {
    if BlockNumber::from_epoch(block_num.block_epoch()) != block_num {
        return Err(TransactionEncryptionKeyScheduleError::NotEpochBoundary { name, block_num });
    }
    Ok(())
}

fn validate_key(
    key: &TransactionEncryptionKeyInfo,
    role: &'static str,
) -> Result<(), TransactionEncryptionKeyScheduleError> {
    if key.scheme != u32::from(proto::transaction::IesScheme::X25519Xchacha20Poly1305 as u8) {
        return Err(TransactionEncryptionKeyScheduleError::UnsupportedScheme(role));
    }
    if key.key_id.is_empty() {
        return Err(TransactionEncryptionKeyScheduleError::EmptyKeyId(role));
    }
    EncryptionPublicKey::read_from_bytes(&key.public_key)
        .map_err(|_| TransactionEncryptionKeyScheduleError::InvalidPublicKey(role))?;
    Ok(())
}

fn decode_key(
    key: &proto::transaction::TransactionEncryptionKey,
    role: &'static str,
) -> Result<TransactionEncryptionKeyInfo, TransactionEncryptionKeyScheduleError> {
    let scheme = u32::try_from(key.scheme)
        .map_err(|_| TransactionEncryptionKeyScheduleError::UnsupportedScheme(role))?;
    let key = TransactionEncryptionKeyInfo {
        scheme,
        key_id: key.key_id.clone(),
        public_key: key.public_key.clone(),
    };
    validate_key(&key, role)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;

    use super::*;

    const CURRENT_ACTIVATION: u32 = 0;
    const NEXT_ACTIVATION: u32 = 1 << 16;

    fn key(seed: u8) -> TransactionEncryptionKeyInfo {
        let public_key = KeyExchangeKey::read_from_bytes(&[seed; 32]).unwrap().public_key();
        TransactionEncryptionKeyInfo {
            scheme: u32::from(proto::transaction::IesScheme::X25519Xchacha20Poly1305 as u8),
            key_id: public_key.to_commitment().to_bytes(),
            public_key: public_key.to_bytes(),
        }
    }

    fn signed_response(
        schedule: &TransactionEncryptionKeySchedule,
        attestation_epoch: u16,
        genesis: Word,
        signer: &SigningKey,
    ) -> proto::transaction::TransactionEncryptionKeyResponse {
        let encode =
            |key: &TransactionEncryptionKeyInfo| proto::transaction::TransactionEncryptionKey {
                scheme: i32::try_from(key.scheme).unwrap(),
                key_id: key.key_id.clone(),
                public_key: key.public_key.clone(),
            };
        let signature = signer
            .sign(schedule.attestation_commitment(genesis, attestation_epoch))
            .to_bytes();
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
            attestations: vec![proto::transaction::ValidatorKeyAttestation {
                validator_public_key: signer.public_key().to_bytes(),
                signature,
            }],
        }
    }

    fn schedule(next: bool) -> TransactionEncryptionKeySchedule {
        TransactionEncryptionKeySchedule {
            current_key: key(7),
            current_key_activation_block_num: BlockNumber::from(CURRENT_ACTIVATION),
            next_key: next.then(|| NextTransactionEncryptionKey {
                key: key(8),
                activation_block_num: BlockNumber::from(NEXT_ACTIVATION),
            }),
        }
    }

    #[test]
    fn verifies_schedule_without_rotation() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let schedule = schedule(false);
        let response = signed_response(&schedule, 0, genesis, &signer);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from(42),
            validator_keys: &[signer.public_key()],
        };

        assert_eq!(
            verify_transaction_encryption_key_schedule(&response, &trusted).unwrap(),
            schedule
        );
    }

    #[test]
    fn verifies_schedule_with_next_key() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let schedule = schedule(true);
        let response = signed_response(&schedule, 0, genesis, &signer);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from(42),
            validator_keys: &[signer.public_key()],
        };

        assert_eq!(
            verify_transaction_encryption_key_schedule(&response, &trusted).unwrap(),
            schedule
        );
    }

    #[test]
    fn one_signature_covers_optional_next_presence() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from(42),
            validator_keys: &[signer.public_key()],
        };

        let mut stripped = signed_response(&schedule(true), 0, genesis, &signer);
        stripped.next_key = None;
        assert_eq!(
            verify_transaction_encryption_key_schedule(&stripped, &trusted),
            Err(TransactionEncryptionKeyScheduleError::NoTrustedAttestation)
        );

        let mut injected = signed_response(&schedule(false), 0, genesis, &signer);
        injected.next_key = signed_response(&schedule(true), 0, genesis, &signer).next_key;
        assert_eq!(
            verify_transaction_encryption_key_schedule(&injected, &trusted),
            Err(TransactionEncryptionKeyScheduleError::NoTrustedAttestation)
        );
    }

    #[test]
    fn rejects_stale_schedule_replay() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let response = signed_response(&schedule(false), 0, genesis, &signer);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from_epoch(1),
            validator_keys: &[signer.public_key()],
        };

        assert_eq!(
            verify_transaction_encryption_key_schedule(&response, &trusted),
            Err(TransactionEncryptionKeyScheduleError::StaleAttestation {
                attestation_epoch: 0,
                trusted_epoch: 1,
            })
        );
    }

    #[test]
    fn rejects_premature_current_key() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let mut schedule = schedule(false);
        schedule.current_key_activation_block_num = BlockNumber::from_epoch(1);
        let response = signed_response(&schedule, 0, genesis, &signer);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from(42),
            validator_keys: &[signer.public_key()],
        };

        assert!(matches!(
            verify_transaction_encryption_key_schedule(&response, &trusted),
            Err(TransactionEncryptionKeyScheduleError::PrematureCurrentKey { .. })
        ));
    }

    #[test]
    fn rejects_non_boundary_activation() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let mut schedule = schedule(true);
        schedule.next_key.as_mut().unwrap().activation_block_num =
            BlockNumber::from(NEXT_ACTIVATION + 1);
        let response = signed_response(&schedule, 0, genesis, &signer);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from(42),
            validator_keys: &[signer.public_key()],
        };

        assert!(matches!(
            verify_transaction_encryption_key_schedule(&response, &trusted),
            Err(TransactionEncryptionKeyScheduleError::NotEpochBoundary { .. })
        ));
    }

    #[test]
    fn rejects_untrusted_validator() {
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let other = SigningKey::read_from_bytes(&[10; 32]).unwrap();
        let genesis = Word::from([1_u32, 2, 3, 4]);
        let response = signed_response(&schedule(false), 0, genesis, &signer);
        let trusted = TrustedChainState {
            genesis_commitment: genesis,
            chain_tip: BlockNumber::from(42),
            validator_keys: &[other.public_key()],
        };

        assert_eq!(
            verify_transaction_encryption_key_schedule(&response, &trusted),
            Err(TransactionEncryptionKeyScheduleError::NoTrustedAttestation)
        );
    }
}
