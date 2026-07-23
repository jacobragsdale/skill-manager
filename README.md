# Skill Manager

A small cross-platform desktop app for installing and removing agent skills.
The Tauri backend uses
[`jacobragsdale/skillbook`](https://github.com/jacobragsdale/skillbook) as its
catalog and installs skills at the user level.

## What works

- The GUI refreshes the catalog from the `skillbook` repository's `main`
  branch.
- The last validated catalog remains available when GitHub is unreachable.
- Install copies a skill to `~/.agents/skills/<name>`.
- Updates replace only unmodified skills previously installed by Skill
  Manager.
- Uninstall removes only directories carrying Skill Manager's ownership marker.
- Existing unmanaged or locally modified skill directories are left untouched.
- Managed skills removed from `skillbook` remain visible so they can be
  uninstalled.

The catalog is public, so no GitHub authentication is required. There is
deliberately no version selection, background updating, or telemetry.

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
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## Repository shape

```text
src/                  React GUI
src-tauri/            GitHub catalog, cache, and install logic
```

## Next likely steps

1. Publish tagged `skillbook` releases and let the app select stable versions.
2. Package signed installers for Windows, macOS, and Linux.
