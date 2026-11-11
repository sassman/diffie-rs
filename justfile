# Launch the app; args pass through, e.g. `just run --target main /path/to/repo`
run *args:
    cargo run -p diffie -- {{args}}
