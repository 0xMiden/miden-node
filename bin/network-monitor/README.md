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
miden-network-monitor start --port 8080 --rpc-url http://localhost:50051
miden-network-monitor start --remote-prover-urls http://prover1.com:50052,http://prover2.com:50053
miden-network-monitor start --faucet-url http://localhost:8080 --enable-otel
```

**Available Options:**
- `--rpc-url`: RPC service URL (default: `http://localhost:50051`)
- `--remote-prover-urls`: Comma-separated list of remote prover URLs. If omitted or empty, prover tasks are disabled.
- `--faucet-url`: Faucet service URL for testing. If omitted, faucet testing is disabled.
- `--enable-counter`: Enable the counter increment task. If set, the monitor ensures the counter account exist (creating and deploying them if missing).
- `--remote-prover-test-interval`: Interval at which to test the remote provers services (default: `2m`)
- `--faucet-test-interval`: Interval at which to test the faucet services (default: `2m`)
- `--status-check-interval`: Interval at which to check the status of the services (default: `3s`)
- `--port, -p`: Web server port (default: `3000`)
- `--enable-otel`: Enable OpenTelemetry tracing
- `--counter-file`: Path where the counter program account will be saved (default: `counter_program.bin`) — used only when `--enable-counter` is set
- `--help, -h`: Show help information
- `--version, -V`: Show version information

### Environment Variables

If command-line arguments are not provided, the application falls back to environment variables:

- `MIDEN_MONITOR_RPC_URL`: RPC service URL
- `MIDEN_MONITOR_REMOTE_PROVER_URLS`: Comma-separated list of remote prover URLs. If unset or empty, prover tasks are disabled.
- `MIDEN_MONITOR_FAUCET_URL`: Faucet service URL for testing. If unset, faucet testing is disabled.
- `MIDEN_MONITOR_ENABLE_COUNTER`: Set to `true` to enable the counter increment task (default: disabled)
- `MIDEN_MONITOR_REMOTE_PROVER_TEST_INTERVAL`: Interval at which to test the remote provers services
- `MIDEN_MONITOR_FAUCET_TEST_INTERVAL`: Interval at which to test the faucet services
- `MIDEN_MONITOR_STATUS_CHECK_INTERVAL`: Interval at which to check the status of the services
- `MIDEN_MONITOR_PORT`: Web server port
- `MIDEN_MONITOR_ENABLE_OTEL`: Enable OpenTelemetry tracing
- `MIDEN_MONITOR_COUNTER_FILE`: Path where the counter program account will be saved

## Commands

The monitor application supports one main command:

### Start Monitor

Starts the network monitoring service with the web dashboard. RPC status is always enabled. Other tasks are optional and spawn only when configured:

- Prover checks/tests: enabled when `--remote-prover-urls` (or `MIDEN_MONITOR_REMOTE_PROVER_URLS`) is provided
- Faucet testing: enabled when `--faucet-url` (or `MIDEN_MONITOR_FAUCET_URL`) is provided
- Counter increment: enabled when `--enable-counter` (or `MIDEN_MONITOR_ENABLE_COUNTER=true`) is set

```bash
# Start with default configuration (RPC only)
miden-network-monitor start

# Start with custom configuration
miden-network-monitor start --port 8080 --rpc-url http://localhost:50051

# Enable counter task with custom account file paths
miden-network-monitor start \
  --enable-counter \
  --counter-file my_counter.bin \
  --rpc-url https://testnet.miden.io:443
```

**Optional Counter Account Management (only when counter is enabled):**
When `--enable-counter` is set, the monitor ensures required counter account exist before starting the task:
1. If file is missing, creates new counter account:
   - Counter program account with the increment procedure
2. Saves counter account to the specified file using the Miden AccountFile format
3. Deploys accounts to the network via RPC (if not already deployed)
4. The counter contract authorizes increments only from the counter account

## Usage

### Using Command-line Arguments

```bash
# Single remote prover
miden-network-monitor start --remote-prover-urls http://localhost:50052

# Multiple remote provers and custom configuration
miden-network-monitor start \
  --remote-prover-urls http://localhost:50052,http://localhost:50053,http://localhost:50054 \
  --faucet-url http://localhost:8080 \
  --enable-counter \
  --remote-prover-test-interval 2m \
  --faucet-test-interval 2m \
  --status-check-interval 3s \
  --port 8080 \
  --counter-file my_counter.bin \
  --enable-otel

# Get help
miden-network-monitor --help
```

### Using Environment Variables

```bash
# Single remote prover
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052" miden-network-monitor start

# Multiple remote provers, faucet testing, and counter task
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052,http://localhost:50053,http://localhost:50054" \
MIDEN_MONITOR_FAUCET_URL="http://localhost:8080" \
MIDEN_MONITOR_ENABLE_COUNTER=true \
MIDEN_MONITOR_COUNTER_FILE="my_counter.bin" \
miden-network-monitor start
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
  - Automated testing on a configurable interval to verify faucet functionality

### Counter Increment
- **Service Health**: End-to-end transaction submission and on-chain state query
- **Metrics**:
  - Current counter value (queried from RPC one block after submission)
  - Success/Failure counts
  - Last TX ID with copy-to-clipboard

## User Interface

The web dashboard provides a clean, responsive interface with the following features:

- **Real-time Updates**: Automatically refreshes service status every 10 seconds
- **Unified Service Cards**: Each service is displayed in a dedicated card that auto-sizes to show all information
- **Combined Prover Information**: Remote prover cards integrate both connectivity status and proof generation test results
- **Faucet Testing Display**: Shows faucet test results with challenge difficulty and minting success metrics
- **Visual Health Indicators**: Color-coded status indicators and clear success/failure metrics
- **Interactive Elements**: Copy-to-clipboard functionality for genesis commitments, transaction IDs, and note IDs
- **Responsive Design**: Optimized for both desktop and mobile viewing

## Account Management

When the counter task is enabled, the monitor manages the necessary Miden accounts:

### Created Accounts

**Counter Program Account:**
- Implements a simple counter with increment functionality
- Includes authentication logic that restricts access to the counter account
- Uses custom MASM script with account ID-based authorization
- Automatically created if not present

**Wallet Account:**
- Uses RpoFalcon512 authentication scheme
- Contains authentication keys for transaction signing
- Automatically created if not present

### Account File Management

The monitor automatically:
1. Checks for existing account files on startup
2. Creates new accounts if files don't exist
3. Deploys accounts to the network via RPC
4. Uses the specified file paths (default: `counter_program.bin`)

### Example Usage

```bash
# Start monitor with counter task and default account files
miden-network-monitor start --enable-counter --rpc-url https://testnet.miden.io:443

# Start monitor with custom account file paths
miden-network-monitor start \
  --enable-counter \
  --rpc-url https://testnet.miden.io:443 \
  --counter-file my_counter.bin
```

## Future Monitor Items

Planned features:

- Note synchronization views (by tag) with inclusion proofs
- Account state proof verification (client-side) and historical state queries
- Batch prover status and test coverage
- Configurable alerting/webhooks for unhealthy services

## License
This project is [MIT licensed](../../LICENSE).
