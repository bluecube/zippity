#!/bin/sh
# Runs all checks from CI locally
set -e
cargo clippy --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps --document-private-items
cargo t --all-features
