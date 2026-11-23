# Launch the app; args pass through, e.g. `just run --target main /path/to/repo`
run *args:
    cargo run -p diffie -- {{args}}

# The full CI gate: formatting, lints, dependency hygiene, docs and tests.
ci-check:
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo deny check
    cargo shear
    cargo doc --workspace --no-deps --locked
    cargo test --workspace --locked

# The tag is what releases, so this only prepares one: review and commit
# the result, then tag `v<version>` and push the tag.
# Stage a release: bump the version, curate CHANGELOG.md from the commits.
release-prepare version:
    perl -pi -e 's/^version = ".*"/version = "{{version}}"/' Cargo.toml
    cargo update --workspace
    git-cliff --tag v{{version}} --output CHANGELOG.md
    # Only the version a visitor installs; the page's other version names the
    # release its demo clips were recorded from and moves with the clips.
    perl -pi -e 's/(?<=class="app-version">)v[0-9.]+/v{{version}}/' website/index.html
