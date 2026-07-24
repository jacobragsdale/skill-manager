# Skill Manager

A small cross-platform desktop app for installing and maintaining reusable
agent configuration from Git repositories.

- **Skills** are model-invoked capabilities installed under
  `~/.agents/skills/`.
- **Rules** are always-on instructions installed as managed sections in the
  user-wide Codex `~/.codex/AGENTS.md`.
- **Bundles** are named selections of skills and rules. They are catalog
  conveniences, not dependency or ownership containers.

The app includes
[`jacobragsdale/skillbook`](https://github.com/jacobragsdale/skillbook) on
first launch. Users can remove it, keep an empty source list, add it again
explicitly, and add other HTTPS or SSH Git sources.

## What works

- Commit-pinned, source-aware caches keep validated catalogs available offline.
- Every source may publish skills, rules, bundles, or any useful combination.
- Invalid items are reported beside their source without hiding other valid
  content. A source is rejected only when it has no valid skill or rule.
- Skills and rules remain individually installable even when they belong to a
  bundle.
- A source-level **Install all** covers every skill and rule, whether or not the
  source publishes bundles.
- Bundle and source bulk actions show the complete member plan first. Any
  adoption, replacement, modification, or source conflict blocks the entire
  bulk operation until it is resolved individually.
- Bundle status is derived as available, partially installed, installed, update
  available, or needs attention.
- Automatic checks update only existing, unmodified managed skills and rules.
  Newly discovered items and new bundle members are never installed
  automatically.
- Removing a source never uninstalls its managed content. Orphaned skills and
  rules remain visible for protected uninstall.
- Closing the window keeps scheduled checks running from the macOS menu bar or
  Windows notification area.

## Source repository contract

Every top-level directory is optional:

```text
skills/
  python-standards/
    SKILL.md
    ...optional resources

rules/
  python.md

bundles/
  python-development.yaml
```

### Skills

Each immediate `skills/` child is one Agent Skill. Its directory name must
match the `name` in `SKILL.md` frontmatter.

### Rules

Rules are standalone Markdown files:

```markdown
---
name: python
description: Always-on constraints for high-integrity Python work.
---

# Python rules

...
```

The filename, without `.md`, must match `name`. Rules do not have scripts or
install hooks.

### Bundles

Bundles are standalone `.yaml` files:

```yaml
name: python-development
description: Python standards and always-on rules.
skills:
  - python-standards
rules:
  - python
```

The filename must match `name`. A bundle must contain at least one member, may
not duplicate members, and may reference only valid skills and rules from the
same source commit. Nested and cross-source bundles are not supported.

## Codex rule installation

The first rule target is Codex at user-wide scope. Codex loads global
instructions from `~/.codex/AGENTS.md` once per run or TUI session, so rule
changes take effect in a new run or session.

Skill Manager:

- renders only the rule instruction body into an explicitly bounded managed
  section;
- stores source ID, source URL, source commit, canonical content digest,
  installed-section digest, target, and scope in sidecar ownership metadata;
- preserves unrelated `AGENTS.md` bytes through install, update, replacement,
  and uninstall;
- backs up the full instruction file before replacement;
- protects locally modified managed sections; and
- warns when a non-empty `~/.codex/AGENTS.override.md` is masking the managed
  global file.

An existing `AGENTS.md` does not need to be owned by Skill Manager. The app
adds and edits only its own bounded sections. Matching unowned sections may be
adopted; differing bounded sections require explicit replacement.

## Install, conflict, and update behavior

Skills retain directory ownership markers under their install directories.
Rules retain sidecar markers under `~/.codex/.skill-manager/rules/`. Both kinds
record source ownership and content digests.

- **Install** writes a staged managed copy.
- **Manage** adopts an exact unmanaged match without replacing its content.
- **Replace…** requires confirmation and keeps a recoverable backup.
- **Update** replaces only a managed item whose installed digest still matches
  its marker.
- **Uninstall** removes only an unmodified item owned by the requested source.

Same-named skills conflict only with skills, and same-named rules conflict only
with rules. Skill and rule identities are explicit, so `skill:python` and
`rule:python` may coexist.

Bulk execution does not attempt cross-item rollback. If an unexpected member
failure occurs after preflight, completed members remain managed, failures are
reported, and retry is safe.

## Sources and caches

The default `skillbook` source uses the GitHub HTTPS API and immutable commit
archives, so it does not require Git or GitHub authentication. Custom sources
use the system Git executable, follow each repository's default branch, and use
the user's existing HTTPS credential helper or SSH configuration.

Custom refreshes use a shallow blob-filtered sparse checkout of `skills/`,
`rules/`, and `bundles/`. Catalog copies are capped at 2,000 files and 50 MB.
Built-in downloads are capped before and during extraction. Paths are validated
for Windows portability and case-insensitive collisions.

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
- UTF-8 BOM and CRLF metadata are accepted; non-metadata assets remain
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
- No rule templating, variables, conditional evaluation, or secret injection.
- No dependency solver, version constraints, lockfile, or bundle reference
  counting.
- No nested or cross-source bundles.
- No project-scoped or second-agent rule target yet.
- No automatic install of new items or automatic uninstall of removed items.
- No silent edits to unmanaged instructions.
- No authentication UI, credential storage, telemetry, or source priority.
