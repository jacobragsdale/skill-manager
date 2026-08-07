# Skill Manager

Skill Manager is a small cross-platform desktop app for installing and maintaining Agent Skills and other per-user configuration from trusted Git sources.

Every source publishes a required top-level `skill-manager.json`. Its short namespace identifies catalog items, while a separate URL-derived source key secures caches, ownership, and executable trust.

## Capabilities

- Materializes Agent Skills as `source-id-local-name` in both their installed directory and `SKILL.md` frontmatter.
- Installs explicit generic items and path-template collections beneath approved per-user directories.
- Keeps validated commits as immutable offline snapshots and never auto-installs newly published items.
- Tracks ownership and installed digests in an atomic ledger; modified content remains protected during normal updates and uninstall.
- Supports ordered lifecycle hooks plus explicit source and item actions after repository-bound executable trust.
- Plans destructive source cleanup, including path-level warnings, before removing the source, namespace claim, trust, and cache.
- Runs scheduled checks while the window is hidden in the macOS menu bar or Windows notification area.

## Documentation

- [Publish a source](docs/publish-source.md) — create, validate, and test a manifest-backed repository.
- [Source manifest v1 reference](docs/manifest-reference.md) — exact fields, templates, identities, limits, and command environment.
- [Namespace migration](docs/namespace-migration.md) — understand and resolve the move to prefixed Agent Skill names.
- [Executable trust](docs/executable-trust.md) — security model, approvals, revocation, logs, and cleanup behavior.
- [Backend architecture](docs/architecture.md) — acquisition, normalization, ownership, trust, and IPC boundaries.
- [Roadmap](ROADMAP.md) — shipped foundation, invariants, and possible next work.

The published JSON Schema is [`schemas/v1/source-manifest.schema.json`](schemas/v1/source-manifest.schema.json).

## Development

Requirements: Rust, Node.js, pnpm, and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
pnpm install --frozen-lockfile
pnpm tauri dev
```

Run the same local gates as CI:

```bash
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## Boundaries

Version 1 supports current-user destinations only. Workspace and absolute destinations, elevated or interactive commands, dependency solving, sandboxed execution, automatic installation of new items, and automatic uninstall of removed upstream items are outside its scope.
