# Publish a Skill Manager source

This guide creates a source that publishes one Agent Skill, one generic file, and one explicit diagnostic action.

## 1. Choose a namespace

Choose a stable 2–16 character ID such as `acme`. It becomes part of every canonical item ID and Agent Skill name. Changing it later is treated as a source error, not a rename.

Confirm that the target Skill Manager installation does not already use the namespace for another URL.

## 2. Lay out the repository

```text
.
├── skill-manager.json
├── skills/
│   └── review/
│       └── SKILL.md
├── rules/
│   └── review.md
└── scripts/
    └── doctor.sh
```

Create `skills/review/SKILL.md` with the local, unprefixed name:

```markdown
---
name: review
description: Review a change for correctness and maintainability.
license: MIT
disable-model-invocation: true
---

# Review

Follow the repository's review policy.
```

Skill Manager will install this as `acme-review` and change only the staged frontmatter `name` to match.

## 3. Add the manifest

Create `skill-manager.json` at the repository root:

```json
{
  "$schema": "https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v1/source-manifest.schema.json",
  "version": 1,
  "source": { "id": "acme", "name": "Acme agent configuration", "description": "Acme's shared Agent Skills and review policy." },
  "agentSkills": [{ "include": ["skills/*"], "destinations": [{ "anchor": "home", "path": ".agents/skills/${skill.name}" }] }],
  "items": [
    {
      "id": "review-policy",
      "name": "Review policy",
      "description": "Installs Acme's review policy.",
      "kind": "policy",
      "files": [{ "source": "rules/review.md", "destination": { "anchor": "config", "path": "acme/review.md" } }]
    }
  ],
  "actions": [
    { "id": "doctor", "name": "Check source setup", "description": "Runs source diagnostics.", "steps": [{ "id": "doctor", "program": { "source": "scripts/doctor.sh" }, "timeoutSeconds": 60 }] }
  ]
}
```

Consult the [manifest reference](manifest-reference.md) before adding collections, multiple destinations, platform selectors, or lifecycle hooks.

## 4. Make referenced programs executable

On Unix, source programs must have an executable bit in Git:

```bash
chmod +x scripts/doctor.sh
git update-index --chmod=+x scripts/doctor.sh
```

Programs are direct, noninteractive invocations. Put interpreter selection in the file's shebang and accept arguments through the manifest's `args` array.

Write hooks and actions to be idempotent. Skill Manager can roll back its declarative file transaction, but it cannot infer or reverse arbitrary side effects created by a program.

## 5. Validate the JSON Schema

Download the published schema and validate Draft 2020-12 structure:

```bash
schema_file="$(mktemp)"
curl -fsSL \
  https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v1/source-manifest.schema.json \
  -o "$schema_file"
npx --yes ajv-cli validate --spec=draft2020 -s "$schema_file" -d skill-manager.json
```

Schema validation does not inspect repository files or expanded templates. Adding the source in Skill Manager performs the definitive semantic validation, including globs, frontmatter, symlinks, collisions, destinations, and executable permissions.

## 6. Test the source locally

1. Commit every referenced file. Acquisition reads the repository commit, not uncommitted working-tree content.
2. Push the repository to an HTTPS or SSH URL the system Git client can access.
3. In Skill Manager, open **Sources**, enter the URL, and choose **Prepare source**.
4. Review the namespace, commit, and item count.
5. If the manifest contains hooks or actions, read and accept the executable warning. This approval covers future changed code and scheduled background update hooks for this exact repository identity.
6. Install each item and reveal its destinations.
7. Invoke actions explicitly and inspect their streamed and durable logs.
8. Modify a managed destination locally and confirm normal update and uninstall protect it.
9. Review source-removal cleanup before confirming any modified-path warning.

## 7. Publish changes safely

Push manifest and content changes together. Existing installed items update only when their recorded destinations still match their installed digests. New items appear as available and are never installed automatically. Removed upstream items remain uninstallable from the retained installed revision.

Do not change `source.id` to transfer or rename a source. Publish a new source and let users complete a new add and trust flow instead.
