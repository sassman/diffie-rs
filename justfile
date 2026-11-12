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
