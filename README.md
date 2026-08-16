# Agent Plugins

Agent Plugins is a desktop app that installs portable Agent Skills and MCP servers from published sources onto the coding agents you enable: Cursor, Claude Code, Codex, OpenCode, Grok Build, and GitHub Copilot CLI.

A **source** is an HTTPS archive with `skill-manager.json` at its root. That file is the source manifest: it names the source and lists packages of skills and MCP servers. A **source repository** is a separate catalog of those archives. Packages appear only after you add a listed source. The baked-in catalog is `https://repo.ragsdale.dev/repository/files/catalogs/skill-manager-repository.json`.

The app plans the files and config each enabled agent needs, shows compatibility and trust, then applies the change in one recovery journal and ownership-ledger commit. It never executes source content.

## Learn

- [Publish a source](docs/publish-source.md) — write a portable package and publish it as a zip.
- [Publish a source repository](docs/publish-source-repository.md) — publish a browseable catalog.

## Look up

- [Source manifest](docs/manifest-reference.md) — `skill-manager.json`, `SKILL.md`, and MCP document fields.
- [Source repository](docs/source-repository-reference.md) — catalog document, locators, and identity.
- [Target adapter contract](docs/adapter-contract.md) — pinned target mappings.

## Understand

- [Architecture](docs/architecture.md) — desired resources, transactions, ownership, and trust.
- [Native extension evaluation](docs/native-extensions-evaluation.md) — why hooks and in-process plugins stay out of the portable contract.
- [ADR 0001](docs/decisions/0001-multi-agent-desired-state.md) — product and migration decisions.
- [ADR 0002](docs/decisions/0002-source-repositories-and-locators.md) — catalogs and locators.
- [ADR 0003](docs/decisions/0003-artifact-only-catalog.md) — artifact-only distribution and the baked-in catalog.

## Develop

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
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets
cargo test --manifest-path src-tauri/Cargo.toml --all-targets
```

Regenerate the checked-in source and source-repository schema paths after changing the Rust contract:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --bin generate-schema
```
