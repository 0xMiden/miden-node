# Branching policy

The repository has two kinds of protected branches:

- `next` is the active development branch and the default target for pull requests.
- `release/vX.Y` is a maintained branch for a specific minor release line, for example `release/v0.16`.

## Creating a release branch

`next` may continue to represent the latest stable release line until development requires a breaking change. Before
that change is merged, maintainers create the corresponding `release/vX.Y` branch from `next`. The release branch
preserves the compatible release line while breaking development continues on `next`.

The version portion of a release branch contains only the major and minor version. Each component is a non-negative
integer without leading zeroes. Only repository administrators create new release branches.

## Applying changes

After a release branch is created, it permanently diverges from `next`. Releases are not promoted by merging `next` into
a release branch or by merging a release branch back into `next`.

Changes should target the branch where they are required:

- New development targets `next`.
- Maintenance changes target the applicable `release/vX.Y` branch.
- A change needed on multiple branches is applied to each branch through its own pull request, normally by backporting
  the original change.

## Branch protection

The `Protected branches` ruleset applies to `next` and all `release/vX.Y` branches. It prevents deletion and
non-fast-forward updates, requires pull requests, and requires the repository's test status check.

Administrators can bypass reviews and required checks through a pull request when necessary, but cannot bypass the pull
request requirement. This preserves an audit trail for every protected branch change.
