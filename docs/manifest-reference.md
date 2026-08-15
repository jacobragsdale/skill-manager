# Source manifest reference

The source manifest is the file named `skill-manager.json` at the root of a source archive. Agent Plugins reads that file and nothing else to learn what the source publishes. A source without this file is rejected.

The locally pinned generated schema is [`schemas/v2/source-manifest.schema.json`](../schemas/v2/source-manifest.schema.json). Unknown fields are rejected. Version 1 generic file installs are rejected.

A **source repository** is a separate catalog document (`skill-manager-repository.json` or a raw JSON URL). It lists source locators and is not installable. See [the source-repository reference](source-repository-reference.md).

## Object model

```text
skill-manager.json
├── version                     2
├── source                      who published this tree
│   ├── id                      namespace for package and install names
│   ├── name
│   └── description
└── packages[]                  install units shown in the catalog
    ├── id                      unique inside this source
    ├── name                    optional display override
    ├── description             optional display override
    ├── components[]            skills and MCP servers in this package
    │   ├── kind                skill | mcpServer
    │   ├── id                  required when the package has several
    │   └── path                source-relative file or directory
    └── conflictsWith[]         other source-id/package-id strings
```

See [the object diagram](diagrams/source-manifest.mmd).

A package is the atomic, user-facing install unit. Agent Plugins can install the whole package or, when the package has several components, each skill or MCP server on its own.

## Document

| Field      | Rules                                                              |
| ---------- | ------------------------------------------------------------------ |
| `version`  | Required integer. Must be `2`.                                     |
| `source`   | Required object. See [Source](#source).                            |
| `packages` | Required array of one or more packages. See [Packages](#packages). |

The document is limited to 1 MB.

```json
{
  "version": 2,
  "source": { "id": "acme", "name": "Acme", "description": "Shared engineering workflows." },
  "packages": [
    {
      "id": "review",
      "name": "Review workflow",
      "description": "A review skill and database MCP server.",
      "components": [
        { "kind": "skill", "id": "review", "path": "skills/review" },
        { "kind": "mcpServer", "id": "database", "path": "mcp/database.json" }
      ],
      "conflictsWith": ["other-source/old-review"]
    }
  ]
}
```

## Source

| Field                | Rules                                                                         |
| -------------------- | ----------------------------------------------------------------------------- |
| `source.id`          | 2–16 lowercase ASCII letters, digits, or single hyphens; starts with a letter |
| `source.name`        | 1–120 characters                                                              |
| `source.description` | 1–1,024 characters                                                            |

`source.id` namespaces catalog identities as `source-id/package-id` and prefixes installed skill names as `source-id-component-id`.

Agent Plugins separately derives `sourceKey` from the archive URL for cache and ownership authority. That key is the SHA-256 of `artifact:` plus the canonical HTTPS URL, rendered as `source-` plus 16 hex characters. Changing `source.id` does not transfer cache or installation ownership. Two URLs are two sources even when they serve the same bytes.

## Packages

| Field                      | Rules                                                                                                    |
| -------------------------- | -------------------------------------------------------------------------------------------------------- |
| `packages[].id`            | 1–64 lowercase letters, digits, and single hyphens. Unique inside the source.                            |
| `packages[].name`          | Optional display override, 1–120 characters.                                                             |
| `packages[].description`   | Optional display override, 1–1,024 characters.                                                           |
| `packages[].components`    | One or more `skill` or `mcpServer` components.                                                           |
| `packages[].conflictsWith` | Optional list of canonical `source-id/package-id` strings. See [Conflicts](#explicit-package-conflicts). |

When a package contains several components, every component needs a unique package-local `id`. A single component may omit `id` and inherit the package ID.

Component paths are source-relative regular files or directories. They must pass containment, portability, symlink, and size checks.

### Skill component

```json
{ "kind": "skill", "id": "review", "path": "skills/review" }
```

| Field  | Rules                                                                 |
| ------ | --------------------------------------------------------------------- |
| `kind` | `skill`                                                               |
| `id`   | Package-local component ID. Optional when this is the only component. |
| `path` | Directory that contains `SKILL.md`.                                   |

The `SKILL.md` frontmatter `name` must match the component ID. Agent Plugins materializes the installed name as `source-id-component-id` and preserves other frontmatter, Markdown, nested assets, and executable bits.

Cursor, Codex, OpenCode, Grok Build, and GitHub Copilot can co-consume one directory under `~/.agents/skills`. Claude Code uses `~/.claude/skills`.

### MCP server component

```json
{ "kind": "mcpServer", "id": "database", "path": "mcp/database.json" }
```

| Field  | Rules                                                                 |
| ------ | --------------------------------------------------------------------- |
| `kind` | `mcpServer`                                                           |
| `id`   | Package-local component ID. Optional when this is the only component. |
| `path` | Source-relative MCP document.                                         |

The referenced document uses the closed Agent Plugins 1.0.0 `mcp.json` shape: a `$schema` identifier and one or more `mcpServers`. Supported transports are `stdio`, `streamable-http`, and `sse`; a target adapter may report a transport unsupported for its dialect. That schema is a portable MCP document, not a native plugin tree.

A stdio `command` must be a bare executable on `PATH`. Package-relative commands such as `./bin/server` and `${PLUGIN_ROOT}` / `${PLUGIN_DATA}` placeholders are rejected.

Remote URLs require HTTPS, except localhost loopback. Sensitive headers such as `Authorization` and `X-API-Key` must use an environment reference such as `${ACME_TOKEN}`. Agent Plugins writes configuration but never starts the server.

## Explicit package conflicts

`conflictsWith` contains canonical `source-id/package-id` strings. Installation is blocked when a listed package is installed or when two requested batch packages list one another. Dependencies and version solving are not part of v2.

## Rejected shapes

These documents fail validation:

- missing `skill-manager.json`, or a document larger than 1 MB
- `version` other than `2`, including version 1 generic file installs
- unknown fields, including `format: "agent-plugin@1.0.0"` package trees
- `instructionSet` components
- empty `packages`, empty `components`, or duplicate package or component IDs
- destination paths, generic file trees, and always-on instruction files

Leftover v1 and native plugin installs are retired on sync. See [ADR 0001](decisions/0001-multi-agent-desired-state.md).

## Repository and operation limits

A snapshot may contain at most 2,000 files and 50 MB of selected content. Symlinks, special entries, case-insensitive collisions, and paths outside the repository are rejected.

Install and update preflight every physical identity. Identical desired content is coalesced; different content at one path/key/marker is a hard conflict. Local drift blocks automatic update and normal uninstall. Explicit replacement or force removal makes a persistent backup first. Unrelated keys/comments in shared JSONC/TOML documents and unrelated text outside managed markers remain user-owned.
