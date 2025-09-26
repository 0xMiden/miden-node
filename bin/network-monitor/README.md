# Miden network monitor

A monitor app for a Miden network's infrastructure.

## Installation

The binary can be installed using the project's Makefile:

```bash
make install-network-monitor
```

## Configuration

The monitor application supports configuration through both command-line arguments and environment variables. Command-line arguments take precedence over environment variables.

### Command-line Arguments

```bash
# View all available options
miden-network-monitor --help

# Common usage examples
miden-network-monitor --port 8080 --rpc-url http://localhost:50051
miden-network-monitor --remote-prover-urls http://prover1.com:50052,http://prover2.com:50053
miden-network-monitor --faucet-url http://localhost:8080 --enable-otel
```

**Available Options:**
- `--rpc-url`: RPC service URL (default: `http://localhost:50051`)
- `--remote-prover-urls`: Comma-separated list of remote prover URLs (default: `http://localhost:50052`)
- `--faucet-url`: Faucet service URL for testing (default: `http://localhost:8080`)
- `--port, -p`: Web server port (default: `3000`)
- `--enable-otel`: Enable OpenTelemetry tracing
- `--help, -h`: Show help information
- `--version, -V`: Show version information

### Environment Variables

If command-line arguments are not provided, the application falls back to environment variables:

- `MIDEN_MONITOR_RPC_URL`: RPC service URL
- `MIDEN_MONITOR_REMOTE_PROVER_URLS`: Comma-separated list of remote prover URLs
- `MIDEN_MONITOR_FAUCET_URL`: Faucet service URL for testing
- `MIDEN_MONITOR_PORT`: Web server port
- `MIDEN_MONITOR_ENABLE_OTEL`: Enable OpenTelemetry tracing

## Usage

### Using Command-line Arguments

```bash
# Single remote prover
miden-network-monitor --remote-prover-urls http://localhost:50052

# Multiple remote provers and custom configuration
miden-network-monitor \
  --remote-prover-urls http://localhost:50052,http://localhost:50053,http://localhost:50054 \
  --faucet-url http://localhost:8080 \
  --port 8080 \
  --enable-otel

# Get help
miden-network-monitor --help
```

### Using Environment Variables

```bash
# Single remote prover
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052" miden-network-monitor

# Multiple remote provers and faucet testing
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052,http://localhost:50053,http://localhost:50054" \
MIDEN_MONITOR_FAUCET_URL="http://localhost:8080" \
miden-network-monitor
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

### Faucet Service
- **Service Health**: Faucet service availability and token minting capability
- **PoW Challenge Testing**: Real-time proof-of-work challenge solving and token minting
  - Success rate tracking with successful/failed minting attempts
  - Response time measurement for challenge completion
  - Challenge difficulty monitoring
  - Transaction and note ID tracking from successful mints
  - Automated testing every 30 seconds to verify faucet functionality

## User Interface

The web dashboard provides a clean, responsive interface with the following features:

- **Real-time Updates**: Automatically refreshes service status every 10 seconds
- **Unified Service Cards**: Each service is displayed in a dedicated card that auto-sizes to show all information
- **Combined Prover Information**: Remote prover cards integrate both connectivity status and proof generation test results
- **Faucet Testing Display**: Shows faucet test results with challenge difficulty and minting success metrics
- **Visual Health Indicators**: Color-coded status indicators and clear success/failure metrics
- **Interactive Elements**: Copy-to-clipboard functionality for genesis commitments, transaction IDs, and note IDs
- **Responsive Design**: Optimized for both desktop and mobile viewing

## Future Monitor Items

Planned workflow testing features for future releases:

### Network Transaction Testing
The monitor system will submit actual transactions to the network to perform end-to-end testing of the complete workflow. This test covers transaction creation, submission, processing, and confirmation, providing comprehensive validation of network functionality.

## License
This project is [MIT licensed](../../LICENSE).
