# Miden network monitoring

This crate contains a binary for running a Miden network monitor that can monitor multiple remote provers.

## Installation

The binary can be installed using the project's Makefile:

```bash
make install-network-monitoring
```

## Configuration

The monitoring application uses environment variables for configuration:

- `MIDEN_MONITORING_RPC_URL`: RPC service URL (default: `http://localhost:50051`)
- `MIDEN_MONITORING_REMOTE_PROVER_URLS`: Comma-separated list of remote prover URLs (default: `http://localhost:50052`)
- `MIDEN_MONITORING_PORT`: Web server port (default: `3000`)

## Usage

```bash
# Single remote prover
MIDEN_MONITORING_REMOTE_PROVER_URLS="http://localhost:50052" miden-network-monitoring

# Multiple remote provers
MIDEN_MONITORING_REMOTE_PROVER_URLS="http://localhost:50052,http://localhost:50053,http://localhost:50054" miden-network-monitoring
```

## License
This project is [MIT licensed](../../LICENSE).
