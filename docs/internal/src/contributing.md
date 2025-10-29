# Contributing to Miden Node

#### First off, thanks for taking the time to contribute!

We want to make contributing to this project as easy and transparent as possible.

## Before you begin..

Start by commenting your interest in the issue you want to address - this let's us assign the issue to you and prevents
multiple people from repeating the same work. This also lets us add any additional information or context you may need.

We use the `next` branch as our active development branch. This means your work should fork off the `next` branch (and
not `main`).

### Typos and low-effort contributions

We don't accept PRs for typo fixes as these are often scanned for AI "contributors". If you find typos please open an
issue instead.

## Commits

Try keep your commit names and messages related to the content. This provides reviewers with context if they need to
step through your changes by commit.

This does not need to be perfect because we generally squash merge a PR - the commit naming is therefore only relevant
for the review process.

## Pre-PR checklist

Before submitting a PR, ensure that you're up to date by rebasing onto `next`, and that tests and lints pass by running:

```sh
# Runs the various lints
make lint
# Runs the test suite
make test
```

## Post-PR

Please don't rebase your branch once the PR has been opened. In other words - only append new commits. This lets
reviewers have a consistent view of your changes for follow-up reviews. Reviewers may request a rebase once they're
ready in order to merge your changes in.

## Any contributions you make will be under the MIT Software License

In short, when you submit code changes, your submissions are understood to be under the same
[MIT License](http://choosealicense.com/licenses/mit/) that covers the project. Feel free to contact the maintainers if
that's a concern.
