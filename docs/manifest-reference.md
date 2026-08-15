# Source manifest reference

Every source repository must contain `skill-manager.json` at its root. Unknown fields are rejected.

The generated JSON Schema is published at [`schemas/v1/source-manifest.schema.json`](../schemas/v1/source-manifest.schema.json).

## Root object

| Field      | Type    | Required | Meaning                            |
| ---------- | ------- | -------- | ---------------------------------- |
| `version`  | integer | Yes      | Must be `1`.                       |
| `source`   | object  | Yes      | Stable namespace and display text. |
| `installs` | array   | Yes      | One or more explicit installs.     |

## `source`

| Field         | Rules                                                                          |
| ------------- | ------------------------------------------------------------------------------ |
| `id`          | 2–16 lowercase ASCII letters, digits, or single hyphens; begins with a letter. |
| `name`        | 1–120 characters.                                                              |
| `description` | 1–1,024 characters.                                                            |

`source.id` namespaces every catalog id as `source-id/install-id`. It also prefixes installed Agent Skill names.

The source id is distinct from `sourceKey`, which Skill Manager derives from the canonical repository URL. The source key owns cache and installation records; configured sources cannot share a URL, source key, or source id.

## `installs`

Each entry has exactly three fields:

| Field         | Meaning                                                                   |
| ------------- | ------------------------------------------------------------------------- |
| `id`          | Local identifier. Use 1–64 lowercase letters, digits, and single hyphens. |
| `source`      | One repository-relative regular file or directory.                        |
| `destination` | One absolute path or a home-relative path beginning with `~/`.            |

Example generic file:

```json
{ "id": "settings", "source": "config/settings.json", "destination": "~/.config/acme/settings.json" }
```

Example directory:

```json
{ "id": "templates", "source": "templates", "destination": "/opt/acme/templates" }
```

A directory is copied recursively. Files such as `scripts/check.sh` are ordinary bundled content, and executable permission bits are preserved where the operating system supports them. Skill Manager never invokes copied programs.

## Destination paths

`destination` is a non-root absolute filesystem path. A path beginning with `~/` expands from the current user's home directory on macOS, Linux, and Windows, so `~/.agents/skills/acme-review` is the portable choice for a per-user install. Native absolute paths are also accepted; use a drive-qualified path such as `C:/Users/alice/tools/acme` on Windows.

Bare relative paths, a filesystem root, `.` or `..`, Windows reserved names, non-portable characters, trailing spaces or periods, and path components longer than 255 UTF-16 units are rejected. Forward slashes are required for `~/` paths and recommended in manifests for portability.

Destinations cannot overlap another install in the same source. They also cannot resolve inside Skill Manager's own config, data, local-data, or cache state directories.

## Agent Skills

A source directory whose root contains `SKILL.md` is treated as an Agent Skill.

Skill Manager reads three frontmatter values:

- `name`, a non-empty string;
- `description`, a non-empty string of at most 1,024 characters;
- `disable-model-invocation`, an optional boolean. When `true`, the app marks the skill for manual invocation.

The frontmatter name, install id, and source directory basename must match. For source `acme` and local id `review`, the installed name is `acme-review`; the destination basename must also be `acme-review`.

During staging, only the installed `name` value is replaced. Other frontmatter values, the Markdown body, and every other file in the directory are retained.

## Agent Plugins and MCP servers

A source directory whose root contains `plugin.json` is treated as an Agent Plugin package conformant with the [Agent Plugins specification](https://agent-plugins.org/specification).

Skill Manager supports plugins that package Model Context Protocol (MCP) servers (`mcp.json`) and skills (`skills/`):

- `plugin.json` specifies the plugin manifest, including `$schema` and `name`.
- `mcp.json` defines MCP server configurations using standard `stdio`, `streamable-http`, or `sse` transports.
- Dynamic placeholders `${PLUGIN_ROOT}` and `${PLUGIN_DATA}` in `mcp.json` are automatically expanded during installation.

When installed:

- The package is staged to its configured `destination` (e.g. `~/.agents/plugins/<source-id>-<id>`).
- It is automatically linked into Cursor's local plugins directory (`~/.cursor/plugins/local/<source-id>-<id>`).
- It is automatically linked into GitHub Copilot's plugins directory (`~/.copilot/installed-plugins/_direct/<source-id>-<id>`) and enabled in `~/.copilot/settings.json`.
- On uninstallation, all plugin directories and Copilot settings are cleanly cleaned up.

## Validation and partial errors

The source metadata and non-empty install list are structural requirements. Each install is then normalized independently. A bad source path, id, Agent Skill, or destination becomes a catalog error while valid sibling entries remain available.

A source is rejected when it produces no valid installs. Duplicate ids and overlapping destination roots are also rejected from ownership because the result would be ambiguous.

The entire sparse snapshot is checked for symlinks, non-regular entries, non-portable paths, case-insensitive path collisions, more than 2,000 files, or more than 50 MB of content.

## Install behavior

An install owns one destination digest in the local ledger. Updates apply only when the installed destination still matches that digest. An unmanaged destination is reported as a conflict and can be replaced only through the explicit backup-first Replace flow.

New upstream entries remain available until selected manually. Already-installed, unmodified entries may update during synchronization. Entries removed upstream remain visible and uninstallable from their ledger record.
