.DEFAULT_GOAL := help

.PHONY: help
help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

# -- variables ------------------------------------------------------------------------------------

WARNINGS=RUSTDOCFLAGS="-D warnings"
BUILD_PROTO=BUILD_PROTO=1

# -- linting --------------------------------------------------------------------------------------

.PHONY: clippy
clippy: ## Runs Clippy with configs
	cargo clippy --locked --all-targets --all-features --workspace --exclude miden-remote-prover -- -D warnings # miden-tx async feature on.
	cargo clippy --locked --all-targets --all-features -p miden-remote-prover -- -D warnings # miden-tx async feature off.


.PHONY: fix
fix: ## Runs Fix with configs
	cargo fix --allow-staged --allow-dirty --all-targets --all-features --workspace --exclude miden-remote-prover # miden-tx async feature on.
	cargo fix --allow-staged --allow-dirty --all-targets --all-features -p miden-remote-prover # miden-tx async feature off.


.PHONY: format
format: ## Runs Format using nightly toolchain
	cargo +nightly fmt --all


.PHONY: format-check
format-check: ## Runs Format using nightly toolchain but only in check mode
	cargo +nightly fmt --all --check


.PHONY: machete
toml: ## Runs machete to find unused dependencies
	cargo machete


.PHONY: toml
toml: ## Runs Format for all TOML files
	taplo fmt


.PHONY: toml-check
toml-check: ## Runs Format for all TOML files but only in check mode
	taplo fmt --check --verbose

.PHONY: typos-check
typos-check: ## Runs spellchecker
	typos

.PHONY: workspace-check
workspace-check: ## Runs a check that all packages have `lints.workspace = true`
	cargo workspace-lints


.PHONY: lint
lint: typos-check format fix clippy toml workspace-check machete ## Runs all linting tasks at once (Clippy, fixing, formatting, workspace, machete)

# --- docs ----------------------------------------------------------------------------------------

.PHONY: doc
doc: ## Generates & checks documentation
	$(WARNINGS) cargo doc --all-features --keep-going --release --locked

.PHONY: book
book: ## Builds the book & serves documentation site
	mdbook serve --open docs

# --- testing -------------------------------------------------------------------------------------

.PHONY: test
test:  ## Runs all tests
	cargo nextest run --all-features --workspace --exclude miden-remote-prover # miden-tx async feature on.
	cargo nextest run --all-features -p miden-remote-prover # miden-tx async feature off.

# --- checking ------------------------------------------------------------------------------------

.PHONY: check
check: ## Check all targets and features for errors without code generation
	${BUILD_PROTO} cargo check --all-features --all-targets --locked --workspace --exclude miden-remote-prover # miden-tx async feature on.
	${BUILD_PROTO} cargo check --all-features --all-targets --locked -p miden-remote-prover  # miden-tx async feature off

# --- building ------------------------------------------------------------------------------------

.PHONY: build
build: ## Builds all crates and re-builds ptotobuf bindings for proto crates
	${BUILD_PROTO} cargo build --locked --workspace --exclude miden-remote-prover # miden-tx async feature on.
	${BUILD_PROTO} cargo build --locked -p miden-remote-prover  # miden-tx async feature off
	${BUILD_PROTO} cargo build --locked -p miden-remote-prover-client --target wasm32-unknown-unknown --no-default-features  # no-std compatible build

# --- installing ----------------------------------------------------------------------------------

.PHONY: install-node
install-node: ## Installs node
	${BUILD_PROTO} cargo install --path bin/node --locked

.PHONY: install-faucet
install-faucet: ## Installs faucet
	${BUILD_PROTO} cargo install --path bin/faucet --locked

.PHONY: install-remote-prover
install-remote-prover: ## Install remote prover's CLI
	$(BUILD_PROTO) cargo install --path bin/remote-prover --bin miden-remote-prover --features concurrent

.PHONY: install-stress-test
install-stress-test: ## Installs stress-test binary
	cargo install --path bin/stress-test --locked

# --- docker --------------------------------------------------------------------------------------

.PHONY: docker-build-node
docker-build-node: ## Builds the Miden node using Docker
	@CREATED=$$(date) && \
	VERSION=$$(cat bin/node/Cargo.toml | grep -m 1 '^version' | cut -d '"' -f 2) && \
	COMMIT=$$(git rev-parse HEAD) && \
	docker build --build-arg CREATED="$$CREATED" \
        		 --build-arg VERSION="$$VERSION" \
          		 --build-arg COMMIT="$$COMMIT" \
                 -f bin/node/Dockerfile \
                 -t miden-node-image .

.PHONY: docker-run-node
docker-run-node: ## Runs the Miden node as a Docker container
	docker volume create miden-db
	docker run --name miden-node \
			   -p 57291:57291 \
               -v miden-db:/db \
               -d miden-node-image

.PHONY: docker-build-faucet
docker-build-faucet: ## Builds the Miden faucet using Docker
	@CREATED=$$(date) && \
	VERSION=$$(cat bin/faucet/Cargo.toml | grep -m 1 '^version' | cut -d '"' -f 2) && \
	COMMIT=$$(git rev-parse HEAD) && \
	docker build --build-arg CREATED="$$CREATED" \
        		 --build-arg VERSION="$$VERSION" \
          		 --build-arg COMMIT="$$COMMIT" \
                 -f bin/faucet/Dockerfile \
                 -t miden-faucet-image .

.PHONY: docker-run-faucet
docker-run-faucet: ## Runs the Miden faucet as a Docker container
	docker volume create miden-db
	docker run --name miden-faucet \
			   -p 8080:8080 \
               -d miden-faucet-image
