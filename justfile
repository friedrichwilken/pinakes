# Recipes for working on pinakes. `just` lists them; `just check` is what CI runs offline.

set shell := ["bash", "-euo", "pipefail", "-c"]

release := "target/release/pinakes"
tmp := env("TMPDIR", "/tmp") / "pinakes-e2e"

# List the recipes.
default:
    @just --list --unsorted

# Debug build.
build:
    cargo build --all-targets

# Optimised build; the binary is target/release/pinakes.
release:
    cargo build --release --locked

# Install the pinakes binary from this checkout (cargo's bin directory, normally ~/.cargo/bin).
install:
    cargo install --path . --locked
    @echo "installed $(pinakes --version) at $(command -v pinakes)"

# Remove the binary `just install` put on PATH.
uninstall:
    cargo uninstall pinakes

# Apply rustfmt (not just check it).
fmt:
    cargo fmt

# The lint gate exactly as CI runs it: rustfmt check, clippy with pedantic, rustdoc with warnings denied.
lint:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Unit, integration, golden corpus and snapshot tests, plus the doctests; all offline.
test:
    cargo test --all-targets
    cargo test --doc

# Everything CI checks without the network: lint, then test.
check: lint test

# The end-to-end run on examples/pinakes.yaml against the two public repositories it names (network).
e2e: release
    rm -rf "{{tmp}}" && mkdir -p "{{tmp}}"
    cd examples && cp manifest.json "{{tmp}}/old-manifest.json"
    cd examples && ../{{release}} resolve --artifact "{{tmp}}/artifact"
    cd examples && ../{{release}} verify --artifact "{{tmp}}/artifact"
    cd examples && ../{{release}} diff "{{tmp}}/old-manifest.json" manifest.json > "{{tmp}}/diff.json" || [ $? -eq 3 ]
    cd examples && ../{{release}} eval --artifact "{{tmp}}/artifact" --queries queries.jsonl --json "{{tmp}}/eval.json"
    cd examples && ../{{release}} report --old "{{tmp}}/old-manifest.json" --eval-after "{{tmp}}/eval.json" > "{{tmp}}/report.md"
    @echo "report at {{tmp}}/report.md"
    cd examples && git checkout -- manifest.json residue.jsonl && git clean -fq -- duplicates.jsonl

# Refresh the pinned golden result and the report snapshots after an intended change (say why in the commit).
update-golden:
    UPDATE_GOLDEN=1 cargo test --test golden
    UPDATE_SNAPSHOTS=1 cargo test

# Build the Python wheel into dist/ and run its pytest suite (needs maturin and pytest on PATH).
wheel:
    maturin build --release --manifest-path python/Cargo.toml --out dist
    pip install --force-reinstall dist/*.whl
    pytest python/tests -v

# cargo audit over Cargo.lock (needs cargo-audit).
audit:
    cargo audit

# Symlink the curate skill into a project so Claude Code loads it there: `just skill ../my-project`.
skill project:
    mkdir -p "{{project}}/.claude/skills"
    ln -sfn "{{justfile_directory()}}/skills/curate" "{{project}}/.claude/skills/curate"
    @echo "linked {{project}}/.claude/skills/curate -> {{justfile_directory()}}/skills/curate"
