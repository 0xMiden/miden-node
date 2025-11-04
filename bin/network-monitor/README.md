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
- `--remote-prover-urls`: Comma-separated list of remote prover URLs (default: `http://localhost:50052`)
- `--faucet-url`: Faucet service URL for testing (default: `http://localhost:8080`)
- `--remote-prover-test-interval`: Interval at which to test the remote provers services (default: `2m`)
- `--faucet-test-interval`: Interval at which to test the faucet services (default: `2m`)
- `--status-check-interval`: Interval at which to check the status of the services (default: `3s`)
- `--port, -p`: Web server port (default: `3000`)
- `--enable-otel`: Enable OpenTelemetry tracing
- `--wallet-filepath`: Path where the wallet account will be saved (default: `wallet_account.bin`)
- `--counter-filepath`: Path where the counter program account will be saved (default: `counter_program.bin`)
- `--help, -h`: Show help information
- `--version, -V`: Show version information

### Environment Variables

If command-line arguments are not provided, the application falls back to environment variables:

- `MIDEN_MONITOR_RPC_URL`: RPC service URL
- `MIDEN_MONITOR_REMOTE_PROVER_URLS`: Comma-separated list of remote prover URLs
- `MIDEN_MONITOR_FAUCET_URL`: Faucet service URL for testing
- `MIDEN_MONITOR_REMOTE_PROVER_TEST_INTERVAL`: Interval at which to test the remote provers services
- `MIDEN_MONITOR_FAUCET_TEST_INTERVAL`: Interval at which to test the faucet services
- `MIDEN_MONITOR_STATUS_CHECK_INTERVAL`: Interval at which to check the status of the services
- `MIDEN_MONITOR_PORT`: Web server port
- `MIDEN_MONITOR_ENABLE_OTEL`: Enable OpenTelemetry tracing
- `MIDEN_MONITOR_WALLET_FILEPATH`: Path where the wallet account will be saved
- `MIDEN_MONITOR_COUNTER_FILEPATH`: Path where the counter program account will be saved

## Commands

The monitor application supports one main command:

### Start Monitor

Starts the network monitoring service with the web dashboard. The monitor automatically creates and deploys Miden accounts if they don't already exist.

```bash
# Start with default configuration
miden-network-monitor start

# Start with custom configuration
miden-network-monitor start --port 8080 --rpc-url http://localhost:50051

# Start with custom account file paths
miden-network-monitor start \
  --wallet-filepath my_wallet.bin \
  --counter-filepath my_counter.bin \
  --rpc-url https://testnet.miden.io:443
```

**Automatic Account Management:**
The monitor automatically handles account creation and deployment:
1. Checks if wallet and counter account files exist
2. If files don't exist, creates new accounts:
   - A wallet account with RpoFalcon512 authentication
   - A counter program account with custom MASM script
3. Saves both accounts to the specified files using the Miden AccountFile format
4. Deploys the accounts to the network via RPC
5. The counter program includes authentication logic that only allows the wallet account to increment the counter
6. Both accounts are ready for use in Miden transactions

## Usage

### Using Command-line Arguments

```bash
# Single remote prover
miden-network-monitor start --remote-prover-urls http://localhost:50052

# Multiple remote provers and custom configuration
miden-network-monitor start \
  --remote-prover-urls http://localhost:50052,http://localhost:50053,http://localhost:50054 \
  --faucet-url http://localhost:8080 \
  --remote-prover-test-interval 2m \
  --faucet-test-interval 2m \
  --status-check-interval 3s \
  --port 8080 \
  --wallet-filepath my_wallet.bin \
  --counter-filepath my_counter.bin \
  --enable-otel

# Get help
miden-network-monitor --help
```

### Using Environment Variables

```bash
# Single remote prover
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052" miden-network-monitor start

# Multiple remote provers and faucet testing
MIDEN_MONITOR_REMOTE_PROVER_URLS="http://localhost:50052,http://localhost:50053,http://localhost:50054" \
MIDEN_MONITOR_FAUCET_URL="http://localhost:8080" \
MIDEN_MONITOR_WALLET_FILEPATH="my_wallet.bin" \
MIDEN_MONITOR_COUNTER_FILEPATH="my_counter.bin" \
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

## Account Management

The monitor automatically manages Miden accounts for testing and monitoring purposes:

### Created Accounts

**Wallet Account:**
- Uses RpoFalcon512 authentication scheme
- Contains authentication keys for transaction signing
- Automatically created if not present

**Counter Program Account:**
- Implements a simple counter with increment functionality
- Includes authentication logic that restricts access to the wallet account
- Uses custom MASM script with account ID-based authorization
- Automatically created if not present

### Account File Management

The monitor automatically:
1. Checks for existing account files on startup
2. Creates new accounts if files don't exist
3. Deploys accounts to the network via RPC
4. Uses the specified file paths (default: `wallet_account.bin` and `counter_program.bin`)

### Example Usage

```bash
# Start monitor with default account files
miden-network-monitor start --rpc-url https://testnet.miden.io:443

# Start monitor with custom account file paths
miden-network-monitor start \
  --rpc-url https://testnet.miden.io:443 \
  --wallet-filepath my_wallet.bin \
  --counter-filepath my_counter.bin

# The generated files can be loaded in Miden applications:
# - wallet_account.bin: Contains the wallet account with authentication keys
# - counter_program.bin: Contains the counter program account
```

## Future Monitor Items

Planned workflow testing features for future releases:

### Network Transaction Testing
The monitor system will submit actual transactions to the network to perform end-to-end testing of the complete workflow. This test covers transaction creation, submission, processing, and confirmation, providing comprehensive validation of network functionality.

## License
This project is [MIT licensed](../../LICENSE).
