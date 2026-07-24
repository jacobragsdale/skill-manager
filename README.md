# Skill Manager

A small cross-platform desktop app for installing and removing agent skills.
It ships with
[`jacobragsdale/skillbook`](https://github.com/jacobragsdale/skillbook) as its
built-in source, and users can add their own Git repositories as additional
skill sources. Skills are installed at the user level.

## What works

- The built-in `skillbook` source works without Git installed.
- Custom sources accept `https://` and `ssh://` Git URLs and track each
  repository's default branch.
- Sources can be added and removed from the app.
- Each source has its own commit, refresh status, error message, and last
  validated offline cache.
- A source that is unavailable does not prevent other sources from refreshing
  or using their cached catalogs.
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
- Managed skills removed from a repository, or installed from a source that was
  later removed, remain visible so they can be uninstalled.

## Skill sources

`skillbook` is always available as the built-in source. It follows
`skillbook/main` and uses GitHub's HTTPS API and immutable commit archives, so
the default experience does not require Git or GitHub authentication.

Custom sources use the system `git` executable and may use either an HTTPS or
SSH URL. Skill Manager follows the remote repository's default branch and pins
each validated cache to the resolved commit. Private repositories work only
when the user's existing Git credential helper or SSH setup can already access
them. Skill Manager does not ask for, store, or manage credentials.

To add a custom source, open **Sources**, enter its Git URL, and choose
**Add source**. Skill Manager makes a shallow, blob-filtered sparse checkout of
the repository's `skills/` directory into a staging area, validates the copied
catalog, and registers the source only after validation succeeds. Catalog copies
are capped at 2,000 files and 50 MB; the temporary Git checkout can be larger if
the remote server does not honor partial-clone filtering. The source list shows
its URL, current commit, and refresh status. Custom sources can be removed from
the same view; the built-in source cannot be removed.

Every source repository uses this layout:

```text
skills/
  my-skill/
    SKILL.md
    ...other skill files
```

Each immediate child of `skills/` is one skill. Its directory name and
`SKILL.md` metadata must be valid and consistent. A repository with no valid
skills is rejected.

Source definitions are stored in the app's platform configuration directory.
Each source has a separate cache in the platform-local cache directory. If a
refresh fails, Skill Manager keeps that source's last validated cache active
and reports the failure on that source instead of making the entire catalog
unavailable.

## Automatic updates

The built-in source checks `skillbook/main`; each custom source checks its
remote default branch. Sources are checked on launch, every 15 minutes while
the app is open, when returning to a stale window, and on demand. A catalog is
downloaded only when its commit changes, then validated and activated with a
staged replacement.

Skill Manager then updates installed skills whose content still matches the
digest recorded when Skill Manager last installed them. Ownership is
source-aware: an update is eligible only when the installed skill belongs to
that source.

New skills are never installed automatically. Locally modified, unmanaged, and
legacy managed skills are not overwritten, and skills removed upstream are not
deleted. Each skill update uses a staged replacement with rollback, and a
failure for one skill does not prevent other eligible skills from updating.
Failed refreshes and updates are retried on later checks.

## Unmanaged skill conflicts

When a directory already exists without Skill Manager's ownership marker, its
contents are compared with the selected source's catalog:

- **Manage** adds the ownership marker only when every skill file already
  matches.
- **Replace…** requires confirmation, stages the catalog copy, moves the
  existing path to the backup directory, and then activates the staged copy. If
  activation fails, Skill Manager restores the original automatically.

Conflict resolution is always manual. Automatic catalog checks never adopt or
replace unmanaged skills.

## Duplicate skill names

The install directory remains flat, so only one source can own
`~/.agents/skills/<name>` at a time. Skills with the same name remain visible
under each source, but entries from other sources show a source conflict while
one copy is installed.

Skill Manager never treats a same-named skill from another source as an update
and never switches its ownership automatically. To switch sources, uninstall
the currently managed copy, then install the copy from the other source.

## Removing sources and upgrading

Removing a custom source removes its registration and cache. It never removes
that source's installed skills. Those managed installs remain visible as
belonging to a removed source and can still be safely uninstalled; they are not
updated until their source is added again.

Upgrading from the original single-source release migrates the existing
`skillbook` cache into the built-in source cache when possible. Existing
version-1 ownership markers for `skillbook` remain recognized. Skill Manager
does not rewrite installed skill files merely to migrate metadata; a marker is
rewritten only as part of a later successful managed operation. Cache and
marker migration are safe to retry.

The 15-minute checks run only while the app is open. Running updates while the
app is closed would require a separate tray or operating-system startup
integration.

## Windows support

- The app uses native user-profile and local-cache directories and never
  constructs paths with a hard-coded separator.
- Git is not required for the built-in source. Custom sources require
  `git.exe` to be installed and available on `PATH`.
- Custom-source Git commands are executed directly rather than through a shell.
  HTTPS credentials and SSH keys remain managed by the user's existing Git and
  SSH configuration.
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

## Deliberate non-goals

- No branch, tag, commit, or skill-version selector.
- No authentication UI, token storage, SSH key management, or credential
  storage.
- No source priority or automatic duplicate-name resolution.
- No automatic installation of newly discovered skills.
- No background refresh while the app is closed.
- No repository layout other than `skills/<name>/SKILL.md`.
- No telemetry.

## Repository shape

```text
src/                  React GUI
src-tauri/            Source, cache, GitHub/Git, and install logic
```

## Next likely steps

1. Package signed installers for Windows, macOS, and Linux.
