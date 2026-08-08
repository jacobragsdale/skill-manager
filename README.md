# Skill Manager

Skill Manager is a small desktop app that installs files and directories from Git repositories into explicit per-user destinations.

Each source has one top-level `skill-manager.json`. Every entry maps exactly one repository file or directory to one destination:

```json
{
  "version": 1,
  "source": { "id": "acme", "name": "Acme tools", "description": "Shared agent configuration." },
  "installs": [{ "id": "review", "source": "skills/review", "destination": "~/.agents/skills/acme-review" }]
}
```

Directories are copied recursively, including bundled scripts and executable permission bits. Skill Manager does not execute source content.

## Documentation

- [Publish a source](docs/publish-source.md) — create and test a repository.
- [Manifest reference](docs/manifest-reference.md) — field rules, destinations, and Agent Skill handling.
- [Architecture](docs/architecture.md) — source identity, snapshots, ownership, and transactions.
- [Roadmap](ROADMAP.md) — current boundaries and possible next work.

## Development

Requirements: Rust, Node.js, pnpm, system Git, and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
pnpm install
pnpm tauri dev
```

Local checks:

```bash
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml --all-targets
```

Regenerate the checked-in schema after changing the Rust manifest types:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --bin generate-schema
```
