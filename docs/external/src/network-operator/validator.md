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
  --data-directory validator-data
```

For local development, the validator can use its default insecure development key. Production deployments should
configure validator signing explicitly, either with a local key or with KMS-backed signing.

In addition to its signing key, every validator holds the shared transaction encryption key, configured with
`--encryption-key.hex` or `MIDEN_VALIDATOR_ENCRYPTION_KEY`. Unlike the signing key, this value must be identical across
every validator in the set. The validator logs a warning at startup if the insecure development default is in use, and
always logs the resolved key id so you can confirm which key is live.

Production deployments should not pass the secret in plaintext. Instead, wrap it with a symmetric AWS KMS key
(`aws kms encrypt`) and pass the resulting base64 ciphertext blob unchanged via `--encryption-key.kms-ciphertext` or
`MIDEN_VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT`. The validator recovers the key material at startup with `kms:Decrypt`,
so its AWS identity needs that permission on the wrapping key. Note that, unlike KMS-backed signing, the decrypted
encryption key is held in validator memory: AWS KMS cannot perform X25519 key agreement itself, so envelope encryption
is the supported provisioning path.

The validator stores accepted private inputs under this shared key. Any holder of the shared secret
can decrypt those records. Run each validator inside its trusted execution environment. If
transaction proving uses a remote prover, that prover also receives the plaintext inputs and must
run inside the same trusted boundary.

The sealed-input storage migration cannot recover ciphertext for older validated rows. Rebootstrap
an existing validator database before deploying a build with this schema.

Use `miden-validator start --help` for the complete current option list.
