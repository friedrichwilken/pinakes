# Setup action for GitHub Actions

[`action.yml`](../../action.yml) at the repository root is a composite action, "Set up
pinakes", for any workflow — a consumer's own CI, not only the [curation workflow](weekly-workflow.md) —
that just wants the `pinakes` binary on `PATH`. It picks the right release asset for the runner
(Linux x86_64 glibc or musl, Linux aarch64, or Apple silicon macOS; there is no Intel Mac build),
downloads it, verifies it against the release's `SHA256SUMS`, and adds it to `PATH`:

```yaml
- uses: friedrichwilken/pinakes@v1
  with:
    version: latest        # or a specific "X.Y.Z", without a leading "v"
    github-token: ${{ github.token }}   # avoids the anonymous GitHub API rate limit
    musl: false             # true picks x86_64-unknown-linux-musl over the glibc build
- run: pinakes --version
```

`v1` is a moving tag: `release.yml` force-updates it to the commit of every `v1.x.y` release, so
pinning to `@v1` tracks the latest compatible release automatically, the way `actions/checkout@v4`
does. Pin a specific tag (`@v1.0.1`) instead for a fully reproducible workflow. The
[weekly curation workflow](weekly-workflow.md) uses this same action when it runs outside this
repository, and falls back to `cargo install --path .` when it runs inside it.
