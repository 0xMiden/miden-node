# Store component

This component persists the chain state in a `sqlite` database. It also stores each block's raw data as a file.

Mekle data structures are kept in-memory and are rebuilt on startup. Other data like account, note and nullifier
information is always read from disk. We will need to revisit this in the future but for now this is performant enough.

## Migrations

We have database migration support in place but don't actively use it yet. There is only the latest schema, and we reset
chain state (aka nuke the existing database) on each release.

Note that the migration logic includes both a schema number _and_ a hash based on the sql schema. These are both checked
on node startup to ensure that any existing database matches the expected schema. If you're seeing database failures on
startup its likely that you created the database _before_ making schema changes resulting in different schema hashes.

### Regenerating the Schema

After modifying SQL migrations in `crates/store/src/db/migrations/`, regenerate the Diesel schema:

```sh
make schema
```

This runs the migrations against an ephemeral SQLite database and generates `crates/store/src/db/schema.rs`.

A patch file (`crates/store/schema.patch`) applies customizations to the generated schema:

- **`BigInt` type mappings**: SQLite `INTEGER` columns map to Diesel's `Integer` (i32) by default, but our code uses `i64` for block numbers, nonces, timestamps, etc. The patch changes these to `BigInt`.
- **`joinable!` macro**: Adds `accounts -> account_codes` relationship for implicit joins.

#### Updating the Patch

When adding new columns that need `i64` in Rust:

1. Run `make schema` to regenerate with the current patch
2. Edit `schema.rs` to change `Integer` to `BigInt` for the new column
3. Regenerate the patch:
   ```sh
   diesel migration run --database-url=/tmp/miden.db --migration-dir=crates/store/src/db/migrations --config-file=crates/store/diesel.toml
   diesel print-schema --database-url=/tmp/miden.db --config-file=crates/store/diesel.toml > /tmp/base.rs
   diff -U6 /tmp/base.rs crates/store/src/db/schema.rs > crates/store/schema.patch
   rm /tmp/miden.db /tmp/base.rs
   ```

## Architecture

The store consists mainly of a gRPC server which answers requests from the RPC and block-producer components, as well as
new block submissions from the block-producer.
