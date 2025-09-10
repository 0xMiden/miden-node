# Miden network monitoring

A monitoring app for a Miden network's infrastructure.

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

## Currently Supported Monitoring

The monitoring application provides real-time status monitoring for the following Miden network components:

### RPC Service
- **Service Health**: Overall RPC service availability and status
- **Version Information**: RPC service version
- **Genesis Commitment**: Network genesis commitment (with copy-to-clipboard functionality)
- **Store Status**:
  - Store service version and health
  - Chain tip (latest block number)
- **Block Producer Status**:
  - Block producer version and health

### Remote Provers
- **Service Health**: Individual remote prover availability and status
- **Version Information**: Remote prover service version
- **Supported Proof Types**: Types of proofs the prover can generate
- **Worker Status**:
  - Individual worker addresses and versions
  - Worker health status (HEALTHY/UNHEALTHY/UNKNOWN)
  - Worker count per prover

## Future Monitoring Items

Planned workflow testing features for future releases:

### Faucet Workflow Testing
The monitoring application will test the faucet service by minting tokens from the official faucet. This test verifies that the faucet is operational and can successfully distribute tokens for testing purposes.

### Prover Workflow Testing
The application will use the delegated prover to prove transactions, testing the proving infrastructure without requiring network submission. This includes testing with static transactions to validate proof generation capabilities and monitor prover performance under various loads.

### Network Transaction Testing
The monitoring system will submit actual transactions to the network to perform end-to-end testing of the complete workflow. This test covers transaction creation, submission, processing, and confirmation, providing comprehensive validation of network functionality.

## License
This project is [MIT licensed](../../LICENSE).
