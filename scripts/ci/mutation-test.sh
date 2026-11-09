#!/usr/bin/env bash
# Run from the Cargo workspace root. Optional first argument: a Git base SHA.
# Without a base, audit the whole workspace. Remaining args go to cargo-mutants.
set -euo pipefail
base=${1:-}
if [ "$#" -gt 0 ]; then shift; fi
: "${DATABASE_URL:?Use a disposable Postgres database with CREATEDB for the test suite}"
export SQLX_OFFLINE=true
# Never put deliberately broken artifacts into the normal/shared Cargo cache.
# A relative target stays inside each temporary tree created by cargo-mutants.
export CARGO_TARGET_DIR=target

if [ -z "$base" ]; then
    exec cargo mutants --workspace "$@"
fi
# Do not silently turn an invalid base into an empty successful check.
git cat-file -e "$base^{commit}"
diff_file=$(mktemp)
trap 'rm -f -- "$diff_file"' EXIT
# --relative is essential when this workspace is nested in another repository.
# Diff to the checked-out tree (the PR merge commit in Actions), not a remote tip.
git diff --no-ext-diff --no-renames --relative "$base" -- . > "$diff_file"
# Git diff omits untracked Rust files. Include them so a local pre-commit run
# cannot silently pass without checking newly added implementations.
untracked=$(mktemp)
trap 'rm -f -- "$diff_file" "$untracked"' EXIT
git ls-files --others --exclude-standard -z -- '*.rs' > "$untracked"
while IFS= read -r -d '' path; do
    result=0
    git diff --no-ext-diff --no-index -- /dev/null "$path" >> "$diff_file" || result=$?
    [[ "$result" -le 1 ]] || exit "$result"
done < "$untracked"
cargo mutants --workspace --in-diff "$diff_file" "$@"
