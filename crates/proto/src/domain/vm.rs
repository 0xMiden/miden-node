use miden_core::deferred::{DeferredStateWire, Tag, WireEntry};
use miden_core::proof::{DeferredProof, ExecutionProof, HashFunction, StarkProof};
use miden_protocol::{Felt, Word};

use crate::decode::{ConversionResultExt, GrpcDecodeExt};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

impl From<HashFunction> for proto::vm::ExecutionProofHashFunction {
    fn from(value: HashFunction) -> Self {
        match value {
            HashFunction::Blake3_256 => Self::Blake3256,
            HashFunction::Rpo256 => Self::Rpo256,
            HashFunction::Rpx256 => Self::Rpx256,
            HashFunction::Poseidon2 => Self::Poseidon2,
            HashFunction::Keccak => Self::Keccak,
        }
    }
}

fn decode_hash_function(value: i32) -> Result<HashFunction, ConversionError> {
    match proto::vm::ExecutionProofHashFunction::try_from(value) {
        Ok(proto::vm::ExecutionProofHashFunction::Blake3256) => Ok(HashFunction::Blake3_256),
        Ok(proto::vm::ExecutionProofHashFunction::Rpo256) => Ok(HashFunction::Rpo256),
        Ok(proto::vm::ExecutionProofHashFunction::Rpx256) => Ok(HashFunction::Rpx256),
        Ok(proto::vm::ExecutionProofHashFunction::Poseidon2) => Ok(HashFunction::Poseidon2),
        Ok(proto::vm::ExecutionProofHashFunction::Keccak) => Ok(HashFunction::Keccak),
        Ok(proto::vm::ExecutionProofHashFunction::Unspecified) => {
            Err(ConversionError::message("execution proof hash function is unspecified"))
        },
        Err(_) => Err(ConversionError::message(format!(
            "unknown execution proof hash function value {value}"
        ))),
    }
}

impl From<&StarkProof> for proto::vm::StarkProof {
    fn from(value: &StarkProof) -> Self {
        Self {
            proof: value.bytes().to_vec(),
            hash_function: proto::vm::ExecutionProofHashFunction::from(value.hash_fn()) as i32,
        }
    }
}

impl From<StarkProof> for proto::vm::StarkProof {
    fn from(value: StarkProof) -> Self {
        let (proof, hash_function) = value.into_parts();
        Self {
            proof,
            hash_function: proto::vm::ExecutionProofHashFunction::from(hash_function) as i32,
        }
    }
}

impl TryFrom<proto::vm::StarkProof> for StarkProof {
    type Error = ConversionError;

    fn try_from(value: proto::vm::StarkProof) -> Result<Self, Self::Error> {
        let hash_function = decode_hash_function(value.hash_function).context("hash_function")?;
        Ok(Self::new(value.proof, hash_function))
    }
}

fn encode_wire_entry(entry: &WireEntry) -> proto::vm::DeferredWireEntry {
    use proto::vm::deferred_wire_entry::Entry;

    let (tag, entry) = match entry {
        WireEntry::Data { tag, chunks } => {
            let chunks = chunks
                .iter()
                .map(|chunk| proto::vm::DeferredDataChunk {
                    elements: chunk.iter().map(Into::into).collect(),
                })
                .collect();
            (
                Word::new((*tag).as_word()).into(),
                Entry::Data(proto::vm::DeferredData { chunks }),
            )
        },
        WireEntry::Join { tag, lhs, rhs } => (
            Word::new((*tag).as_word()).into(),
            Entry::Join(proto::vm::DeferredJoin { lhs: *lhs, rhs: *rhs }),
        ),
        WireEntry::PairList { tag, pairs } => {
            let pairs = pairs
                .iter()
                .map(|(lhs, rhs)| proto::vm::DeferredIndexPair { lhs: *lhs, rhs: *rhs })
                .collect();
            (
                Word::new((*tag).as_word()).into(),
                Entry::PairList(proto::vm::DeferredPairList { pairs }),
            )
        },
    };

    proto::vm::DeferredWireEntry { tag: Some(tag), entry: Some(entry) }
}

