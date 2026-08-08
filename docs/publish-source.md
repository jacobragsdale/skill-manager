# Publish a Skill Manager source

Use a Skill Manager source when you want one Git repository to publish files, complete directories, or Agent Skills to explicit locations on a user's computer. The repository needs a top-level `skill-manager.json` on its default branch and must be reachable through an `https://` or `ssh://` Git URL.

This guide builds a source that publishes three kinds of content:

- one Agent Skill;
- one complete directory; and
- one individual file.

See the [manifest reference](manifest-reference.md) for the complete field and validation rules.

## Understand the supported model

Every install maps exactly one repository path to exactly one destination. Add a separate install entry for every independently managed file or directory.

| Content            | `source` points to                      | Install behavior                                                                |
| ------------------ | --------------------------------------- | ------------------------------------------------------------------------------- |
| Individual file    | A regular file                          | Copies the file to the exact destination path.                                  |
| Complete directory | A regular directory                     | Recursively copies the directory and all of its contents.                       |
| Agent Skill        | A directory with `SKILL.md` at its root | Copies the complete skill and namespaces its installed name with the source id. |

Directory installs do not require a list of their contents. If `source` is `templates` and `destination` is `~/.config/example/templates`, the complete `templates` directory is installed at that destination. New files added beneath `templates` are included in later updates.

Skill Manager preserves nested files and executable permission bits where the operating system supports them. It never executes source content. Symlinks, special filesystem entries, globs, hooks, commands, dependencies, platform selectors, and environment-variable expansion are not supported.

## Create the repository layout

Start with this structure:

```text
example-source/
├── skill-manager.json
├── config/
│   └── settings.json
├── skills/
│   └── review/
│       ├── SKILL.md
│       └── scripts/
│           └── check.sh
└── templates/
    ├── pull-request.md
    └── release.md
```

The repository may contain other files. Skill Manager sparsely checks out the manifest and the paths named by its install entries.

## Define the Agent Skill

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

Skill Manager reads these Agent Skill frontmatter fields:

- `name` must match the install id and source directory name;
- `description` becomes the catalog description; and
- `disable-model-invocation: true` marks the item as **Manual Invocation** in the app.

The boolean flag must not be quoted. Omit it or set it to `false` for a skill that may be selected automatically. Skill Manager preserves the flag and all other frontmatter when it copies the skill; it only rewrites `name` to include the source namespace.

For example, source id `example` and skill name `review` produce the installed skill name `example-review`. The destination directory must end with that same name.

The Skillbook [`releases` skill](https://github.com/jacobragsdale/skillbook/blob/main/skills/releases/SKILL.md) is a live example of a manual-invocation skill.

## Add the source manifest

Create `skill-manager.json` at the repository root:

```json
{
  "version": 1,
  "source": { "id": "example", "name": "Example source", "description": "Shared Agent Skills and configuration." },
  "installs": [
    { "id": "review", "source": "skills/review", "destination": "~/.agents/skills/example-review" },
    { "id": "templates", "source": "templates", "destination": "~/.config/example/templates" },
    { "id": "settings", "source": "config/settings.json", "destination": "~/.config/example/settings.json" }
  ]
}
```

The source id is the stable namespace for every item. Keep it short and do not change it after users add the source. Install ids are local to the source; the app combines them as `source-id/install-id` for ownership.

## Choose destination paths

Set `destination` to the final path that Skill Manager should own. It accepts two forms.

Use `~/` for a portable per-user path:

```json
"destination": "~/.agents/skills/example-review"
```

`~/` expands from the current user's home directory on macOS, Linux, and Windows. Forward slashes make this form portable across all three operating systems.

Use a native absolute path when the source is intended for a known machine or operating system:

```json
"destination": "/opt/example/templates"
```

On Windows, use a drive-qualified path:

```json
"destination": "C:/Users/alice/AppData/Local/example/templates"
```

Skill Manager creates missing parent directories. It rejects bare relative paths, filesystem roots, `.` and `..` components, non-portable path names, overlapping destinations in the same source, and destinations inside its own state directories. It does not expand `$HOME`, `%USERPROFILE%`, or arbitrary environment variables.

An absolute path is evaluated on the operating system running Skill Manager. Prefer `~/` when the same manifest should work on macOS, Linux, and Windows.

## Preserve executable files

If a bundled script should remain executable after installation, record that permission in Git:

```bash
chmod +x skills/review/scripts/check.sh
git add --chmod=+x skills/review/scripts/check.sh
```

The script remains ordinary copied content. Skill Manager does not run it during installation, updates, or removal.

## Validate the source

First validate the JSON and generated schema:

```bash
jq empty skill-manager.json
npx ajv-cli validate --spec=draft2020 --strict=false \
  -s https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v1/source-manifest.schema.json \
  -d skill-manager.json
```

Schema validation checks the document shape and basic limits. Run Skill Manager's repository-aware validator to check source paths, Agent Skill naming, path portability, symlinks, destination overlap, and repository size:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source -- /path/to/example-source
```

A valid result for this example is:

```text
example: 3 valid install(s), 0 catalog error(s)
```

You can also validate a published source directly:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source -- https://github.com/acme/example-source.git
```

The manifest is limited to 1 MB. The selected source content is limited to 2,000 files and 50 MB. Invalid install entries are reported separately so valid siblings can still be published, but a source with no valid installs is rejected.

## Publish and verify

Commit and push the manifest and every referenced path together. Then verify the published source in Skill Manager:

1. Open **Manage Sources**.
2. Add the repository's `https://` or `ssh://` URL.
3. Review the source name, namespace, commit, and valid install count.
4. Confirm the source.
5. Install each item and inspect its resolved destination.
6. Confirm that a skill with `disable-model-invocation: true` displays the **Manual Invocation** tag.

Push a content change and select **Refresh** to exercise updates. Skill Manager automatically refreshes installed items only when their destination still matches the recorded digest. New entries remain available until installed. A conflicting unmanaged destination requires an explicit backup-first **Replace**, while a locally modified managed destination remains protected.
