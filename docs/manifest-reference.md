# Source manifest v1 reference

This page is the normative field reference for the required top-level `skill-manager.json`. The [published Draft 2020-12 schema](../schemas/v1/source-manifest.schema.json) defines its JSON shape; Skill Manager also applies the semantic rules below.

All manifest objects reject unknown fields.

## Top-level object

| Field         | Required | Contract                                                                                                                         |
| ------------- | -------: | -------------------------------------------------------------------------------------------------------------------------------- |
| `$schema`     |       No | If present, exactly `https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v1/source-manifest.schema.json`. |
| `version`     |      Yes | Integer `1`.                                                                                                                     |
| `source`      |      Yes | Source namespace and display metadata.                                                                                           |
| `agentSkills` |       No | Agent Skill glob groups. Default `[]`.                                                                                           |
| `items`       |       No | Explicit generic items. Default `[]`.                                                                                            |
| `collections` |       No | Glob-generated generic items. Default `[]`.                                                                                      |
| `actions`     |       No | Explicit source actions. Default `[]`.                                                                                           |

At least one of `agentSkills`, `items`, or `collections` must publish an item.

## Source identity

```json
{ "source": { "id": "fiqit", "name": "Fiqit agent configuration", "description": "Shared skills, prompts, and maintenance actions." } }
```

`source.id` is 2–16 characters. It begins with a lowercase ASCII letter and otherwise contains lowercase letters, digits, or single hyphens. It cannot end with a hyphen or contain `--`.

The manifest ID and repository identity are intentionally distinct:

| Identity    | Example                   | Used for                                                                                          |
| ----------- | ------------------------- | ------------------------------------------------------------------------------------------------- |
| `sourceId`  | `fiqit`                   | Namespace, canonical catalog IDs, and materialized Agent Skill names.                             |
| `sourceKey` | `source-0123456789abcdef` | Immutable cache paths, trust, ownership, and repository identity. Derived from the canonical URL. |

Configured sources cannot share a URL, source key, or namespace. A refresh that changes `source.id` is rejected while the last validated commit stays active. Removing a source releases its namespace only after complete cleanup; trust never transfers because it remains bound to the old URL and source key.

Canonical IDs are:

- item: `fiqit/cursor-review-rule`
- source action: `fiqit/@doctor`
- item action: `fiqit/cursor-review-rule@check`

## Agent Skills

```json
{
  "agentSkills": [
    {
      "include": ["skills/*"],
      "destinations": [
        { "anchor": "home", "path": ".agents/skills/${skill.name}" },
        { "anchor": "home", "path": ".claude/skills/${skill.name}" }
      ],
      "when": { "os": ["macos", "linux"], "arch": ["aarch64", "x86_64"] },
      "hooks": {},
      "actions": []
    }
  ]
}
```

`include` contains one or more repository-relative glob patterns. Each matching directory must contain a UTF-8 `SKILL.md` and its directory name must equal the frontmatter `name`.

For local name `review` in source `fiqit`, normalization produces:

| Value                                    | Result               |
| ---------------------------------------- | -------------------- |
| Catalog ID                               | `fiqit/review`       |
| Effective skill name                     | `fiqit-review`       |
| Destination variable `${skill.name}`     | `fiqit-review`       |
| Local-name variable `${skill.localName}` | `review`             |
| Installed `SKILL.md` frontmatter         | `name: fiqit-review` |

Every Agent Skill destination must end in `${skill.name}`. The effective name must satisfy the portable Agent Skills name contract and its 64-character limit. An overlong effective name rejects only that catalog entry.

Skill Manager parses the complete YAML frontmatter mapping and exposes `description`, `license`, `compatibility`, `metadata`, `allowed-tools`, and Claude's `disable-model-invocation`. The latter appears as **Manual Only**. Only `name` is replaced in the staged installed copy; other frontmatter and the Markdown body are preserved. See the [Agent Skills specification](https://agentskills.io/specification) and [Claude Code skill extensions](https://code.claude.com/docs/en/slash-commands).

## Explicit generic items

```json
{
  "items": [
    {
      "id": "cursor-review-rule",
      "name": "Cursor review rule",
      "description": "Installs shared review instructions.",
      "kind": "cursor-rule",
      "files": [{ "source": "rules/review.mdc", "destination": { "anchor": "home", "path": ".cursor/rules/review.mdc" } }],
      "when": { "os": ["macos", "linux", "windows"] },
      "hooks": {},
      "actions": []
    }
  ]
}
```

`id` and `kind` use 1–64 lowercase ASCII letters, digits, or single hyphens, beginning with a letter. `name` is 1–120 characters and `description` is 1–1,024 characters. `files` must contain at least one mapping.

