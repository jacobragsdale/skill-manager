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
| `destination` | One anchor and relative path.                                             |

Example generic file:

```json
{ "id": "settings", "source": "config/settings.json", "destination": { "anchor": "config", "path": "acme/settings.json" } }
```

Example directory:

```json
{ "id": "templates", "source": "templates", "destination": { "anchor": "data", "path": "acme/templates" } }
```

A directory is copied recursively. Files such as `scripts/check.sh` are ordinary bundled content, and executable permission bits are preserved where the operating system supports them. Skill Manager never invokes copied programs.

## Destination anchors

| Anchor      | Resolves beneath                        |
| ----------- | --------------------------------------- |
| `home`      | Current user's home directory.          |
| `config`    | Current user's configuration directory. |
| `data`      | Current user's data directory.          |
| `localData` | Current user's local data directory.    |
| `cache`     | Current user's cache directory.         |

Destination paths must be non-empty relative UTF-8 paths using forward slashes. Absolute paths, `.` or `..`, Windows reserved names, non-portable characters, trailing spaces or periods, and path components longer than 255 UTF-16 units are rejected.

Destinations cannot overlap another install in the same source. They also cannot resolve inside Skill Manager's own config, data, local-data, or cache state directories.

## Agent Skills

A source directory whose root contains `SKILL.md` is treated as an Agent Skill.

Skill Manager reads only two frontmatter values:

- `name`, a non-empty string;
- `description`, a non-empty string of at most 1,024 characters.

The frontmatter name, install id, and source directory basename must match. For source `acme` and local id `review`, the installed name is `acme-review`; the destination basename must also be `acme-review`.

During staging, only the installed `name` value is replaced. Other frontmatter values, the Markdown body, and every other file in the directory are retained.

## Validation and partial errors

The source metadata and non-empty install list are structural requirements. Each install is then normalized independently. A bad source path, id, Agent Skill, or destination becomes a catalog error while valid sibling entries remain available.

A source is rejected when it produces no valid installs. Duplicate ids and overlapping destination roots are also rejected from ownership because the result would be ambiguous.

The entire sparse snapshot is checked for symlinks, non-regular entries, non-portable paths, case-insensitive path collisions, more than 2,000 files, or more than 50 MB of content.

## Install behavior

An install owns one destination digest in the local ledger. Updates apply only when the installed destination still matches that digest. An unmanaged destination is reported as a conflict and can be replaced only through the explicit backup-first Manage flow.

New upstream entries remain available until selected manually. Already-installed, unmodified entries may update during synchronization. Entries removed upstream remain visible and uninstallable from their ledger record.
