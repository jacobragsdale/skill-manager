# Publish a portable multi-agent source

This tutorial publishes one Agent Skill and one MCP server as a manifest v2 package. Agent Plugins will project the package across the agents a user explicitly enables.

## Create the repository

Use this layout:

```text
example-source/
├── skill-manager.json
├── mcp/
│   └── database.json
└── skills/
    └── review/
        ├── SKILL.md
        └── scripts/
            └── check.sh
```

Create `skills/review/SKILL.md`:

```markdown
---
name: review
description: Reviews a change before it is submitted.
disable-model-invocation: true
---

# Review

Follow the repository's review workflow.
```

The skill name must match its component ID. Agent Plugins will install it as `example-review`, preserving all other frontmatter and files. A bundled script remains ordinary content: the app never executes it, although a target agent may invoke it later. For every `SKILL.md` field, see [the source manifest reference](manifest-reference.md#skillmd).

Create `mcp/database.json`:

```json
{ "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json", "mcpServers": { "database": { "type": "stdio", "command": "npx", "args": ["@acme/database-mcp"] } } }
```

The stdio command must be a bare executable on `PATH`. Do not use `./bin/server` or `${PLUGIN_ROOT}` / `${PLUGIN_DATA}` placeholders. Never embed a secret in a sensitive header; reference an environment variable such as `${ACME_TOKEN}`. For every MCP field, see [the source manifest reference](manifest-reference.md#mcp-server-component).

## Declare the package

Create the root source manifest, `skill-manager.json`:

```json
{
  "version": 2,
  "source": { "id": "example", "name": "Example workflows", "description": "Portable review configuration." },
  "packages": [
    {
      "id": "review",
      "name": "Review workflow",
      "description": "Review skill and database MCP server.",
      "components": [
        { "kind": "skill", "id": "review", "path": "skills/review" },
        { "kind": "mcpServer", "id": "database", "path": "mcp/database.json" }
      ]
    }
  ]
}
```

One package can bundle several skills and MCP servers. Users can install the package as a whole or expand it and manage each component separately. Unsupported target/component pairs remain visible and are skipped during planning; any failure among accepted resources rolls back the entire requested operation.

## Validate locally

Validate the JSON shape against the checked-in schema:

```bash
jq empty skill-manager.json
npx ajv-cli validate --spec=draft2020 --strict=false \
  -s https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v2/source-manifest.schema.json \
  -d skill-manager.json
```

Then run the repository-aware validator for source containment, component names, MCP shape, portability, symlinks, and repository limits:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source -- /path/to/example-source
```

You can also validate a published HTTPS archive of the same tree:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source -- \
  https://nexus.example.com/repository/raw/sources/example-latest.zip
```

## Publish a zip of the same tree

A source artifact is a zip, tar, or tar.gz of the source tree: `skill-manager.json` at the root, plus every referenced path. If the archive contains exactly one top-level directory and no sibling files, Agent Plugins treats that directory as the source root.

Host the archive at a stable HTTPS URL that you overwrite when you want users to receive a new snapshot, for example `…/example-latest.zip`. Agent Plugins always fetches that URL; it does not pin versions and does not send credentials.

## Publish and verify

Upload the archive to the HTTPS URL and list it in a [source repository](publish-source-repository.md). In Agent Plugins:

1. Open **Detected Agents** and confirm the agents this machine found.
2. Open **Manage Sources**. Select **Add** on the listed source.
3. Confirm. Packages become available; nothing is installed yet.
4. Select **Install** on the package.
5. Review every target capability and physical resource. Shared `~/.agents/skills` projections should appear once with several detected consumers. Claude Code adds a separate `~/.claude/skills` copy. MCP install is Tier 3; clicking Install is the approval.
6. Confirm the transaction and inspect the target after its documented reload boundary.

Static file presence proves the desired state was written, not that an agent loaded it. For runtime evidence, use the target's own skill/config inspection surface in a disposable home and record the target version.

## Portable concepts only

V2 has no generic file-tree component. Publish skills and MCP servers; do not map repository paths to machine-specific destinations. For every field rule, see [the source manifest reference](manifest-reference.md).