fn decode_wire_entry(
    value: proto::vm::DeferredWireEntry,
    index: usize,
) -> Result<WireEntry, ConversionError> {
    use proto::vm::deferred_wire_entry::Entry;

    let decoder = value.decoder();
    let tag: Word = decode!(decoder, value.tag)?;
    let tag = Tag::from_word(tag.into_elements());
    let entry = value
        .entry
        .ok_or_else(|| ConversionError::missing_field::<proto::vm::DeferredWireEntry>("entry"))?;

    let max_child = u32::try_from(index)
        .map_err(|_| ConversionError::message("too many deferred wire entries"))?;
    let validate_child = |child: u32, field: &str| {
        if child > max_child {
            Err(ConversionError::message(format!(
                "child index {child} must refer to TRUE or an earlier entry"
            ))
            .context(field))
        } else {
            Ok(())
        }
    };

    match entry {
        Entry::Data(data) => {
            if data.chunks.is_empty() {
                return Err(ConversionError::message("data entry must contain at least one chunk")
                    .context("data.chunks"));
            }
            let chunks = data
                .chunks
                .into_iter()
                .enumerate()
                .map(|(chunk_index, chunk)| {
                    if chunk.elements.len() != 8 {
                        return Err(ConversionError::message(format!(
                            "deferred data chunk must contain exactly 8 elements, got {}",
                            chunk.elements.len()
                        ))
                        .context(format!("data.chunks[{chunk_index}].elements")));
                    }
                    let elements = chunk
                        .elements
                        .into_iter()
                        .enumerate()
                        .map(|(element_index, element)| {
                            Felt::try_from(element).context(format!("elements[{element_index}]"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    elements.try_into().map_err(|_| {
                        ConversionError::message("deferred data chunk has invalid length")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(WireEntry::Data { tag, chunks })
        },
        Entry::Join(join) => {
            validate_child(join.lhs, "join.lhs")?;
            validate_child(join.rhs, "join.rhs")?;
            Ok(WireEntry::Join { tag, lhs: join.lhs, rhs: join.rhs })
        },
        Entry::PairList(pair_list) => {
            if pair_list.pairs.is_empty() {
                return Err(ConversionError::message(
                    "pair-list entry must contain at least one pair",
                )
                .context("pair_list.pairs"));
            }
            let pairs = pair_list
                .pairs
                .into_iter()
                .enumerate()
                .map(|(pair_index, pair)| {
                    validate_child(pair.lhs, &format!("pair_list.pairs[{pair_index}].lhs"))?;
                    validate_child(pair.rhs, &format!("pair_list.pairs[{pair_index}].rhs"))?;
                    Ok((pair.lhs, pair.rhs))
                })
                .collect::<Result<Vec<_>, ConversionError>>()?;
            Ok(WireEntry::PairList { tag, pairs })
        },
    }
}

impl From<&DeferredProof> for proto::vm::DeferredProof {
    fn from(value: &DeferredProof) -> Self {
        use proto::vm::deferred_proof::Proof;

        let proof = match value {
            DeferredProof::Empty => Proof::Empty(proto::vm::EmptyDeferredProof {}),
            DeferredProof::Wire(wire) => Proof::Wire(proto::vm::DeferredStateWire {
                entries: wire.entries.iter().map(encode_wire_entry).collect(),
            }),
            DeferredProof::Stark { proof, public_root } => {
                Proof::Stark(proto::vm::DeferredStarkProof {
                    proof: Some(proof.into()),
                    public_root: Some(public_root.into()),
                })
            },
        };
        Self { proof: Some(proof) }
    }
}

impl TryFrom<proto::vm::DeferredProof> for DeferredProof {
    type Error = ConversionError;

    fn try_from(value: proto::vm::DeferredProof) -> Result<Self, Self::Error> {
        use proto::vm::deferred_proof::Proof;

        match value.proof {
            Some(Proof::Empty(_)) => Ok(Self::Empty),
            Some(Proof::Wire(wire)) => {
                let entries = wire
                    .entries
                    .into_iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        decode_wire_entry(entry, index).context(format!("wire.entries[{index}]"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Wire(DeferredStateWire { entries }))
            },
            Some(Proof::Stark(stark)) => {
                let decoder = stark.decoder();
                let proof = decode!(decoder, stark.proof)?;
                let public_root = decode!(decoder, stark.public_root)?;
                Ok(Self::Stark { proof, public_root })
            },
            None => Err(ConversionError::missing_field::<proto::vm::DeferredProof>("proof")),
        }
    }
}

impl From<&ExecutionProof> for proto::vm::ExecutionProof {
    fn from(value: &ExecutionProof) -> Self {
        Self {
            miden: Some(value.miden_proof().into()),
            deferred: Some(value.deferred_proof().into()),
        }
    }
}

impl From<ExecutionProof> for proto::vm::ExecutionProof {
    fn from(value: ExecutionProof) -> Self {
        Self::from(&value)
    }
}

impl TryFrom<proto::vm::ExecutionProof> for ExecutionProof {
    type Error = ConversionError;

    fn try_from(value: proto::vm::ExecutionProof) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let miden = decode!(decoder, value.miden)?;
        let deferred = decode!(decoder, value.deferred)?;
        Ok(Self::new(miden, deferred))
    }
}

#[cfg(test)]
mod tests {
    use miden_core::deferred::{DeferredStateWire, Tag, WireEntry};
    use miden_core::proof::{DeferredProof, ExecutionProof, HashFunction, StarkProof};
    use miden_protocol::{Felt, Word};

    use crate::generated as proto;

    #[test]
    fn execution_proof_roundtrips_all_variants() {
        let hashes = [
            HashFunction::Blake3_256,
            HashFunction::Rpo256,
            HashFunction::Rpx256,
            HashFunction::Poseidon2,
            HashFunction::Keccak,
        ];
        for hash in hashes {
            let proofs = [
                ExecutionProof::new(StarkProof::new(vec![1, 2, 3], hash), DeferredProof::Empty),
                ExecutionProof::new(
                    StarkProof::new(vec![4], hash),
                    DeferredProof::Wire(DeferredStateWire {
                        entries: vec![WireEntry::Data {
                            tag: Tag::from_word(Word::from([3_u32, 4, 5, 6]).into_elements()),
                            chunks: vec![[Felt::new_unchecked(7); 8]],
                        }],
                    }),
                ),
                ExecutionProof::new(
                    StarkProof::new(vec![], hash),
                    DeferredProof::Stark {
                        proof: StarkProof::new(vec![8, 9], HashFunction::Poseidon2),
                        public_root: Word::from([10_u32, 11, 12, 13]),
                    },
                ),
            ];
            for proof in proofs {
                let encoded = proto::vm::ExecutionProof::from(&proof);
                assert_eq!(ExecutionProof::try_from(encoded).unwrap(), proof);
            }
        }
    }

    #[test]
    fn rejects_unspecified_and_unknown_hash_functions() {
        for hash_function in [0, 99] {
            let error =
                StarkProof::try_from(proto::vm::StarkProof { proof: Vec::new(), hash_function })
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("hash function"));
        }
    }
}
