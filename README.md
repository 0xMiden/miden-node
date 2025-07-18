# Miden node

[![LICENSE](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/0xMiden/miden-node/blob/main/LICENSE)
[![test](https://github.com/0xMiden/miden-node/actions/workflows/test.yml/badge.svg)](https://github.com/0xMiden/miden-node/actions/workflows/test.yml)
[![RUST_VERSION](https://img.shields.io/badge/rustc-1.88+-lightgray.svg)](https://www.rust-lang.org/tools/install)
[![crates.io](https://img.shields.io/crates/v/miden-node)](https://crates.io/crates/miden-node)

Welcome to the Miden node implementation :) This software is used to operate a Miden ZK-rollup network by
receiving transactions and sequencing them into blocks.

Access to the network is provided via a gRPC interface which can be found [here](./proto/README.md).

> [!NOTE]
> The Miden node is still under heavy development and the project can be considered to be in an _alpha_ stage.
> Many features are yet to be implemented and there are a number of limitations which we will lift in the near future.
>
> At this point we are developing the Miden node for a centralized operator. As such, the work does not yet include
> components such as P2P networking and consensus. These will be added in the future.

## Documentation

Documentation, tutorials and guides for the current Miden version (aka testnet) can be found
[here](https://0xMiden.github.io/miden-docs/), including an operator manual and gRPC reference guide. This is
your one-stop-shop for all things Miden.

For node operators living on the development edge, we also host the latest unreleased documentation
[here](https://0xMiden.github.io/miden-node/index.html).

## Contributing

Developer documentation and onboarding guide is available
[here](https://0xMiden.github.io/miden-node/developer/index.html).

At minimum, please see our [contributing](CONTRIBUTING.md) guidelines and our [makefile](Makefile) for example workflows
e.g. run the testsuite using

```sh
make test
```

Note that we do _not_ accept low-effort contributions or AI generated code. For typos and documentation errors please
rather open an issue.

## License

This project is [MIT licensed](./LICENSE).
