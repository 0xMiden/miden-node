# Release process

A release is created by tagging the commit to release. It does not require a merge between `next` and a release branch.
The commit may be on `next` while it still represents that release line, or on the applicable `release/vX.Y` branch
after the branches have diverged.

## Release tags

Release tags use one of these forms:

- `vX.Y.Z` for a stable release, for example `v0.16.2`.
- `vX.Y.Z-suffix` for a prerelease, for example `v0.17.0-rc.1`.

`X`, `Y`, and `Z` are non-negative integers without leading zeroes. A prerelease suffix must start with `-` and contain
at least one character after it.

The `Release tags` ruleset is the source of truth for the accepted tag format and restricts who can create, update, or
delete release tags.

## Creating a release

1. Ensure the target commit is in a publishable state. Every publishable workspace package must use the same version.
2. Create the release tag on that commit and push it to GitHub. The tag without its leading `v` must exactly match the
   workspace package version.
3. The `Release` workflow validates the tag and package versions, checks crate builds and the minimum supported Rust
   version, dry-runs crate, Docker image, and Compose publishing, then publishes the Docker images and Compose
   application.
4. After those checks pass, the workflow creates the GitHub release and release notes, then starts the crates.io and
   Debian publishing workflows.

The root `docker-compose.yml` includes the component models under `compose/`, keeping direct local Compose commands and
profiles independent of additional `-f` arguments. Local includes cannot be published directly, so the publication jobs
first render the complete, all-profile model without interpolating its variables or normalizing project resource names.
They then merge `compose/publish.yml` into that flattened model using the same options and publish the result.

The local model supports custom genesis configuration through a bind mount, which cannot be included in a portable OCI
artifact. `compose/publish.yml` uses Compose's `!override` tag to replace that volume list and configures the published
application to use the validator's built-in genesis configuration.

The workflow uses a broad `v*` trigger because GitHub Actions does not use the same pattern language as repository
rulesets. Its preflight step verifies that this trigger still matches the target of the `Release tags` ruleset, while
the ruleset itself enforces the release-tag format.
