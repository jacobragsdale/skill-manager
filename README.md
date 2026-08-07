# Skill Manager

A small cross-platform desktop app for installing and maintaining Agent Skills
from Git repositories. Skills are installed for the current user under
`~/.agents/skills/`.

The app includes
[`jacobragsdale/skillbook`](https://github.com/jacobragsdale/skillbook) on
first launch. Users can remove it, keep an empty source list, add it again
explicitly, and add other HTTPS or SSH Git sources.

## What works

- Commit-pinned, source-aware caches keep validated skill catalogs available
  offline.
- Invalid skills are reported beside their source without hiding other valid
  skills. A source is rejected only when it has no valid skills.
- Every catalog skill remains visible and independently installable.
- Source-level **Install All** and **Uninstall All** show the complete plan
  before changing any skill.
- A manual adoption, replacement, local modification, or source conflict blocks
  a source-wide operation until the affected skill is handled individually.
- Automatic checks update only existing, unmodified managed skills. Newly
  discovered skills are never installed automatically.
- Removing a source never uninstalls its managed skills. Orphaned skills remain
  visible for protected uninstall.
- Closing the window keeps scheduled checks running from the macOS menu bar or
  Windows notification area.

## Source repository contract

A source is a Git repository with one or more skills under a top-level
`skills/` directory:

```text
skills/
  python-standards/
    SKILL.md
    ...optional resources
  git-ops/
    SKILL.md
```

Each immediate child of `skills/` is one Agent Skill. The directory name:

- must contain only lowercase ASCII letters, digits, and single hyphens;
- may not start or end with a hyphen;
- may not use a Windows reserved name; and
- must match the skill's frontmatter `name`.

Every skill requires a UTF-8 `SKILL.md` whose first content is frontmatter with
non-empty `name` and `description` fields:

```markdown
---
name: python-standards
description: Standards for high-integrity Python development.
---

# Python standards

Instructions for the agent go here.
```

Other files and subdirectories beside `SKILL.md` are installed as part of the
skill. A source skill must not contain Skill Manager's reserved
`.skill-manager-managed` file at its root. All repository paths must be
portable to Windows.

Only `skills/` is part of the source contract. A `bundles/` directory or
other repository metadata is ignored and has no effect on catalog entries,
installation, or source-wide actions.

## Install, conflict, and update behavior

Each installed skill contains a source-aware ownership marker.

- **Install** writes a staged managed copy.
- **Manage** adopts an exact unmanaged directory match by writing the ownership
  marker without replacing skill content.
- **Replace…** is available for differing unmanaged content. It requires
  confirmation, stages the catalog copy, backs up the original under
  `~/.agents/.skill-manager-backups/<name>/<timestamp>`, and restores the
  original if activation fails.
- **Update** replaces only a managed skill whose installed digest still matches
  its marker.
- **Uninstall** removes only an unmodified skill owned by the requested source.

Unmanaged symlinks and differing unmanaged directories are never adopted or
replaced automatically. Existing ownership markers and backups remain valid.

Bulk execution does not attempt cross-skill rollback. If an unexpected failure
occurs after preflight, completed skills remain managed, failures are reported,
and retry is safe.

## Sources and caches

The default `skillbook` source uses the GitHub HTTPS API and immutable commit
archives, so it does not require Git or GitHub authentication. Custom sources
use the system Git executable, follow each repository's default branch, and use
the user's existing HTTPS credential helper or SSH configuration.

Custom refreshes use a shallow blob-filtered sparse checkout of `skills/`.
Catalog copies are capped at 2,000 files and 50 MB. Built-in downloads are
capped before and during extraction. Paths are validated for Windows
portability and case-insensitive collisions.

Source configuration distinguishes an uninitialized install from an explicitly
saved empty list:

- first launch seeds `skillbook`;
- upgrading the earlier custom-source format adds the previously implicit
  default source once;
- removing every source persists an empty list across restart; and
- **Add Default Skillbook Source** opts back in explicitly.

## Architecture

The backend remains one Rust crate split into focused private modules. See
[Backend architecture](docs/architecture.md) for module ownership, lock and
mutation boundaries, and the extension points reserved for an optional future
source manifest.

## Development

Requirements:

- Rust
- Node.js
- pnpm
- [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/)

Run the app:

```bash
pnpm install
pnpm tauri dev
```

Run the checks:

```bash
pnpm install --frozen-lockfile
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## Deliberate non-goals

- No bundles, dependency solver, version constraints, or lockfile.
- No install scripts or lifecycle hooks.
- No automatic install of new skills or automatic uninstall of removed skills.
- No automatic adoption or replacement of unmanaged skill entries.
- No authentication UI, credential storage, telemetry, or source priority.
