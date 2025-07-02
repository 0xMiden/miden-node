#!/bin/bash

set -euo pipefail

NODE_LOG="node_output.log"
TIME_LOG="time_report.log"
RUN_DURATION=120  # seconds to let node run (optional if tests are fast)

# Start the node
echo "[INFO] Starting node..."
start_time=$(date +%s)

/usr/bin/time -al ./target/release/miden-node bundled start --data-directory data --rpc.url http://0.0.0.0:57291 > "$NODE_LOG" 2>&1 &
NODE_PID=$!

echo "[INFO] Node PID: $NODE_PID"

# Optional: wait for node to become ready (e.g., wait for a TCP port or log message)
sleep 1

# Run the tests and measure the time
echo "[INFO] Running tests..."
cd ../miden-client && make integration-test
echo "[INFO] Tests completed"

# Optional: wait a fixed time instead of waiting for test completion
sleep "$RUN_DURATION"

# Kill the node if still running
if kill -0 "$NODE_PID" 2>/dev/null; then
  echo "[INFO] Stopping node..."
  kill "$NODE_PID"
  wait "$NODE_PID" 2>/dev/null || true
fi

end_time=$(date +%s)
elapsed=$((end_time - start_time))

# Save timing info
echo "Total runtime: ${elapsed}s" > "$TIME_LOG"

echo "[INFO] Done. Logs:"
echo "  Node log: $NODE_LOG"
echo "  Time log: $TIME_LOG"
