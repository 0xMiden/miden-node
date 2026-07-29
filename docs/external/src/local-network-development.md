---
title: "Local Network Development"
sidebar_position: 1
---

# Local Network Development

Use this guide to start a disposable Miden network for local development and testing. The provided Docker Compose setup
includes a sequencer, three validators, a transaction prover, a network transaction builder, and optional block
explorer, monitoring, and trace services, so you can develop against a working environment without wiring the network
services manually.

The Compose model lives in `docker-compose.yml` and uses profiles for optional explorer, telemetry, and monitoring
services. The guide uses `make` targets as shorthand for the underlying Docker image builds and Docker Compose commands;
check the `Makefile` when you need the exact command.

This is not a production deployment guide and it is not the path for independent full node runners on an existing
network.

## Prerequisites

- Git
- Docker with Docker Compose support
- `make`

## Check Out a Version

Prefer a release tag when testing against released artifacts. Use a branch when developing against the current
repository state.

```bash
git clone https://github.com/0xMiden/node.git
cd node
git checkout <release-tag-or-branch>
```

## Run a Published Version

New releases are also published as Compose applications in the GitHub container registry. With Docker Compose 2.34.0 or
later, start the core local network directly from the release artifact:

```bash
RELEASE_TAG=vX.Y.Z
COMPOSE_APPLICATION=oci://ghcr.io/0xmiden/miden-local-network:${RELEASE_TAG}

docker compose -f "${COMPOSE_APPLICATION}" up -d
docker compose -f "${COMPOSE_APPLICATION}" logs -f
docker compose -f "${COMPOSE_APPLICATION}" down -v
```

The application includes an OpenTelemetry Collector that receives traces from the Miden services. Enable the optional
Midenscan explorer, Tempo, Grafana, and network monitor services with Compose profiles:

```bash
docker compose \
  -f "${COMPOSE_APPLICATION}" \
  --profile explorer \
  --profile telemetry \
  --profile monitor \
  up -d
```

When the telemetry profile is enabled, the collector forwards traces to Tempo. To send a copy to another OTLP/gRPC
endpoint, set `OTEL_EXPORTER_OTLP_ENDPOINT`; this works with or without the telemetry profile:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://collector.example.com:4317 \
docker compose -f "${COMPOSE_APPLICATION}" up -d
```

The genesis configuration can be replaced with the same Compose override used for a repository checkout.

## Local Network Commands

Build the images after checkout or whenever you need fresh local images. The Makefile targets enable the `telemetry` and
`monitor` profiles. The local network stores data in the `node-data` Docker volume; `local-network-down` keeps that
data, while `local-network-delete` removes it.

```bash
# Build the Docker images used by the local network.
make local-network-build

# Optionally build for a specific Docker platform.
make local-network-build DOCKER_PLATFORM=linux/arm64

# Start the local network.
make local-network-up

# Follow container logs.
#
# Logs are useful for startup checks; use Tempo traces for request-level debugging.
make local-network-logs

# Stop the local network, preserving the local chain data volume.
make local-network-down

# Stop the local network and delete the local chain data volume.
make local-network-delete
```

After `make local-network-delete`, run `make local-network-up` to bootstrap a fresh local chain.

## Exposed Endpoints

Published ports are bound to `localhost`; the following services are available:

| Service          | URL                             | Purpose                                          |
| ---------------- | ------------------------------- | ------------------------------------------------ |
| RPC API          | `http://localhost:57291`        | Submit transactions and query local chain state. |
| Grafana          | `http://localhost:3000`         | Inspect dashboards and traces.                   |
| Network monitor  | `http://localhost:3001`         | View local network health.                       |
| Block explorer   | `http://localhost:8080`         | Browse locally indexed network data.             |
| Explorer GraphQL | `http://localhost:8199/graphql` | Query the explorer backend.                      |
| Tempo HTTP API   | `http://localhost:3200`         | Query stored trace data.                         |
| Tempo OTLP gRPC  | `http://localhost:4317`         | Receive OpenTelemetry traces from services.      |

## Block Explorer

Enable the `explorer` profile to run the Gateway FM Midenscan frontend, backend, indexer, database, and database
migration:

```bash
docker compose --profile explorer up -d
```

The indexer reads from the local sequencer and persists its state in the `explorer-data` volume. The frontend is
available at `http://localhost:8080`, with its GraphQL API at `http://localhost:8199/graphql`. These third-party
components are intended for local development and are not part of the Miden node implementation.

## Monitoring and Traces

The bundled OpenTelemetry Collector receives traces from the local network. With the telemetry profile enabled, it
forwards traces to Tempo. Grafana is preconfigured with Tempo as a data source, so use `http://localhost:3000` to
inspect traces when a request fails, stalls, or behaves differently than expected.

Container logs are still useful for startup failures and quick checks, but traces usually provide a better view of how a
request moved through the local network.

The network monitor at `http://localhost:3001` provides a compact health view for the running local network.

## Prover Override

The default stack spins up an internal prover instance which means proving will happen locally. This can be overridden
to use an external prover by setting `MIDEN_REMOTE_PROVER_URL` when starting the stack. The URL must be reachable from
inside the Compose network.

```bash
MIDEN_REMOTE_PROVER_URL=http://<prover-host>:50051 make local-network-up
```

## Genesis Config Override

By default, the local network bootstraps from the bundled `genesis` Compose config in `compose/bootstrap.yml`. It
contains the public signing keys for the three validator services. Their corresponding private keys are insecure
defaults defined in `compose/validator.yml` and must never be used outside local development.

To replace it, create a Compose override file:

```yaml title="genesis.override.yml"
configs:
  genesis: !override
    file: /absolute/path/to/genesis.toml
```

Use that override with either the repository model or a published application:

```bash
make local-network-up COMPOSE_OVERRIDE_FILE=/absolute/path/to/genesis.override.yml

docker compose \
  -f oci://ghcr.io/0xmiden/miden-local-network:vX.Y.Z \
  -f /absolute/path/to/genesis.override.yml \
  up -d
```

The custom configuration is mounted into the bootstrap validator as `/genesis.toml` and passed to
`miden-validator genesis --config`. Its `validators` list must contain the public keys corresponding to the three
validator private keys. Override those private keys with `MIDEN_VALIDATOR_1_SIGNING_KEY`,
`MIDEN_VALIDATOR_2_SIGNING_KEY`, and `MIDEN_VALIDATOR_3_SIGNING_KEY`.

This only affects validator bootstrap. If the local network has already been bootstrapped, delete the existing local
chain data before starting with a different genesis configuration:

```bash
make local-network-delete
```

## Check the RPC API

The RPC server exposes gRPC reflection. With `grpcurl` installed, a basic status check looks like:

```bash
grpcurl -plaintext localhost:57291 rpc.Api/Status
```

Note the `-plaintext` flag, the local network does not use TLS.

Use the [gRPC API](./rpc/) section for the public RPC surface and streaming endpoints.
