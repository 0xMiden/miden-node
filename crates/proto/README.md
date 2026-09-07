# Miden node proto

`proto` contains generated protobuf bindings, conversion code, and gRPC error helpers used inside the Miden node
workspace. It is part of the [Miden node](https://github.com/0xMiden/node#readme) repository.

## Role

This crate is an internal implementation crate for the node binaries and component crates. It is not the recommended
crate for external clients that want to generate bindings from the public protobuf API.

For external gRPC client generation, use `proto-build`.

## Canonical objects

This crate reuses the canonical protobuf messages from `miden-objects` `0.17.0-rc.3`. Binding generation uses
`miden_objects::EXTERN_PATHS` to map canonical protobuf packages to their Rust types. Node-owned generated messages
define service requests, responses, submission envelopes, and block proving inputs.

The canonical protobuf migration changes the gRPC wire format and generated bindings. Regenerate clients and deploy
connected components together. Database and backup formats do not change. See the
[migration guidance](../../proto/README.md#canonical-protobuf-migration) for API changes and descriptor usage.

## Notes

This crate does not provide a ready-to-use TLS client for official public RPC endpoints. Client applications should
configure transport security in their generated client stack.

## License

This project is [MIT licensed](../../LICENSE).
