#!/usr/bin/env bash
# Installs the LLVM toolchain OpenVM's rvr backend builds generated C with. It needs clang 19 or
# newer, which Ubuntu does not ship, and a matching lld. `-fuse-ld=lld` takes a bare name or an
# absolute path, so the versioned linker gets plain aliases.
set -euo pipefail

VERSION=22

wget -q https://apt.llvm.org/llvm.sh
chmod +x llvm.sh
sudo ./llvm.sh "${VERSION}"
rm llvm.sh

sudo apt-get install -y -qq --no-install-recommends "lld-${VERSION}"
sudo ln -sf "/usr/bin/lld-${VERSION}" /usr/bin/lld
sudo ln -sf "/usr/bin/ld.lld-${VERSION}" /usr/bin/ld.lld

"clang-${VERSION}" --version
