# Contributing

Thanks for taking the time. The contract for everything the tool does is in
[`SPEC.md`](SPEC.md); [`AGENTS.md`](AGENTS.md) holds the working rules in more detail.

## Build

```sh
cargo build            # debug
cargo build --release  # target/release/pinakes
just install           # cargo install --path . --locked, onto PATH
just                   # lists every recipe (the justfile mirrors CI)
```

Rust 2024 edition; the minimum supported version is the `rust-version` in `Cargo.toml`.

## Test

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

`cargo test` runs the unit tests, the integration tests, the golden corpus check and the
report snapshots, all offline. `UPDATE_GOLDEN=1 cargo test --test golden` and
`UPDATE_SNAPSHOTS=1 cargo test` refresh the pinned results after an intended change; say why
in the commit body.

## Run the example

```sh
cargo build --release
cd examples
../target/release/pinakes resolve --artifact /tmp/pinakes-artifact
../target/release/pinakes verify --artifact /tmp/pinakes-artifact
../target/release/pinakes eval --artifact /tmp/pinakes-artifact
```

This fetches two public repositories, so it needs the network. CI runs the same sequence as
the `e2e` job.

## Open a pull request

1. Branch from `main`.
2. If the change alters documented behaviour, update `SPEC.md` first, then the code, then the
   docs (`README.md`, `docs/manual/`, `docs/tutorials/`).
3. Run the three gates above; CI runs them again plus an MSRV build, the example and
   `cargo audit`.
4. Commit in logical steps with an imperative subject and a body that says what and why.
5. Open the PR with a short description of the change and how you tested it.

No sign-off or CLA is required. By contributing you agree that your contribution is licensed
under Apache-2.0, like the rest of the project.
