#!/usr/bin/env bash
# Keeps a copy of everything the build takes from elsewhere in dependencies/, each as one archive, with their checksums.
set -euo pipefail
cd "$(dirname "$0")/.."

# On macOS what the build fetched lies in the Lima VM that setupcodebase.sh makes, and this runs there.
if [[ $(uname -s) == Darwin ]]; then
    exec limactl shell --start --workdir "$(pwd -P)" r1metal scripts/backupdependencies.sh
fi

BACKUPS=dependencies
KERNEL=kernel/src
MODULES=kernel/vendor/mediatek/kernel_modules
TOOLCHAIN_X64=kernel/toolchain/LLVM-23.1.3-Linux-X64.tar.zst
TOOLCHAIN_ARM64=kernel/toolchain/LLVM-23.1.3-Linux-ARM64.tar.zst
if [[ $(uname -m) == aarch64 ]]; then
    TOOLCHAIN=$TOOLCHAIN_ARM64
else
    TOOLCHAIN=$TOOLCHAIN_X64
fi
MUSL=kernel/toolchain/musl-1.2.5.tar.gz
STOCK_ZIP=stock/v0.8.293/rabbit_OS_v0.8.293.zip
CRATES=crates

for needed in "$KERNEL/.git" "$MODULES/.git" "$TOOLCHAIN" "$MUSL" "$STOCK_ZIP"; do
    if [[ ! -e $needed ]]; then
        echo "$needed is missing: run scripts/setupcodebase.sh first" >&2
        exit 1
    fi
done
mkdir -p "$BACKUPS"

# A repository whole, every branch and tag, as one bundle that git fetches from as from a remote; made anew once the repository has fetched since, as it does when its pin moves on.
bundle() {
    local url name
    url=$(git -C "$1" remote get-url origin)
    name=$(basename "$url" .git)
    if [[ -f $BACKUPS/$name.bundle && ! $1/.git/FETCH_HEAD -nt $BACKUPS/$name.bundle ]]; then
        return
    fi
    echo "fetching all of $url"
    rm -rf "$BACKUPS/$name.git"
    git clone --quiet --bare "$url" "$BACKUPS/$name.git"
    git -C "$BACKUPS/$name.git" bundle create "$PWD/$BACKUPS/$name.bundle.part" --all
    git -C "$BACKUPS/$name.git" bundle verify --quiet "$PWD/$BACKUPS/$name.bundle.part"
    mv "$BACKUPS/$name.bundle.part" "$BACKUPS/$name.bundle"
    rm -rf "$BACKUPS/$name.git"
}
bundle "$KERNEL"
bundle "$MODULES"

# LLVM's release archive as the build fetched it.
if [[ ! -f $BACKUPS/$(basename "$TOOLCHAIN") ]]; then
    cp --reflink=auto "$TOOLCHAIN" "$BACKUPS/"
fi

if [[ ! -f $BACKUPS/$(basename "$STOCK_ZIP") ]]; then
    cp --reflink=auto "$STOCK_ZIP" "$BACKUPS/"
fi

# The C library's source as the build fetched it.
if [[ ! -f $BACKUPS/$(basename "$MUSL") ]]; then
    cp --reflink=auto "$MUSL" "$BACKUPS/"
fi

# The source of every crate Cargo.lock names, packed anew whenever that file has changed.
if [[ ! -f $BACKUPS/$CRATES.tar.gz || Cargo.lock -nt $BACKUPS/$CRATES.tar.gz ]]; then
    echo "fetching the crates of Cargo.lock"
    rm -rf "${BACKUPS:?}/$CRATES"
    cargo vendor --locked --quiet "$BACKUPS/$CRATES" > /dev/null
    tar -czf "$BACKUPS/$CRATES.tar.gz.part" -C "$BACKUPS" "$CRATES"
    mv "$BACKUPS/$CRATES.tar.gz.part" "$BACKUPS/$CRATES.tar.gz"
    rm -rf "${BACKUPS:?}/$CRATES"
fi

(cd "$BACKUPS" && sha256sum -- *.bundle *.tar.gz *.tar.zst *.zip > SHA256SUMS)
du -h "$BACKUPS"/*
