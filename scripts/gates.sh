#!/usr/bin/env bash
# Runs the six gates every change must pass, as the README lists them, each to its end; a clippy warning fails its gate, and the script fails where any gate failed.
set -euo pipefail
cd "$(dirname "$0")/.."

# r1-impshell builds only on Linux: on macOS the gates run in the Lima VM that setupcodebase.sh makes.
if [[ $(uname -s) == Darwin ]]; then
    exec limactl shell --start --workdir "$(pwd -P)" r1metal scripts/gates.sh
fi

failed=()
gate() {
    echo "=== $*"
    "$@" || failed+=("$*")
}
gate cargo test --workspace
gate cargo test -p r1-funcore -p r1-image -p r1-flasher -- --ignored
gate cargo clippy --workspace --all-targets -- -D warnings
gate cargo clippy -p r1-impshell --no-default-features --all-targets -- -D warnings
gate cargo fmt --all -- --check
gate cargo r1 visibility
if (( ${#failed[@]} )); then
    printf 'failed: %s\n' "${failed[@]}" >&2
    exit 1
fi
echo "every gate passed"
