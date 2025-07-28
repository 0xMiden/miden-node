# Miden stress test

This crate contains a binary for running Miden node stress tests.

## Seed Store

This command seeds a store with newly generated accounts. For each block, it first creates a faucet transaction that sends assets to multiple accounts by emitting notes, then adds transactions that consume these notes for each new account. As a result, the seeded store files are placed in the given data directory, including a dump file with all the newly created accounts ids.

Once it's finished, it prints out several metrics.

After building the binary, you can run the following command to generate one million accounts:

`miden-node-stress-test seed-store --data-directory ./data --num-accounts 1000000`

The store file will then be located at `./data/miden-store.sqlite3`.

## Benchmark Store

This command allows to run stress tests against the Store component. This tests use the dump file with accounts ids created when seeding the store, so be sure to run the `seed-store` command beforehand.

The endpoints that you can test are:
- `sync_state`
- `sync_notes`
- `check_nullifiers_by_prefix`

Each benchmark accepts options to control the number of iterations and concurrency level.

Example usage:

```bash
miden-node-stress-test benchmark-store \
  --data-directory ./data \
  --iterations 10000 \
  --concurrency 16 \
  sync-notes
```

### Results

Using the current store seed command:
```bash
# Using 1M accounts, half are public
miden-node-stress-test seed-store --data-directory data --num-accounts 100000 --public-accounts-percentage 50
```
Current results of the store stress-tests:

- sync-state
``` bash
$ miden-node-stress-test benchmark-store --data-directory ./data --iterations 10000 --concurrency 16 sync-state

Average request latency: 77.386421ms
P95 request latency: 29.490042ms
Average notes per response: 1.4048
```

- sync-notes
``` bash
$ miden-node-stress-test benchmark-store --data-directory ./data --iterations 10000 --concurrency 16 sync-notes

Average request latency: 77.229469ms
P95 request latency: 27.882625ms
```

- check-nullifiers-by-prefix
``` bash
$ miden-node-stress-test benchmark-store --data-directory ./data --iterations 10000 --concurrency 16 check-nullifiers-by-prefix --prefixes 10

Average request latency: 1.727873ms
P95 request latency: 2.615959ms
Average nullifiers per response: 25.1951
```

## License
This project is [MIT licensed](../../LICENSE).
