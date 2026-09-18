# Development

```sh
just check      # the offline CI gates: rustfmt check, clippy, rustdoc, all tests
just install    # cargo install --path . --locked; puts `pinakes` on PATH
just            # every recipe: build, release, fmt, lint, test, e2e, update-golden, wheel, audit, skill
```

Without [`just`](https://github.com/casey/just), the gates are:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Library crate `pinakes` (modules per `SPEC.md` §9), binary `pinakes` (`src/main.rs`, clap
only). Errors are `thiserror` types in the library and `anyhow` at the CLI edge. Report
snapshots live in `tests/snapshots/`; `UPDATE_SNAPSHOTS=1 cargo test` refreshes them. The
golden corpus check in `tests/golden.rs` runs with the rest of the suite;
`UPDATE_GOLDEN=1 cargo test --test golden` re-pins its expected result.

## Unit tests versus e2e

`cargo test` is offline and deterministic: unit tests in the modules, the integration tests in
`tests/` against fake fetchers and scripts, the golden corpus and the report snapshots. None of
them touch the network, so they say nothing about whether the codeload download, the GitHub
archived check or the example resolver still work against the real world. That is what the `e2e`
job in CI covers: it builds the release binary and runs `resolve`, `verify`, `diff`, `eval` and
`report` on `examples/pinakes.yaml` against the two public repositories it names. It fails only
on exit codes the commands do not document, so an upstream commit that changes the corpus (exit
3 from `diff`) is reported, not treated as a failure. Run it locally:

```sh
cargo build --release
cd examples
../target/release/pinakes resolve
```

## CI

`.github/workflows/ci.yml`, on pushes to `main` and pull requests: `lint` (`cargo fmt --check`,
clippy with `-D warnings`, `cargo doc` with warnings denied); `test` on Ubuntu and macOS
(`cargo test --all-targets` plus the doctests); `msrv` (a build on the `rust-version` from
`Cargo.toml`); `e2e` (the example, as above); `audit` (`cargo audit`, also weekly on a
schedule). Dependabot opens weekly, grouped update PRs for Cargo and the Actions. See
[`CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`AGENTS.md`](../../AGENTS.md).
