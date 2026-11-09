#!/usr/bin/env bash
# Run from the Cargo workspace root; extra arguments select Cargo features.
set -euo pipefail
: "${DATABASE_URL:?Set DATABASE_URL to a disposable Postgres database with CREATEDB}"
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# Keep this pin in sync with the coverage jobs and README.
toolchain=nightly-2026-10-01
export SQLX_OFFLINE=true
export CARGO_INCREMENTAL=0
output="$PWD/target/coverage"
mkdir -p "$output"
# Remove prior reports so a failed run cannot publish stale results.
rm -f "$output/coverage.json" "$output/lcov.info" "$output/summary.md"
rm -rf "$output/html"
cargo +"$toolchain" llvm-cov clean --workspace
cargo +"$toolchain" llvm-cov --workspace --locked --branch --no-report "$@"
cargo +"$toolchain" llvm-cov report --branch --json --output-path "$output/coverage.json"
cargo +"$toolchain" llvm-cov report --branch --lcov --output-path "$output/lcov.info"
cargo +"$toolchain" llvm-cov report --branch --html --output-dir "$output"
python3 "$script_dir/coverage-summary.py" "$output/coverage.json" > "$output/summary.md"
cat "$output/summary.md"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$output/summary.md" >> "$GITHUB_STEP_SUMMARY"
fi
