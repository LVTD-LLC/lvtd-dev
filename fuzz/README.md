# Fuzz testing

This is a separate cargo-fuzz workspace: libFuzzer mutates inputs using coverage
feedback and AddressSanitizer checks memory accesses, including dependencies.
Normal application builds remain on stable Rust and do not include libFuzzer.
No database or external service is used by these targets; SQLx uses the checked-in
offline metadata. A short green run is a smoke test, not proof of correctness or
complete coverage.

From the project root on Linux or macOS:

```sh
rustup toolchain install nightly --profile minimal --component rust-src
cargo install cargo-fuzz --version 0.13.2 --locked
bash scripts/fuzz.sh 30
```

The argument is seconds **per target**, excluding compilation and seed replay.
The script first replays every saved input, then runs a bounded mutation campaign
for each target. Inputs are limited to 4096 bytes, individual cases to 10 seconds,
and resident memory to 2 GiB. Panics, sanitizer findings, timeouts and out-of-memory
failures fail CI; ordinary parser rejection is expected. Assertions stay enabled.
For a longer local campaign use `bash scripts/fuzz.sh 600`. To keep seed replay and the same safety limits for selected targets:

```sh
bash scripts/fuzz.sh 30 input_validation webauthn
```

Unknown or repeated target names fail before a campaign starts. Omitting target
names still runs all targets. For a direct one-target campaign:

```sh
SQLX_OFFLINE=true cargo +nightly fuzz run TARGET -- -max_total_time=600 -max_len=4096
```

## Seeds, failures and regression tests

`seeds/TARGET/` contains small, reviewed examples in source control. The runner
copies them into the ignored `corpus/TARGET/` directory; libFuzzer adds inputs
that discover coverage. CI uploads the corpus and crash artifacts even on failure.
Download those artifacts and copy useful corpus inputs into a local campaign to
continue exploring them. Raw corpora and crash dumps can contain sensitive data:
never seed production cookies, tokens or customer payloads, and review files
before committing them.

To reproduce and minimize a failure, from the project root:

```sh
SQLX_OFFLINE=true cargo +nightly fuzz run TARGET fuzz/artifacts/TARGET/crash-HASH
SQLX_OFFLINE=true cargo +nightly fuzz tmin TARGET fuzz/artifacts/TARGET/crash-HASH
```

After fixing the cause, add the minimized input under `fuzz/seeds/TARGET/` and a
focused ordinary unit test asserting the intended behavior. The next CI run will
replay it before generating mutations. Do not catch panics or turn off validation
to make fuzzing pass. For corpus coverage reports use `cargo +nightly fuzz coverage
TARGET` (see the [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz/coverage.html)).

## Adding a target

Add a `[[bin]]` in `fuzz/Cargo.toml`, a `#![no_main]` harness under `fuzz_targets/`
using `libfuzzer_sys::fuzz_target!`, and a `seeds/TARGET/` directory with at least
one useful input. The runner discovers targets with `cargo fuzz list`. Exercise
real parsing/validation functions, include malformed and valid inputs, and assert
invariants or round trips where possible. Keep targets deterministic, bounded and
free of network/database side effects. If adding app-specific code, use a path
dependency on `../app` with a stable alias and select its required Cargo features.
Check formatting with `cargo fmt --manifest-path fuzz/Cargo.toml --all --check`
and dependencies with `cargo deny --manifest-path fuzz/Cargo.toml --config deny.toml check`.

## Included targets

- `input_validation`: base32/base64 round trips and malformed decoding, TOTP code
  normalization, relying-party URLs, safe storage keys and HTTP Accept parsing.
- `webauthn`: client JSON, COSE public keys and signatures, CBOR attestation parsing
  with valid client data so mutations reach beyond the initial JSON checks.
- `mcp`: JSON-RPC parsing, batches, notifications, version negotiation and tool
  dispatch using a local echo tool; every response must have a valid envelope.

The core/auth crates ship in every project, so these targets work even when the
app's optional account or MCP routes are disabled. They do not test a live server
or replace the application's database and authorization integration tests.
