# Skill Manager

A small cross-platform desktop app for installing and removing agent skills.
The repository is intentionally a skeleton: it contains a bundled skill catalog,
a Tauri GUI, and safe user-level install and uninstall operations.

## What works

- Skills under `skills/` are embedded in the desktop app at build time.
- The GUI lists the bundled skills.
- Install copies a skill to `~/.agents/skills/<name>`.
- Uninstall removes only directories carrying Skill Manager's ownership marker.
- An existing unmanaged skill directory is left untouched and shown as a conflict.

There is deliberately no authentication, remote catalog synchronization,
version selection, automatic updating, or telemetry yet.

## Development

Requirements:

- Rust
- Node.js
- pnpm
- [Tauri's platform prerequisites](https://v2.tauri.app/start/prerequisites/)

Run the desktop app:

```bash
pnpm install
pnpm tauri dev
```

Run the checks:

```bash
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## Repository shape

```text
skills/               bundled skill catalog
src/                  React GUI
src-tauri/            Rust application and install logic
```

## Next likely steps

1. Load the catalog from tagged Git releases.
2. Add version selection and updates.
3. Package signed installers for Windows, macOS, and Linux.
