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
  management recovery, replacement, modification, or source conflict blocks
  the entire bulk operation until it is resolved.
- Existing skill directories, damaged ownership markers, and symlinks are
  classified automatically. When at least two have an unambiguous safe path,
  **Review & Manage All** presents one recovery plan for confirmation.
- Bundle status is derived as available, partially installed, installed, update
  available, or needs attention.
- Automatic checks update only existing, unmodified managed skills. Newly
  discovered skills and new bundle members are never installed automatically.
- Removing a source never uninstalls its managed skills. Orphaned skills remain
  visible for protected uninstall.
- Closing the window keeps scheduled checks running from the macOS menu bar or
  Windows notification area.

## Source repository contract

A source is a Git repository with one or more skills under a top-level
`skills/` directory. It may also publish optional bundles under a top-level
`bundles/` directory:

```text
skills/
  python-standards/
    SKILL.md
    ...optional resources
  git-ops/
    SKILL.md

bundles/
  python-development.yaml
```

### Skills

Each immediate child of `skills/` is one Agent Skill. The directory name:

- must contain only lowercase ASCII letters, digits, and single hyphens;
- may not start or end with a hyphen; and
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
`.skill-manager-managed` file at its root. All repository paths must also be
portable to Windows.

### Bundles

Bundles are optional groupings; they do not prevent their member skills from
being installed individually. Each bundle is a standalone `.yaml` file directly
under `bundles/`:

```yaml
name: python-development
description: Skills for Python development.
skills:
  - python-standards
  - git-ops
```

A bundle manifest supports exactly three fields:

- `name`: required, follows the same naming rules as a skill, and matches the
  filename (`python-development.yaml`);
- `description`: required and non-empty; and
- `skills`: a non-empty list of unique skill names.

Every listed skill must be valid and present in the same source commit. A skill
may belong to at most one bundle within a source. Only standalone `.yaml` files
are accepted; nested, overlapping, and cross-source bundles are not supported.

A source must contain at least one valid skill. Invalid skill or bundle entries
are reported in the app, while other valid entries from the same source remain
available.

## Install, conflict, and update behavior

Each installed skill contains a source-aware ownership marker.

- **Install** writes a staged managed copy.
- **Manage** adds management data to an exact unmanaged directory without
  replacing its content.
- **Repair** replaces damaged or legacy management data for its known source.
  Skill files are preserved; if they differ from the catalog, the repaired
  skill becomes **Local Changes**.
- **Migrate…** handles an exact skill symlink by backing up the link under
  `~/.agents/.skill-manager-backups/` and installing a managed directory copy.
  The external link target is never modified.
- **Replace…** requires confirmation and keeps a recoverable backup.
- **Update** replaces only a managed skill whose installed digest still matches
  its marker.
- **Uninstall** removes only an unmodified skill owned by the requested source.

Recovery is conservative. A missing or unidentifiable marker is managed in
place only when the directory exactly matches one current catalog skill. A
differing directory or symlink remains a conflict, and an exact match offered by
multiple sources is omitted from the global plan so the user can choose its
source on the individual skill card. Automatic updates never perform recovery.

Recovery and bulk execution do not attempt cross-skill rollback. Every recovery
entry is checked again immediately before it changes, completed skills remain
managed if another entry fails, failures are reported, and retry is safe.

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
