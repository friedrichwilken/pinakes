# Python bindings

`python/` is a PyO3 crate (`pinakes-py`, module `pinakes`) built with maturin, wrapping the same
`Index` that `pinakes eval` scores the corpus with (`SPEC.md` §17.2): whatever ranking the
curator measured is exactly what this import searches, not a reimplementation of it.
`release.yml` attaches wheels for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and
`aarch64-apple-darwin` to each [release](releases.md) as `abi3` builds (CPython 3.10+); nothing
is published to PyPI, so install the wheel for your platform directly from the release assets:

```sh
pip install pinakes-1.0.3-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
```

(check a release's assets for the current version and your platform's filename).

```python
from pinakes import Index

index = Index.build("artifact")
print(index.page_count, "pages,", index.searchable_count, "searchable")
hits = index.search("how do I install the service", k=3)
page = index.read(hits[0].page_id)
print(page.title, page.url)
```
