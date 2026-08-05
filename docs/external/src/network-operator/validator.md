---
title: "Validator"
sidebar_position: 5
---

# Validator

The validator provides independent verification of Miden blocks before they can be committed. On official networks, it
is operated by a separate entity from the network operator. Network operators configure their sequencer to use the
official validator endpoint rather than running their own validator for that network.

For unofficial or private networks, this separation matters less and the validator can be run as an internal service. It
should not be exposed publicly.

Since the validator sees every block before it is committed, it also stores the raw block data for the blocks it
validates and signs. This makes the validator a network data backup that can be used to recover committed block data if
the sequencer or full-node replicas lose data.

The validator is also a temporary training-wheels layer while the proof and VM systems mature. It receives the private
inputs needed to independently check proposed blocks, which gives the network another place to detect bugs before a
block is committed. Those inputs arrive encrypted against the shared transaction encryption key, so the validator is the
only component that can read them, and submissions that are not encrypted are rejected.

## Key Rotation

Each block header includes the validator key that must be used for the next block. Because the current validator signs
the block header, this next-key commitment is authenticated by the existing validator key. This makes validator key
rotation safe: the network can verify that the next validator key was authorized by the validator that signed the
current block.

## Start

```bash
miden-validator start \
  --listen 0.0.0.0:50101 \
  --data-directory validator-data \
  --storage-key.epoch <32-byte-hex-epoch> \
  --storage-key.setup-context <setup-context-file> \
  --storage-key.public-key-set <public-key-set-file> \
  --storage-key.secret-share <secret-share-file>
```

For local development, the validator can use its default insecure development key. Production deployments should
configure validator signing explicitly, either with a local key or with KMS-backed signing.

In addition to its signing key, every validator holds the shared transaction encryption key, configured with
`--encryption-key.hex` or `MIDEN_VALIDATOR_ENCRYPTION_KEY`. Unlike the signing key, this value must be identical across
every validator in the set. The validator does not derive replacement keys or rotate it automatically. The validator
logs a warning at startup if the insecure development default is in use, and another if a key was loaded from plain hex
rather than a KMS ciphertext.

The validator accepts an optional previous key for grace decryption and an optional next key for a planned rotation.
Each key has an activation block, which must be an epoch boundary. Configure these with `--encryption-key.previous.*`,
`--encryption-key.activation-block`, and `--encryption-key.next.*`. Hex and KMS ciphertext sources are supported for all
three keys.

All validators must restart with the same key schedule whenever the configured keys change. Keys then activate at their
epoch boundaries without another restart. For example, a restart that activates key B and announces key C uses A as the
previous key, B as the current key, and C as the next key. The provider accepts A through B's activation epoch, then
marks it expired. After that epoch, a restart can drop A. When C activates, move B into the previous slot.

The validator logs a warning when any key is loaded from plain hex. Production deployments should instead wrap each key
with a symmetric AWS KMS key (`aws kms encrypt`) and pass the resulting base64 ciphertext blob unchanged via
`--encryption-key.kms-ciphertext` or `MIDEN_VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT`. The validator recovers the key
material at startup with `kms:Decrypt`, so its AWS identity needs that permission on the wrapping key. Note that, unlike
KMS-backed signing, the decrypted encryption key is held in validator memory: AWS KMS cannot perform X25519 key
agreement itself, so envelope encryption is the supported provisioning path. Other providers may keep the secret outside
the validator process because the provider contract requires only public schedule metadata and a decrypt operation.

Each validator must run inside its trusted execution environment. If transaction proving uses a remote prover, that
prover also receives the plaintext inputs and must run inside the same trusted boundary.

This version requires a fresh validator database. Phase 1 client ciphertext cannot be converted into Golden records.

The files contain canonical Golden wire bytes. Every validator uses the same setup context and public key set, but uses
its own secret share. The validator will not start if any storage key option is missing or the key material is invalid.
After validation, it stores only the transaction ID and the Golden threshold record. It does not store the client
ciphertext.

Use `miden-validator start --help` for the complete current option list.
