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

## Windows support

- The app uses native user-profile and local-cache directories and never
  constructs paths with a hard-coded separator.
- Git is not required at runtime. Catalog refreshes use GitHub's HTTPS archive,
  avoiding `git.exe` discovery, shell quoting, and console-encoding issues.
- Skill metadata is read as UTF-8. UTF-8 BOMs and CRLF line endings are
  accepted, while all other skill files are copied byte-for-byte.
- Archive paths are checked against Windows naming rules before extraction,
  including reserved device names and case-insensitive collisions.
- Repository attributes and editor settings keep project text in UTF-8/LF on
  every platform, independent of a contributor's `core.autocrlf` setting.
- CI runs the complete frontend and Rust check suite natively on both Windows
  and Linux.

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
