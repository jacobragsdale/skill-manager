# Publish a portable multi-agent source

This tutorial publishes one Agent Skill and one always-on instruction set as a manifest v2 package. Skill Manager will project the package across the agents a user explicitly enables.

## Create the repository

Use this layout:

```text
example-source/
├── skill-manager.json
├── rules/
│   └── review.md
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

The skill name must match its component ID. Skill Manager will install it as `example-review`, preserving all other frontmatter and files. A bundled script remains ordinary content: the app never executes it, although a target agent may invoke it later.

Create `rules/review.md`:

```markdown
# Review policy

Before declaring work complete, inspect the diff and report the checks actually run.
```

## Declare the package

Create the root `skill-manager.json`:

```json
{
  "version": 2,
  "source": { "id": "example", "name": "Example workflows", "description": "Portable review configuration." },
  "packages": [
    {
      "id": "review",
      "name": "Review workflow",
      "description": "Review skill and always-on policy.",
      "components": [
        { "kind": "skill", "id": "review", "path": "skills/review" },
        { "kind": "instructionSet", "id": "review-rules", "path": "rules/review.md", "activation": "always", "topics": ["review"] }
      ]
    }
  ]
}
```

Packages are atomic. Unsupported target/component pairs remain visible and are skipped during planning; any failure among accepted resources rolls back the entire requested operation.

## Validate locally

Validate the JSON shape against the checked-in schema:

```bash
jq empty skill-manager.json
npx ajv-cli validate --spec=draft2020 --strict=false \
  -s https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v2/source-manifest.schema.json \
  -d skill-manager.json
```

Then run the repository-aware validator for source containment, component names, Agent Plugin/MCP shape, portability, symlinks, and repository limits:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source -- /path/to/example-source
```

You can also validate a published default branch:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source -- https://github.com/acme/example-source.git
```

## Publish and verify

Commit and push the manifest and referenced paths together. In Skill Manager:

1. Open **Agents I Use**, enable at least one target, and note that detection is advisory.
2. Open **Manage Sources** and add the repository URL.
3. Review the source namespace, commit, and valid package count.
4. Select **Install** on the package.
5. Review every target capability and physical resource. Shared `~/.agents/skills` projections should appear once with several consumers.
6. Confirm the transaction and inspect the target after its documented reload boundary.

Static file presence proves the desired state was written, not that an agent loaded it. For runtime evidence, use the target's own skill/config inspection surface in a disposable home and record the target version.

## Add MCP or an Agent Plugin

For a standalone MCP definition, add an `mcpServer` component whose path is a pinned Agent Plugins 1.0.0 `mcp.json` object. Installation becomes Tier 3 and requires explicit approval. Never embed a secret in a sensitive header; reference an environment variable.

For a portable bundle, replace `components` with:

```json
"format": "agent-plugin@1.0.0",
"path": "plugins/data-tools"
```

Cursor and GitHub Copilot receive the preserved package. Other compatible targets receive its skills and MCP entries. See the [manifest reference](manifest-reference.md) for the exact contract.

## Keep generic path copies on v1

If the content is a generic file or directory with a publisher-selected destination, use manifest v1. V2 intentionally has no generic file-tree component; it describes portable agent concepts rather than machine-specific paths.
