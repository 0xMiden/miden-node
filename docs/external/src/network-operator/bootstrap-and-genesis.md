---
title: "Bootstrap and Genesis"
sidebar_position: 3
---

<!-- markdownlint-disable MD033 MD041 -->

import Tabs from "@theme/Tabs"; import TabItem from "@theme/TabItem";

# Bootstrap and Genesis

A signed genesis block is the trust anchor for every service that joins a network. One of the network's validators is
responsible for creating and signing this block. Its header commits to the full validator set, but only the
bootstrapping validator signs it; the full set must sign every block after genesis. On official networks, the validators
are operated by separate entities from the network operator.

This signed block is subsequently made available for official networks at

```text
https://genesis.<network>.miden.io
```

which provides an easy method to obtain this data. This is directly supported by service bootstrap commands by passing
`--network testnet` or `--network devnet`. Bootstrap commands also support passing a file directly to cover custom
networks, or if the official URLs are not trusted.

## Bootstrap Flow

<Tabs groupId="network-operator-genesis-source" defaultValue="official">
  <TabItem value="official" label="Official network">

The genesis block is the chain's trust root: its header commits to the full validator set, but only the bootstrapping
validator signs it. Every other validator operator first prints their public key and sends it to the bootstrapping
operator:

```bash
miden-validator pubkey --key.kms-id <validator-N-kms-key-id>
```

The full validator set — including the bootstrapping validator's own public key — is part of the genesis configuration,
as a top-level `validators` list in `genesis.toml`. If `validators` is omitted, the set defaults to the bootstrapping
validator's key alone (a single-validator network).

```toml
validators = [
  "<validator-1-public-key-hex>",
  "<validator-2-public-key-hex>",
  "<validator-3-public-key-hex>",
]
```

**One** validator operator then runs the signing form of `bootstrap` with their own KMS key ID:

```bash
miden-validator bootstrap \
  --data-directory validator-1-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --genesis-config-file genesis.toml \
  --key.kms-id <validator-1-kms-key-id>
```

Upload `genesis-data/genesis.dat` so it is served at:

```text
https://genesis.<network>.miden.io
```

Every other validator operator seeds their own database from the same genesis block, without re-signing it:

```bash
miden-validator bootstrap \
  --data-directory validator-2-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --file genesis-data/genesis.dat
```

Initialize the sequencer's node storage from the hosted genesis block:

```bash
miden-node bootstrap \
  --data-directory node-data \
  --network testnet
```

Initialize the network transaction builder from the same hosted genesis block:

```bash
miden-ntx-builder bootstrap \
  --data-directory ntx-builder-data \
  --network testnet
```

For `devnet`, use `--network devnet` instead. The `--network` flag is shorthand for downloading the signed genesis block
from `https://genesis.<network>.miden.io`.

Each validator operator's own KMS key ID must be used when that operator starts their validator for this network.

  </TabItem>
  <TabItem value="unofficial" label="Unofficial network">

**One** validator operator creates and signs the genesis block with their own local key. The genesis header commits to
the full validator set, taken from the top-level `validators` list in `genesis.toml`; the other validators' secret keys
are not needed. Each of the other operators prints their public key with
`miden-validator pubkey --key.hex <validator-N-key-hex>` and sends it to the bootstrapping operator, who lists it in the
genesis configuration alongside their own. If `validators` is omitted, the set defaults to the bootstrapping validator's
key alone (a single-validator network).

```toml
validators = [
  "<validator-1-public-key-hex>",
  "<validator-2-public-key-hex>",
  "<validator-3-public-key-hex>",
]
```

```bash
miden-validator bootstrap \
  --data-directory validator-1-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --genesis-config-file genesis.toml \
  --key.hex <validator-1-key-hex>
```

Distribute `genesis-data/genesis.dat` to the other validator operators, who each seed their own database from it,
without re-signing it:

```bash
miden-validator bootstrap \
  --data-directory validator-2-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --file genesis-data/genesis.dat
```

For unofficial networks or pre-publication testing, distribute the signed genesis block file directly and initialize
services from that file:

```bash
miden-node bootstrap \
  --data-directory node-data \
  --file genesis-data/genesis.dat
```

```bash
miden-ntx-builder bootstrap \
  --data-directory ntx-builder-data \
  --file genesis-data/genesis.dat
```

  </TabItem>
</Tabs>

The validator key used during bootstrap must match the key used when starting the validator for the network.

<!-- markdownlint-enable MD033 MD041 -->
