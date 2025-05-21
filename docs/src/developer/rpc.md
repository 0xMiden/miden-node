# RPC Component

This is by far the simplest component. Essentially this is a thin gRPC server which proxies all requests to the store
and block-producer components.

Its main function is to pre-validate all requests before sending them on. This means malformed or non-sensical requests
get rejected _before_ reaching the store and block-producer, reducing their load. Notably this also includes verifying
the proofs of submitting transactions. This allows the block-producer to skip proof verification (it trusts the RPC
component), reducing the load in this critical component.

## RPC Versioning

The RPC server enforces version requirements against connecting clients. This is done via the HTTP ACCEPT header. Every client is expected to provide this header with a value which follows this format: `application/vnd.miden.v0.9.0+grpc`.

If there is a mismatch in version, clients will encounter an error while executing gRPC requests against the RPC server with the following details:

- gRPC status code: 3 (Invalid Argument)
- gRPC message: Missing required ACCEPT header

The server will reject any version that does not have the same major and minor version to it. This behaviour will change after v1.0.0., at which point only the major version will be taken into account.
