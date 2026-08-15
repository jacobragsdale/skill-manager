# Skill Manager

Skill Manager is a desktop desired-state manager for Agent Skills and MCP servers across Cursor, Claude Code, Codex, OpenCode, Grok Build, and GitHub Copilot CLI.

Sources publish a top-level `skill-manager.json` as an HTTPS archive. A source repository is a catalog that lists those sources by name and description; packages appear only after a listed source is added. The baked-in catalog is `https://repo.ragsdale.dev/repository/files/catalogs/skill-manager-repository.json`. Manifest v2 describes portable packages of skills and MCP servers without agent-specific destinations; enabled agent profiles determine the projections.

Skill Manager plans all target resources, coalesces shared paths, previews compatibility and trust, and applies each requested operation through one recovery journal and ownership-ledger commit. It never executes source content.

## Documentation

- [Publish a source](docs/publish-source.md) — tutorial for a portable v2 package, including a zip publish path.
- [Publish a source repository](docs/publish-source-repository.md) — tutorial for a browseable catalog.
- [Manifest reference](docs/manifest-reference.md) — v1/v2 fields and validation.
- [Source repository reference](docs/source-repository-reference.md) — catalog document, locators, and identity.
- [Architecture](docs/architecture.md) — desired resources, transactions, ownership, and trust.
- [Target adapter contract](docs/adapter-contract.md) — pinned target mappings and acceptance criteria.
- [Native extension evaluation](docs/native-extensions-evaluation.md) — Tier 4 decisions and admission requirements.
- [ADR 0001](docs/decisions/0001-multi-agent-desired-state.md) — product and migration decisions.
- [ADR 0002](docs/decisions/0002-source-repositories-and-locators.md) — catalogs and locators.
- [ADR 0003](docs/decisions/0003-artifact-only-catalog.md) — artifact-only distribution and the baked-in catalog.

## Development

Requirements: Rust, Node.js, pnpm, and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
pnpm install
pnpm tauri dev
```

Run local verification:

```bash
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml --all-targets
```

Regenerate the checked-in source and source-repository schema paths after changing the Rust contract:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --bin generate-schema
```
