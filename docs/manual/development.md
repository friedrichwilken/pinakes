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
only). Errors are `thiserror` types in the library and `anyhow` at the CLI edge.

Five suites pin behaviour rather than assert it, each with its own refresh flag:

- `tests/golden.rs`, `tests/golden_mdbook.rs`, `tests/backend_tantivy_golden.rs` and
  `tests/backend_dense_hybrid_golden.rs` compare `eval` on a fixture corpus with an
  `expected*.json` file. Refresh with `UPDATE_GOLDEN=1 cargo test --test <name>`.
- `tests/snapshots/` holds rendered reports. Refresh with `UPDATE_SNAPSHOTS=1 cargo test`.
- `tests/pipeline_pin.rs` pins the bytes of `manifest.json`, `residue.jsonl`,
  `duplicates.jsonl`, `report.md` and `residue list` written by a fresh resolve and a
  `--from-manifest` one. Refresh with `UPDATE_SNAPSHOTS=1 cargo test --test pipeline_pin`.
- `tests/schema.rs` pins the JSON Schema of `manifest.json` at `docs/schemas/manifest.schema.json`
  byte for byte. Refresh with `UPDATE_SCHEMAS=1 cargo test --test schema`.
- `tests/eval_cli.rs` and `tests/eval_compare_cli.rs` pin the CLI's JSON and table output for
  `eval` through the binary; update the hand-written assertions directly when a change is
  intended, there is no refresh flag.

Refresh only when the change is intended, and say why in the commit body.

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
`Cargo.toml`); `e2e` (the example, as above); `python` (builds the [wheel](python-bindings.md)
with maturin and runs `python/tests`); `action` on Ubuntu and macOS (installs a real past
release with `uses: ./` and checks the binary it puts on `PATH` runs); `audit` (`cargo audit`,
also weekly on a schedule; an advisory that cannot be fixed yet is ignored in
`.cargo/audit.toml` with its reason and the condition for removing the entry). Dependabot opens
weekly, grouped update PRs for Cargo and the Actions. See
[`CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`AGENTS.md`](../../AGENTS.md).
