# Skill Manager

A small cross-platform desktop app for installing and removing agent skills.
The Tauri backend uses
[`jacobragsdale/skillbook`](https://github.com/jacobragsdale/skillbook) as its
catalog and installs skills at the user level.

## What works

- The Rust backend checks the `skillbook` repository's `main` branch on launch,
  every 15 minutes while the app is open, when returning to a stale window, and
  on demand.
- Catalog downloads are pinned to the commit GitHub reports and skipped when
  that commit has not changed.
- The last validated catalog remains available when GitHub is unreachable.
- Install copies a skill to `~/.agents/skills/<name>`.
- Updates automatically replace only unmodified skills previously installed by
  Skill Manager.
- Uninstall removes only directories carrying Skill Manager's ownership marker.
- Unmanaged skills that exactly match the catalog can be adopted without
  replacing their files.
- Differing unmanaged skills can be replaced manually after confirmation. The
  original is retained under
  `~/.agents/.skill-manager-backups/<name>/<timestamp>`.
- Locally modified Skill Manager installs remain protected.
- Managed skills removed from `skillbook` remain visible so they can be
  uninstalled.

The catalog is public, so no GitHub authentication is required. There is
deliberately no version selection or telemetry.

## Automatic updates

`skillbook/main` is the update channel. After a commit check, Skill Manager
downloads and validates an immutable archive only when the commit changed. It
then updates installed skills whose content still matches the digest recorded
when Skill Manager last installed them.

New skills are never installed automatically. Locally modified, unmanaged, and
legacy managed skills are not overwritten, and skills removed upstream are not
deleted. Each update uses a staged replacement with rollback, and a failure for
one skill does not prevent other eligible skills from updating. Failed updates
are retried on later checks.

## Unmanaged skill conflicts

When a directory already exists without Skill Manager's ownership marker, its
contents are compared with the current catalog:

- **Manage** adds the ownership marker only when every skill file already
  matches.
- **Replace…** requires confirmation, stages the catalog copy, moves the
  existing path to the backup directory, and then activates the staged copy. If
  activation fails, Skill Manager restores the original automatically.

Conflict resolution is always manual. Automatic catalog checks never adopt or
replace unmanaged skills.

The 15-minute checks run only while the app is open. Running updates while the
app is closed would require a separate tray or operating-system startup
integration.

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
