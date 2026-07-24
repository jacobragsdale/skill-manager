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
- Invalid skills or bundles are reported beside their source without hiding
  other valid skills. A source is rejected only when it has no valid skills.
- Skills remain individually installable when they belong to a bundle.
- The catalog renders every skill once, inside its bundle group or as an
  individual skill.
- A source-level **Install All** covers every skill, whether or not the source
  publishes bundles.
- Bundle and source bulk actions show the complete skill plan first. Any
  adoption, replacement, modification, or source conflict blocks the entire
  bulk operation until it is resolved individually.
- Bundle status is derived as available, partially installed, installed, update
  available, or needs attention.
- Automatic checks update only existing, unmodified managed skills. Newly
  discovered skills and new bundle members are never installed automatically.
- Removing a source never uninstalls its managed skills. Orphaned skills remain
  visible for protected uninstall.
- Closing the window keeps scheduled checks running from the macOS menu bar or
  Windows notification area.

## Source repository contract

A source contains a `skills/` directory and may include `bundles/`:

```text
skills/
  python-standards/
    SKILL.md
    ...optional resources

bundles/
  python-development.yaml
```

Each immediate `skills/` child is one Agent Skill. Its directory name must
match the `name` in `SKILL.md` frontmatter.

Bundles are standalone `.yaml` files:

```yaml
name: python-development
description: Skills for Python development.
skills:
  - python-standards
  - git-ops
```

The filename must match `name`. A bundle must contain at least one skill, may
not list a skill twice, and may reference only valid skills from the same source
commit. A skill may belong to at most one bundle within a source. Nested,
overlapping, and cross-source bundles are not supported.

## Install, conflict, and update behavior

Each installed skill contains a source-aware ownership marker.

- **Install** writes a staged managed copy.
- **Manage** adopts an exact unmanaged match without replacing its content.
- **Replace…** requires confirmation and keeps a recoverable backup.
- **Update** replaces only a managed skill whose installed digest still matches
  its marker.
- **Uninstall** removes only an unmodified skill owned by the requested source.

Bulk execution does not attempt cross-skill rollback. If an unexpected failure
occurs after preflight, completed skills remain managed, failures are reported,
and retry is safe.

## Sources and caches

The default `skillbook` source uses the GitHub HTTPS API and immutable commit
archives, so it does not require Git or GitHub authentication. Custom sources
use the system Git executable, follow each repository's default branch, and use
the user's existing HTTPS credential helper or SSH configuration.

Custom refreshes use a shallow blob-filtered sparse checkout of `skills/` and
`bundles/`. Catalog copies are capped at 2,000 files and 50 MB. Built-in
downloads are capped before and during extraction. Paths are validated for
Windows portability and case-insensitive collisions.

Source configuration distinguishes an uninitialized install from an explicitly
saved empty list:

- first launch seeds `skillbook`;
- upgrading the earlier custom-source format adds the previously implicit
  default source once;
- removing every source persists an empty list across restart; and
- **Add default skillbook source** opts back in explicitly.

## Windows support

- Native profile, config, and cache directories are used without hard-coded
  separators.
- Git commands are executed directly without a shell.
- UTF-8 BOM and CRLF metadata are accepted; other skill assets remain
  byte-opaque.
- Archive and Git catalog paths reject reserved device names, illegal or
  trailing components, overlong UTF-16 components, and case-insensitive
  collisions.
- CI runs the complete frontend and Rust suite natively on Windows and Linux.

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

- No install scripts or lifecycle hooks.
- No dependency solver, version constraints, lockfile, or bundle reference
  counting.
- No nested or cross-source bundles.
- No automatic install of new skills or automatic uninstall of removed skills.
- No silent edits to unmanaged skill directories.
- No authentication UI, credential storage, telemetry, or source priority.
