# Source manifest reference

Every source repository contains `skill-manager.json` at its root. Unknown fields are rejected. The locally pinned generated schema is available at [`schemas/v2/source-manifest.schema.json`](../schemas/v2/source-manifest.schema.json); the v1 URL remains published for existing validators.

## Shared source object

Both versions require:

| Field                | Rules                                                                         |
| -------------------- | ----------------------------------------------------------------------------- |
| `source.id`          | 2–16 lowercase ASCII letters, digits, or single hyphens; starts with a letter |
| `source.name`        | 1–120 characters                                                              |
| `source.description` | 1–1,024 characters                                                            |

`source.id` namespaces catalog identities as `source-id/package-id`. Skill Manager separately derives `sourceKey` from the canonical repository URL for cache and ownership authority.

## Manifest v2 packages

Version 2 is the portable multi-agent contract:

```json
{
  "version": 2,
  "source": { "id": "acme", "name": "Acme", "description": "Shared engineering workflows." },
  "packages": [
    {
      "id": "review",
      "name": "Review workflow",
      "description": "A skill and its always-on review policy.",
      "components": [
        { "kind": "skill", "id": "review", "path": "skills/review" },
        { "kind": "instructionSet", "id": "review-rules", "path": "rules/review.md", "activation": "always", "topics": ["review"] }
      ],
      "conflictsWith": ["other-source/old-review"]
    }
  ]
}
```

A package is the atomic, user-facing install unit. Its `id` is 1–64 lowercase letters, digits, and single hyphens. `name` and `description` are optional display overrides. A package declares either `components` or both `format` and `path`, never both.

When a package contains several components, every component needs a unique package-local `id`. A single component may omit it and inherit the package ID. Component paths are repository-relative regular files/directories and must pass the same containment, portability, symlink, and size checks as v1.

### Skill component

```json
{ "kind": "skill", "id": "review", "path": "skills/review" }
```

`path` is a directory containing `SKILL.md`. Its frontmatter `name` must match the component ID. Skill Manager materializes the installed name as `source-id-component-id` while preserving other frontmatter, Markdown, nested assets, and executable bits.

Cursor, Codex, OpenCode, and GitHub Copilot can co-consume one directory under `~/.agents/skills`. Claude Code uses `~/.claude/skills`; Grok Build uses `~/.grok/skills`.

### MCP server component

```json
{ "kind": "mcpServer", "id": "database", "path": "mcp/database.json" }
```

The referenced document is a pinned Agent Plugins 1.0.0 `mcp.json` object with one or more `mcpServers`. Supported transports are `stdio`, `streamable-http`, and `sse`; a target adapter may report a transport unsupported for its dialect.

Remote URLs require HTTPS, except localhost loopback. Sensitive headers such as `Authorization` and `X-API-Key` must use an environment reference such as `${ACME_TOKEN}`. Tier 3 install review shows commands or URLs, arguments, working directories, environment-variable names, and header names. Skill Manager writes configuration but never starts the server.

### Instruction-set component

```json
{ "kind": "instructionSet", "id": "review-rules", "path": "rules/review.md", "activation": "always", "topics": ["review"] }
```

Only `activation: "always"` and user scope are supported. Content must be non-empty Markdown no larger than 256 KB. Topics produce advisory overlap warnings; `conflictsWith` is the mechanical hard-conflict mechanism.

Claude Code, Codex, and OpenCode receive exact marked blocks in their documented monolithic user instructions file. Uninstall removes only the matching marker/body. Cursor, Grok Build, and Copilot are reported unsupported where the pinned dialect has no documented writable user-scope mapping.

### Portable Agent Plugin package

```json
{ "id": "data-tools", "format": "agent-plugin@1.0.0", "path": "plugins/data-tools" }
```

The package root must contain a pinned Agent Plugins 1.0.0 `plugin.json`; optional `skills/` and `mcp.json` content is validated locally. `${PLUGIN_ROOT}` and `${PLUGIN_DATA}` placeholders expand to final runtime paths.

Cursor receives the package at `~/.cursor/plugins/local/<source-id>-<package-id>`. GitHub Copilot CLI receives it at `~/.copilot/installed-plugins/_direct/<source-id>-<package-id>`. Skill Manager does not edit Copilot settings and does not create plugin-data directories. Other targets receive lossless skill/MCP projections where supported.

### Explicit package conflicts

`conflictsWith` contains canonical `source-id/package-id` strings. Installation is blocked when a listed package is installed or when two requested batch packages list one another. Dependencies and version solving are not part of v2.

## Manifest v1 generic installs

Version 1 remains supported for explicit generic file trees:

```json
{
  "version": 1,
  "source": { "id": "acme", "name": "Acme", "description": "Machine configuration." },
  "installs": [{ "id": "templates", "source": "templates", "destination": "~/.config/acme/templates" }]
}
```

Each install maps one regular repository file/directory to a non-root absolute destination or a home-relative `~/` destination. Bare relative paths, `.`/`..`, Windows-reserved names, trailing spaces/periods, non-portable characters, overlapping roots, and Skill Manager state directories are rejected.

V1 content is not silently reinterpreted as portable multi-agent configuration. Existing Agent Skill and Agent Plugin recognition remains behavior-compatible, but every primary and auxiliary path now runs through the central resource transaction and ledger.

## Repository and operation limits

The manifest is limited to 1 MB. A snapshot may contain at most 2,000 files and 50 MB of selected content. Symlinks, special entries, case-insensitive collisions, and paths outside the repository are rejected.

Install and update preflight every physical identity. Identical desired content is coalesced; different content at one path/key/marker is a hard conflict. Local drift blocks automatic update and normal uninstall. Explicit replacement or force removal makes a persistent backup first. Unrelated keys/comments in shared JSONC/TOML documents and unrelated text outside managed markers remain user-owned.
