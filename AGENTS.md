# Agent notes

The crate denies rustc and clippy warnings (`[lints.rust] warnings = "deny"` in `src-tauri/Cargo.toml`). `cargo check`, `cargo test`, `cargo clippy`, and rust-analyzer must stay clean locally; CI runs the same gate as `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`. Do not land dead code, unused fields, or clippy lints that this would reject.

The rest of CI is in `.github/workflows/ci.yml`: frontend typecheck/lint/format/build, `cargo fmt --check`, then clippy, then tests.
