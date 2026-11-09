#!/usr/bin/env bash
# Run from the project root. Optional argument: seconds per target (default 30).
set -euo pipefail
seconds=${1:-30}
if [ "$#" -gt 0 ]; then shift; fi
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] || { echo 'Expected positive seconds per target' >&2; exit 2; }
export SQLX_OFFLINE=true
cargo +nightly metadata --manifest-path fuzz/Cargo.toml --locked --format-version 1 > /dev/null
# cargo-fuzz defaults to AddressSanitizer, debug assertions and overflow checks.
# Assign separately so discovery failure cannot silently turn into an empty loop.
targets=$(cargo +nightly fuzz list)
[[ -n "$targets" ]] || { echo 'No fuzz targets found' >&2; exit 1; }
# Optional explicit targets preserve the same replay and time budgets.
if [ "$#" -gt 0 ]; then
    selected=""
    for target in "$@"; do
        [[ "$target" =~ ^[a-zA-Z0-9_]+$ ]] || { echo "Invalid fuzz target: $target" >&2; exit 2; }
        case " $(echo "$targets" | tr '\n' ' ') " in *" $target "*) ;; *) echo "Unknown fuzz target: $target" >&2; exit 2 ;; esac
        case " $selected " in *" $target "*) echo "Duplicate fuzz target: $target" >&2; exit 2 ;; esac
        selected="$selected $target"
    done
    targets=$selected
fi
for target in $targets; do
    mkdir -p "fuzz/corpus/$target"
    cp -R "fuzz/seeds/$target/." "fuzz/corpus/$target/"
    cargo +nightly fuzz run "$target" -- -runs=0 -max_len=4096 -timeout=10 -rss_limit_mb=2048
    cargo +nightly fuzz run "$target" -- -max_total_time="$seconds" -max_len=4096 -timeout=10 -rss_limit_mb=2048
done
