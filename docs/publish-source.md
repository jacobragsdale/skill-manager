# Publish a source

This guide creates a source with one Agent Skill. The skill includes an executable script as ordinary copied content.

## 1. Create the repository layout

```text
example-source/
├── skill-manager.json
└── skills/
    └── review/
        ├── SKILL.md
        └── scripts/
            └── check.sh
```

`skills/review/SKILL.md`:

```markdown
---
name: review
description: Reviews a change before it is submitted.
---

# Review

Follow the repository's review workflow.
```

The skill may contain any additional files. Skill Manager only interprets `name` and `description` in `SKILL.md`.

## 2. Add the manifest

Create `skill-manager.json` at the repository root:

```json
{
  "version": 1,
  "source": { "id": "example", "name": "Example source", "description": "Shared Agent Skills." },
  "installs": [{ "id": "review", "source": "skills/review", "destination": "~/.agents/skills/example-review" }]
}
```

The three uses of `review` must agree: install id, source directory basename, and frontmatter name. The destination basename uses the materialized `example-review` name.

## 3. Preserve script permissions

If the bundled script should be executable after installation, record that bit in Git:

```bash
chmod +x skills/review/scripts/check.sh
git add --chmod=+x skills/review/scripts/check.sh
```

Skill Manager copies the directory and permission bits. It does not run the script.

## 4. Validate locally

Validate JSON syntax and, if desired, the checked-in schema:

```bash
jq empty skill-manager.json
npx ajv-cli validate --spec=draft2020 --strict=false \
  -s https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v1/source-manifest.schema.json \
  -d skill-manager.json
```

Schema validation checks shape and basic limits. Adding the source in Skill Manager performs repository-aware validation such as source existence, Agent Skill matching, path portability, symlinks, and destination overlap.

## 5. Publish and test

Commit and push the manifest and referenced content together. In Skill Manager:

1. Open **Manage Sources**.
2. Paste the HTTPS or SSH Git URL.
3. Review the source name, namespace, commit, and valid install count.
4. Confirm the source.
5. Install the item and inspect its resolved destination.

Push a content change to test updates. Existing unmodified installs update on synchronization; new manifest entries appear as available and require a manual install.
