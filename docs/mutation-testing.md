# Mutation testing

A passing test can still miss a bug. Mutation testing temporarily changes your
Rust code (for example, reverses a comparison or returns a constant) and runs
the tests. A **caught** mutant made a test fail. A **missed** mutant left every
test passing: investigate whether a behavior assertion or boundary case is
missing. An equivalent change may not alter observable behavior at all.

This follows Jason Turner's *Make Bugs (nearly) Impossible*, chapter 25,
“Mutation Test Your Tests,” which names `cargo-mutants` as the Rust tool.
The book motivates testing the tests; the CI scope below is our implementation
choice. See the [cargo-mutants manual](https://mutants.rs/).

## Run locally

Install the same version CI uses:

```sh
cargo install cargo-mutants --version 27.1.0 --locked
```

Start the local Postgres service, export `DATABASE_URL` from `.env.example`,
and apply the migrations with `cargo sqlx migrate run`. Use a disposable
Postgres server whose role can create databases; the integration tests create
fresh databases. Run from the workspace root:

```sh
# Inspect the candidate mutations without compiling or running tests.
cargo mutants --workspace --list

# Test the code changed since a Git revision (including staged, unstaged and untracked Rust files).
bash scripts/ci/mutation-test.sh "$(git merge-base origin/main HEAD)"

# Audit all bundled Rust code, enabling optional capabilities for this test run.
bash scripts/ci/mutation-test.sh

# Focus a full audit on one module.
bash scripts/ci/mutation-test.sh '' --file 'crates/core/src/http.rs'
```

`.cargo/mutants.toml` runs the entire workspace's tests for each mutation,
including the app's integration tests when a shared crate changes. Mutation runs
enable all features because the parser also visits feature-gated source: mutating
a function that is compiled out would otherwise produce a false survivor. This
does not change your app's selected defaults; ordinary CI tests them separately.
The configuration uses the lockfile and measures a clean baseline before mutants. SQL compilation uses the checked-in offline metadata;
tests still need the database. Tests use local provider doubles, not live accounts.
The helper also forces a relative `CARGO_TARGET_DIR=target` inside the tool's
temporary copy. Mutated artifacts must never enter a normal or shared build
cache: Cargo can otherwise reuse deliberately broken code in a later ordinary
build. Use the helper for execution even if your shell exports a shared target
directory. This costs a fresh build per audit but leaves source and normal build
artifacts unchanged.

## Continuous integration

Every pull request and push to `main` mutation-tests Rust functions intersecting
the diff from the PR base or previous push SHA. The PR uses the checked-out merge
commit so the report matches the tested tree. A first push without a base runs
normal tests and establishes the revision for future diffs; it does not claim
mutation coverage. Manual CI runs test the last commit by default; select
`full_mutation` to audit the entire workspace. Full audits can take hours; narrow
local runs with `--file` when investigating a specific area.

Changed-code checks do not measure historical mutation coverage. They can also
miss regressions caused solely by changed tests or configuration. Run a full or
focused audit for those changes. Only the simulated WebAuthn authenticator in `crates/auth/src/testing.rs` is
excluded: it is test support, not production code. No runtime modules or
surviving mutants are blanket-excluded.
A diff with no candidate mutants is reported as such; it proves no mutation coverage.

CI fails for missed mutants, timeouts, a failing baseline, or tool errors.
The baseline prevents an already broken suite from appearing to catch bugs.
A timeout is an investigation item, not proof that an assertion caught a bug.
Uncompilable mutations are reported as **unviable**, not caught.

Download the `mutation-project` CI artifact, or inspect `mutants.out/` locally:
`outcomes.json` records outcomes, `missed.txt` lists survivors, and `diff/` and
`log/` show each change and its test output. Reports are retained in CI for 14 days
and ignored by Git. They can contain source code; treat them like the repository.

For a survivor, add a test of the intended observable behavior, confirm it fails
under that mutation, then rerun the focused audit. Consider boundary values and
negative cases, not only the happy path. For an equivalent mutant, document the
reason before adding a narrow exclusion in `.cargo/mutants.toml`. Do not skip the
baseline, suppress a failure, or weaken tests just to make CI green.
