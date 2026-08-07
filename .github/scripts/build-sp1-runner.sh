#!/usr/bin/env bash
# Builds the helper binary sp1-core-executor-runner embeds and points the crate at it through the
# override its build script honours. Left alone, that build script puts the binary under the crate's
# own source directory in the cargo registry, which the cache drops while keeping the target
# directory recording the path, so a restored build compiles against a path that no longer exists.
set -euo pipefail

MANIFEST=$(cargo metadata --format-version 1 --locked |
  jq -er '.packages[] | select(.name == "sp1-core-executor-runner-binary") | .manifest_path')
TARGET="${PWD}/target/sp1-native-bins"

CARGO_TARGET_DIR="${TARGET}" cargo build --release --locked --manifest-path "${MANIFEST}"

echo "SP1_CORE_RUNNER_OVERRIDE_BINARY=${TARGET}/release/sp1-core-executor-runner-binary" >>"${GITHUB_ENV}"
