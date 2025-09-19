# Miden network monitor

A monitor app for a Miden network's infrastructure.

## Installation

The binary can be installed using the project's Makefile:

```bash
make install-network-monitor
```

## Configuration

The monitor application uses environment variables for configuration:

- `MIDEN_MONITOR_RPC_URL`: RPC service URL (default: `http://localhost:50051`)
- `MIDEN_MONITOR_REMOTE_PROVER_URLS`: Comma-separated list of remote prover URLs (default: `http://localhost:50052`)
- `MIDEN_MONITOR_PORT`: Web server port (default: `3000`)

## Usage

```bash
# Single remote prover
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052" miden-network-monitor

# Multiple remote provers
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052,http://localhost:50053,http://localhost:50054" miden-network-monitor
```

Once running, the monitor will be available at `http://localhost:3000` (or the configured port).

## Currently Supported Monitor

The monitor application provides real-time status monitoring for the following Miden network components:

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
- **Supported Proof Types**: Types of proofs the prover can generate (Transaction, Block, Batch)
- **Worker Status**:
  - Individual worker addresses and versions
  - Worker health status (HEALTHY/UNHEALTHY/UNKNOWN)
  - Worker count per prover
- **Proof Generation Testing**: Real-time testing of proof generation capabilities
  - Success rate tracking with test/failure counters
  - Response time measurement for proof generation
  - Proof size monitoring (in KB)
  - Automated testing with mock transactions, blocks, or batches based on supported proof type
  - Combined health status that reflects both connectivity and proof generation capability

## User Interface

The web dashboard provides a clean, responsive interface with the following features:

- **Real-time Updates**: Automatically refreshes service status every 10 seconds
- **Unified Service Cards**: Each service is displayed in a dedicated card that auto-sizes to show all information
- **Combined Prover Information**: Remote prover cards integrate both connectivity status and proof generation test results
- **Visual Health Indicators**: Color-coded status indicators and clear success/failure metrics
- **Interactive Elements**: Copy-to-clipboard functionality for genesis commitments
- **Responsive Design**: Optimized for both desktop and mobile viewing

## Future Monitor Items

Planned workflow testing features for future releases:

### Faucet Workflow Testing
The monitor application will test the faucet service by minting tokens from the official faucet. This test verifies that the faucet is operational and can successfully distribute tokens for testing purposes.

### Network Transaction Testing
The monitor system will submit actual transactions to the network to perform end-to-end testing of the complete workflow. This test covers transaction creation, submission, processing, and confirmation, providing comprehensive validation of network functionality.

## License
This project is [MIT licensed](../../LICENSE).
