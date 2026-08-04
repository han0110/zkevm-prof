#!/bin/bash

# Copied from https://github.com/eth-act/ere/blob/main/.github/scripts/free-up-disk-space.sh

set -e -o pipefail

df -h /

sudo rm -rf /usr/share/dotnet           # remove dotnet
sudo rm -rf /usr/local/lib/android      # remove android
sudo rm -rf /opt/ghc /usr/local/.ghcup  # remove haskell
sudo rm -rf /opt/hostedtoolcache        # remove codeql

df -h /
