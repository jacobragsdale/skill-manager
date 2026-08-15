# Agent notes

CI treats Rust warnings as errors. `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` must stay clean. Do not land dead code, unused fields, or clippy lints that this gate would reject. Run that command before finishing Rust work.

The rest of CI is in `.github/workflows/ci.yml`: frontend typecheck/lint/format/build, `cargo fmt --check`, then clippy, then tests.
