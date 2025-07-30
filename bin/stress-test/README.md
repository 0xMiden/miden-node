# Miden stress test

This crate contains a binary for running Miden node stress tests.

## Seed Store

This command seeds a store with newly generated accounts. For each block, it first creates a faucet transaction that sends assets to multiple accounts by emitting notes, then adds transactions that consume these notes for each new account. As a result, the seeded store files are placed in the given data directory, including a dump file with all the newly created accounts ids.

Once it's finished, it prints out several metrics.

After building the binary, you can run the following command to generate one million accounts:

`miden-node-stress-test seed-store --data-directory ./data --num-accounts 1000000`

The store file will then be located at `./data/miden-store.sqlite3`.

## Benchmark Store

This command allows to run stress tests against the Store component. These tests use the dump file with accounts ids created when seeding the store, so be sure to run the `seed-store` command beforehand.

The endpoints that you can test are:
- `sync_state`
- `sync_notes`
- `check_nullifiers_by_prefix`

Each benchmark accepts options to control the number of iterations and concurrency level.

**Note on Concurrency**: The concurrency parameter controls how many requests are sent in parallel to the store. Since these benchmarks run against a local store (no network overhead), higher concurrency values can help identify bottlenecks in the store's internal processing. The latency measurements exclude network time and represent pure store processing time.

Example usage:

```bash
miden-node-stress-test benchmark-store \
  --data-directory ./data \
  --iterations 10000 \
  --concurrency 16 \
  sync-notes
```

### Results

Using the store seed command:
```bash
# Using 1M accounts, half are public
$ miden-node-stress-test seed-store --data-directory data --num-accounts 100000 --public-accounts-percentage 50

Total time: 235.452 seconds
Inserted 393 blocks with avg insertion time 212 ms
Initial DB size: 120.1 KB
Average DB growth rate: 325.3 KB per block
```

#### Block metrics

> Note: Each block contains 256 transactions (16 batches * 16 transactions).

| Block  | Insert Time (ms)   |  Get Block Inputs Time (ms)   |  Get Batch Inputs Time (ms)    | Block Size (KB)    |  DB Size (MB) |
| ------ | ------------------ | ----------------------------- | ------------------------------ | ------------------ | ------------- |
| 0      | 22                 | 1                             | 0                              | 375.6              | 0.3           |
| 50     | 186                | 9                             | 1                              | 473.6              | 22.2          |
| 100    | 199                | 10                            | 1                              | 473.6              | 40.7          |
| 150    | 219                | 10                            | 1                              | 473.6              | 58.1          |
| 200    | 218                | 11                            | 1                              | 473.6              | 74.8          |
| 250    | 222                | 11                            | 1                              | 473.6              | 91.6          |
| 300    | 228                | 12                            | 1                              | 473.6              | 108.1         |
| 350    | 232                | 13                            | 1                              | 473.6              | 124.4         |

#### Database stats

> Note: Database contains 100215 accounts and 100215 notes across all blocks.

| Table                              | Size (MB)       | KB/Entry   |
| ---------------------------------- | --------------- | ---------- |
| accounts                           | 26.1            | 0.3        |
| account_deltas                     | 1.2             | 0.0        |
| account_fungible_asset_deltas      | 2.2             | 0.0        |
| account_non_fungible_asset_updates | 0.0             | -          |
| account_storage_map_updates        | 0.0             | -          |
| account_storage_slot_updates       | 3.1             | 0.1        |
| block_headers                      | 0.1             | 0.3        |
| notes                              | 49.1            | 0.5        |
| note_scripts                       | 0.0             | 8.0        |
| nullifiers                         | 4.6             | 0.0        |
| transactions                       | 6.0             | 0.1        |

Current results of the store stress-tests:

**Performance Note**: The latency measurements below represent pure store processing time (no network overhead).

- sync-state
``` bash
$ miden-node-stress-test benchmark-store --data-directory ./data --iterations 10000 --concurrency 16 sync-state

Average request latency: 68.031821ms
P50 request latency: 1.027125ms
P95 request latency: 1.615ms
P99 request latency: 2.838417ms
P99.9 request latency: 41.842534834s
Average notes per response: 1.323
```

- sync-notes
``` bash
$ miden-node-stress-test benchmark-store --data-directory ./data --iterations 10000 --concurrency 16 sync-notes

Average request latency: 67.092947ms
P50 request latency: 627.5µs
P95 request latency: 1.106583ms
P99 request latency: 1.54125ms
P99.9 request latency: 41.511792833s
```

- check-nullifiers-by-prefix
``` bash
$ miden-node-stress-test benchmark-store --data-directory ./data --iterations 10000 --concurrency 16 check-nullifiers-by-prefix --prefixes 10

Average request latency: 526.395µs
P50 request latency: 509.458µs
P95 request latency: 762.583µs
P99 request latency: 891.75µs
P99.9 request latency: 2.265ms
Average nullifiers per response: 24.6875
```

## License
This project is [MIT licensed](../../LICENSE).
