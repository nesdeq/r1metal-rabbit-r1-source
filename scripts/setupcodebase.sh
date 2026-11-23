#!/usr/bin/env bash
# Takes a fresh clone to a built out/boot.img: host tools, Rust target, Rabbit's firmware, udev rule, first build. On macOS all of it runs in a Linux VM it makes first.
set -euo pipefail
cd "$(dirname "$0")/.."

RELEASE=v0.8.293
ZIP=rabbit_OS_v0.8.293.zip
# The archive's SHA-256, and where its images lie once it is unpacked, as the build takes them.
STOCK_SHA256=f6c28b221a91055ec5e44ab0ac0ee59c7e6b52fac12ad08f2bb23c8f0551c6c0
STOCK_IMAGES=stock/v0.8.293/rabbit_OS_v0.8.293_20250516110545
STOCK_DIR=$(dirname "$STOCK_IMAGES")
# Written once the archive is unpacked whole, holding the archive's SHA-256.
UNPACKED=$STOCK_DIR/r1metal-unpacked
RELEASE_URL=https://github.com/rabbit-hmi-oss/firmware/releases/download/$RELEASE/$ZIP
# Where backupdependencies.sh keeps a copy of what is fetched from elsewhere, with the checksums of the copies; the firmware archive, the two repositories, the toolchain and the C library's source found there are taken before the network is asked.
BACKUPS=dependencies
KERNEL=kernel/src
KERNEL_URL=https://github.com/rabbit-hmi-oss/android_kernel_rabbit_mt6765
MODULES=kernel/vendor/mediatek/kernel_modules
MODULES_URL=https://github.com/rabbit-hmi-oss/android_kernel_modules_rabbit_mt6765
# LLVM's release the kernel and the firmware's C code are built with, for Linux on x86-64 and on arm64, as the build fetches the one for its CPU to here and unpacks it beside.
TOOLCHAIN_X64=kernel/toolchain/LLVM-23.1.3-Linux-X64.tar.zst
TOOLCHAIN_ARM64=kernel/toolchain/LLVM-23.1.3-Linux-ARM64.tar.zst
# The C library's source, whose headers the firmware's C code is compiled against; the build fetches it to here.
MUSL=kernel/toolchain/musl-1.2.5.tar.gz
TARGET=aarch64-unknown-linux-musl
UDEV_RULE=udev/70-r1metal.rules
UDEV_DIR=/etc/udev/rules.d
# The Lima VM that sets up and builds on macOS: Linux on the Mac's CPU, which sees this workspace where the Mac has it and keeps the kernel's trees and target/ on a disk of its own.
VM=r1metal
LIMA_TEMPLATE=lima/r1metal.yaml

if [[ $(uname -s) == Darwin ]]; then
    if ! command -v limactl > /dev/null; then
        echo "missing: limactl" >&2
        exit 1
    fi
    here=$(pwd -P)
    if ! limactl list --quiet | grep -qx "$VM"; then
        # A worktree's repository lies outside it; the VM reads it there too, for the commits the build counts.
        repository=$(git rev-parse --path-format=absolute --git-common-dir)
        if [[ $repository == "$here"/* ]]; then
            repository=
        fi
        limactl create --tty=false --name="$VM" --param Workspace="$here" ${repository:+--mount "$repository"} \
            --cpus="$(sysctl -n hw.ncpu)" --memory="$(( $(sysctl -n hw.memsize) >> 31 ))" "$LIMA_TEMPLATE"
    fi
    workspace=$(limactl list "$VM" --format '{{.Param.Workspace}}')
    if [[ $workspace != "$here" ]]; then
        echo "the VM $VM builds $workspace; limactl delete $VM makes way for one that builds here" >&2
        exit 1
    fi
    exec limactl shell --start --workdir "$here" "$VM" scripts/setupcodebase.sh
fi

missing=()
for tool in git curl tar unzip zstd make gcc sha256sum bc bison flex perl gzip awk cmp install sudo udevadm rustup cargo; do
    command -v "$tool" > /dev/null || missing+=("$tool")
done
if ! printf '#include <openssl/opensslv.h>\n' | gcc -E -x c - > /dev/null 2>&1; then
    missing+=("OpenSSL headers")
fi
if (( ${#missing[@]} )); then
    echo "missing: ${missing[*]}" >&2
    exit 1
fi

rustup target add "$TARGET"

# Holds a copy in the backups to the checksum they keep of it; one they keep none of, or one that differs, stops the setup.
checked() {
    if ! (cd "$BACKUPS" && awk -v name="$1" '$2 == name' SHA256SUMS | sha256sum --check --quiet --strict -); then
        echo "$BACKUPS/$1 is not the copy whose checksum $BACKUPS/SHA256SUMS keeps" >&2
        exit 1
    fi
}

# A file that is missing here comes from its backup if that is there.
take() {
    local file=$1 name
    name=$(basename "$file")
    if [[ -f $file || ! -f $BACKUPS/$name ]]; then
        return
    fi
    checked "$name"
    echo "taking $file from $BACKUPS/$name"
    mkdir -p "$(dirname "$file")"
    cp --reflink=auto "$BACKUPS/$name" "$file"
}

if [[ ! -f $UNPACKED || $(< "$UNPACKED") != "$STOCK_SHA256" ]]; then
    take "$STOCK_DIR/$ZIP"
    if [[ ! -f $STOCK_DIR/$ZIP ]]; then
        mkdir -p "$STOCK_DIR"
        curl -fL -o "$STOCK_DIR/$ZIP.part" "$RELEASE_URL"
        mv "$STOCK_DIR/$ZIP.part" "$STOCK_DIR/$ZIP"
    fi
    echo "$STOCK_SHA256  $STOCK_DIR/$ZIP" | sha256sum --check --quiet --strict -
    unzip -q -o "$STOCK_DIR/$ZIP" -x '__MACOSX/*' -d "$STOCK_DIR"
    echo "$STOCK_SHA256" > "$UNPACKED"
fi

# The rule is for a computer with USB, which the VM on macOS has none of.
if [[ -d /sys/bus/usb ]] && ! cmp -s "$UDEV_RULE" "$UDEV_DIR/$(basename "$UDEV_RULE")"; then
    sudo install -m 644 "$UDEV_RULE" "$UDEV_DIR/"
    sudo udevadm control --reload-rules
    sudo udevadm trigger
fi

# A repository that is missing here comes from its backup if that is there; the build then finds it, sees that it stands at none of the commits its patches left one at, puts them on it and fetches nothing.
restore() {
    local directory=$1 url=$2 name
    name=$(basename "$url" .git).bundle
    if [[ -d $directory/.git || ! -f $BACKUPS/$name ]]; then
        return
    fi
    checked "$name"
    echo "taking $directory from $BACKUPS/$name"
    mkdir -p "$(dirname "$directory")"
    git clone --quiet "$BACKUPS/$name" "$directory"
    git -C "$directory" remote set-url origin "$url"
}
restore "$KERNEL" "$KERNEL_URL"
restore "$MODULES" "$MODULES_URL"

if [[ $(uname -m) == aarch64 ]]; then
    take "$TOOLCHAIN_ARM64"
else
    take "$TOOLCHAIN_X64"
fi
take "$MUSL"

cargo run -p r1-flasher -- build
