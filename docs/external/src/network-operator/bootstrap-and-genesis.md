---
title: "Bootstrap and Genesis"
sidebar_position: 3
---

<!-- markdownlint-disable MD033 MD041 -->

import Tabs from "@theme/Tabs"; import TabItem from "@theme/TabItem";

# Bootstrap and Genesis

The genesis block is the trust anchor for every service that joins a network. It is not signed: it simply commits to the
full validator set in its header, and that set must sign every block after genesis. Because nothing signs the genesis
block, it must always be obtained from a trusted source. One of the network's operators is responsible for building it
from the genesis configuration. On official networks, the validators are operated by separate entities from the network
operator.

The genesis block is subsequently made available for official networks at

```text
https://genesis.<network>.miden.io
```

which provides an easy method to obtain this data. This is directly supported by service bootstrap commands by passing
`--network testnet` or `--network devnet`. Bootstrap commands also support passing a file directly to cover custom
networks, or if the official URLs are not trusted.

## Bootstrap Flow

<Tabs groupId="network-operator-genesis-source" defaultValue="official">
  <TabItem value="official" label="Official network">

The genesis block is the chain's trust root: its header commits to the full validator set, which must sign every block
after genesis. Each validator operator first prints their public key and sends it to the bootstrapping operator:

```bash
miden-validator pubkey --signing-key.kms-id <validator-N-kms-key-id>
```

The full validator set is part of the genesis configuration, as a top-level `validators` list in `genesis.toml`. If
`validators` is omitted, the set defaults to the public key of the predefined, insecure development signing key —
production networks must always list their validators explicitly.

```toml
validators = [
  "<validator-1-public-key-hex>",
  "<validator-2-public-key-hex>",
  "<validator-3-public-key-hex>",
]
```

**One** operator then runs `genesis` with the genesis configuration. Building the genesis block requires no signing key:

```bash
miden-validator genesis \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --config genesis.toml
```

Upload `genesis-data/genesis.dat` so it is served at:

```text
https://genesis.<network>.miden.io
```

Every validator operator — including the one that built the genesis block — seeds their own database from the genesis
block:

```bash
miden-validator bootstrap \
  --data-directory validator-1-data \
  --genesis genesis-data/genesis.dat
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

For `devnet`, use `--network devnet` instead. The `--network` flag is shorthand for downloading the genesis block from
`https://genesis.<network>.miden.io`.

Each validator operator's own KMS key ID must be used when that operator starts their validator for this network.

  </TabItem>
  <TabItem value="unofficial" label="Unofficial network">

**One** operator builds the genesis block; no signing key is needed. The genesis header commits to the full validator
set, taken from the top-level `validators` list in `genesis.toml`. Each validator operator prints their public key with
`miden-validator pubkey --signing-key.hex <validator-N-key-hex>` and sends it to the bootstrapping operator, who lists
it in the genesis configuration. If `validators` is omitted, the set defaults to the public key of the predefined,
insecure development signing key — anything but local development must list the validators explicitly.

```toml
validators = [
  "<validator-1-public-key-hex>",
  "<validator-2-public-key-hex>",
  "<validator-3-public-key-hex>",
]
```

```bash
miden-validator genesis \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --config genesis.toml
```

Distribute `genesis-data/genesis.dat` to the validator operators, who each seed their own database from it — including
the operator who built the genesis block:

```bash
miden-validator bootstrap \
  --data-directory validator-1-data \
  --genesis genesis-data/genesis.dat
```

For unofficial networks or pre-publication testing, distribute the genesis block file directly and initialize services
from that file:

```bash
miden-node bootstrap \
  --data-directory node-data \
  --genesis genesis-data/genesis.dat
```

```bash
miden-ntx-builder bootstrap \
  --data-directory ntx-builder-data \
  --genesis genesis-data/genesis.dat
```

  </TabItem>
</Tabs>

The key each validator operator starts their validator with must match the public key committed for them in the genesis
configuration's `validators` list.

<!-- markdownlint-enable MD033 MD041 -->
