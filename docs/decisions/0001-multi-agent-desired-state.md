# ADR 0001: Manage multi-agent configuration as desired state

- Status: accepted
- Date: 2026-08-14

## Context

Manifest v1 maps one repository path to one destination. Agent Plugin support later added Cursor and GitHub Copilot copies beside that primary destination. Those extra writes were not first-class ownership records, failures could be ignored, directory shape was used to infer plugin cleanup, and an undocumented Copilot settings key was edited. This could report success after a partial install or remove content Skill Manager had not proved it owned.

Portable Agent Skills, MCP servers, and instructions also do not map one-to-one to products. Several agents share `~/.agents/skills`, while MCP and instruction dialects use different structured documents and precedence rules.

## Decision

Skill Manager is a desired-state manager with three separate axes:

1. A source package is the user-facing install unit.
2. Portable components describe skills, MCP servers, and always-on instruction sets without target paths.
3. Compile-time target adapters translate components into typed resources but never write them.

One executor owns preflight, staging, activation, rollback, recovery, and the ledger commit. Ledger v4 separates installations, logical target bindings, and reference-counted physical resources.

The v2 source contract makes these choices:

- Packages are installed atomically. Individual component selection is deferred.
- Package IDs are `source-id/package-id`. Component IDs are package-local and required when a package contains several components. Installed names receive the source namespace.
- Portable MCP supports Agent Plugins 1.0.0 `stdio`, `streamable-http`, and `sse` definitions. Adapters may reject a transport their pinned dialect cannot represent.
- Sensitive HTTP headers must reference an environment variable; source manifests do not persist secret values.
- `agent-plugin@1.0.0` is the only portable bundle format. It is preserved for Cursor and GitHub Copilot, and projected into skills/MCP for other targets.
- Target-native extensions are not part of manifest v2. Hooks, monitors, in-process plugins, LSP servers, and native agents remain target-qualified candidates requiring separate threat-model ADRs.
- Generic file-tree installs remain manifest v1-only. V1 remains readable and uses its explicit destination unchanged.
- `conflictsWith` records explicit canonical package IDs. Dependencies and version solving are deferred.
- User scope is the only managed scope in this release. Agent detection is advisory and never selects an agent.

Cursor plugins use the documented `~/.cursor/plugins/local` development path. Copilot direct plugins use `~/.copilot/installed-plugins/_direct`; Skill Manager no longer writes an untracked `enabledPlugins` settings entry. Plugin data is a runtime location exposed through `${PLUGIN_DATA}`, not a directory created during installation.

## Consequences

Identical resources can satisfy several agents without duplication. Disabling one agent removes its binding and retains a shared resource until its final consumer is gone. Shared JSON, JSONC, TOML, and Markdown files retain unrelated user content; drift is checked at the owned key or marker while activation also checks the whole document.

Install, update, bulk operations, target cleanup, and source removal use a recovery journal and one ledger commit. Replacing unmanaged or force-removing modified content creates a persistent backup first.

The tradeoff is more explicit planning and compatibility data. Unsupported mappings remain visible rather than being counted as installed, and new target-native execution surfaces cannot be added as generic portable components.
