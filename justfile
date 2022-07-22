# Build all Rust crates.
build-crates:
        cargo build --workspace --release

# Run the app.
run-app:
        cargo tauri dev

# Local Variables:
# mode: makefile
# End:
# vim: set ft=make :