`source` names a repository-relative regular file or directory. Symlinks and other file types are rejected. `destination` is an approved anchor plus a relative portable path.

## Generic collections

```json
{
  "collections": [
    {
      "include": ["prompts/*.prompt.md"],
      "item": {
        "id": "${stem}",
        "name": "${stem}",
        "description": "Installs ${basename}.",
        "kind": "prompt",
        "files": [{ "source": "${path}", "destination": { "anchor": "config", "path": "fiqit/prompts/${basename}" } }]
      }
    }
  ]
}
```

Each matched regular file or directory generates one item. Collection item strings may use:

| Variable      | Meaning                                        |
| ------------- | ---------------------------------------------- |
| `${path}`     | Full repository-relative matched path.         |
| `${basename}` | Final path component, including its extension. |
| `${stem}`     | Basename before its final extension.           |

The expanded item must satisfy the same rules as an explicit item. Duplicate generated IDs become per-entry catalog errors.

## Destinations

Approved anchors resolve to the current user's platform directories:

| Anchor      | Root                     |
| ----------- | ------------------------ |
| `home`      | Home directory.          |
| `config`    | Configuration directory. |
| `data`      | Data directory.          |
| `localData` | Local data directory.    |
| `cache`     | Cache directory.         |

Destination paths are non-empty relative UTF-8 paths using forward slashes. Absolute paths, `.` or `..`, globs, control characters, Windows reserved names, trailing spaces or periods, non-portable characters, case-insensitive collisions, and overlapping ownership roots are rejected. Destinations inside Skill Manager's own config, data, local-data, or cache state directories are also rejected.

Only current-user anchors exist in v1. Workspace, system-wide, and elevated destinations are unsupported.

## Platform selectors

`when` is optional on Agent Skill groups, items, actions, and command steps.

```json
{ "when": { "os": ["macos", "linux", "windows"], "arch": ["x86_64", "aarch64"] } }
```

An omitted or empty dimension matches every value. An item that does not match the current platform remains visible as unsupported. A nonmatching action remains visible but disabled; a nonmatching command step inside an otherwise supported action or hook is skipped.

## Hooks

`hooks` may contain these ordered command arrays:

1. `preInstall`
2. `postInstall`
3. `preUpdate`
4. `postUpdate`
5. `preUninstall`
6. `postUninstall`

Install and update stage all mappings and activate them as one item transaction between their pre- and post-hooks. A pre-hook or file failure leaves the previous installation active. A failed post-hook leaves activated files in place and marks the item **Incomplete** for a retry. Updates never fall back to install hooks when update hooks are absent.

Source removal runs uninstall hooks and deletes every ledger-owned path. Locally modified paths require an explicit path-level warning and are removed without backup during this cleanup.

## Actions and command steps

Source and item actions use the same shape:

```json
{
  "id": "doctor",
  "name": "Check source setup",
  "description": "Runs source diagnostics.",
  "steps": [{ "id": "doctor", "program": { "source": "scripts/doctor.sh" }, "args": ["--verbose"], "timeoutSeconds": 300, "when": { "os": ["macos", "linux"] } }]
}
```

`program` is exactly one of:

- `{ "source": "repository-relative executable" }`
- `{ "system": "executable resolved by the operating system" }`

Commands are direct process invocations. `args` is an array; shell strings are never inferred. Source programs must be regular files and executable on Unix. `timeoutSeconds` defaults to 300 and ranges from 1 to 3,600.

Commands receive:

- `SKILL_MANAGER_SOURCE_ID`
- `SOURCE_KEY`
- `ITEM_ID`
- `LOCAL_ITEM_ID`
- `SKILL_NAME`
- `LOCAL_SKILL_NAME`
- `COMMIT`
- `OPERATION`
- `SKILL_MANAGER_SOURCE_SNAPSHOT`
- `SKILL_MANAGER_HOME`
- `SKILL_MANAGER_CONFIG`
- `SKILL_MANAGER_DATA`
- `SKILL_MANAGER_LOCAL_DATA`
- `SKILL_MANAGER_CACHE`

Standard input is closed. Output is streamed to the UI, retained in durable per-step logs, and capped at 1 MB per stream. Timeouts terminate the process tree. Commands run unsandboxed as the current user; see [Executable trust](executable-trust.md).

## Source and operation limits

| Resource                     |           Limit |
| ---------------------------- | --------------: |
| Manifest                     |            1 MB |
| Referenced source files      |           2,000 |
| Referenced extracted content |           50 MB |
| Built-in archive download    |           25 MB |
| Command timeout              |      60 minutes |
| Captured output              | 1 MB per stream |

Repository paths must be portable and cannot be symlinks. Custom sources are acquired with a manifest-first sparse checkout. The built-in source reads its GitHub archive twice: manifest discovery, then referenced-content extraction.
