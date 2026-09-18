# Releases

A release is a tag `vX.Y.Z` on `main`, matching the `version` in `Cargo.toml`. Pushing the
tag runs `.github/workflows/release.yml`, which builds release binaries for
`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl` (static), `aarch64-unknown-linux-gnu`
and `aarch64-apple-darwin` (Apple silicon only; Intel Macs are not supported), packages each as
`pinakes-X.Y.Z-<target>.tar.gz` (the binary, `README.md`, `LICENSE` and `SPEC.md`), builds the
Python wheel (see [Python bindings](python-bindings.md)) for the three platforms it supports,
writes a `SHA256SUMS` file and attaches everything to a GitHub release created from the tag.
Nothing is published to crates.io or PyPI.

```sh
git tag -a v0.1.0 -m "pinakes 0.1.0"
git push origin v0.1.0
```

`v1` (and every other major tag) is a moving tag that `release.yml` force-updates to the commit
of the latest matching release, so the [setup action](setup-action.md) pinned to `@v1` tracks
new releases automatically. Never push a major tag by hand — it only triggers a second, wasted
release run.
