# Architecture

Skill Manager turns immutable Git source packages into desired resources for explicitly enabled coding agents. Acquisition, normalization, planning, execution, ownership, and presentation are separate boundaries.

## Source snapshots and normalization

`sourceId` is the short namespace published in `skill-manager.json`. `sourceKey` is a hash of the canonical repository URL. A repository cannot transfer cache or installation ownership by changing its display namespace.

A refresh queries the default-branch commit, creates a shallow blob-filtered sparse clone, expands it to the manifest's referenced paths, removes Git metadata, validates the complete portable tree, and activates an immutable commit directory. A failed refresh leaves the prior validated commit active.

Manifest v1 normalizes each explicit source/destination pair as a legacy file-tree package. Manifest v2 normalizes packages containing skills and MCP servers. Invalid package entries are reported independently; source-wide ambiguity remains fatal.

## Profiles, adapters, and plans

Agent profiles are stored separately from sources. Detection enables any installed agent that has no saved preference; an explicit disable is remembered. Version output stays advisory.

Each stable target selects a pinned dialect. A built-in adapter reports `native`, `losslessTranslation`, `lossyTranslation`, `unsupported`, or `blocked`, then returns typed desired resources. It cannot mutate the machine.

The planner fans every package component across enabled profiles and coalesces identical physical identities. Cursor, Codex, OpenCode, Grok Build, and GitHub Copilot can therefore share one namespaced skill under `~/.agents/skills` while retaining separate logical bindings.

The initial resources are:

- whole files or directories with installed digests;
- semantic JSON, JSONC, or TOML entries with key/value digests; and
- bindings from a package/component/target/scope to those resources.

Global preflight rejects cross-source identity collisions, different desired content at one identity, unsafe path overlap, exact owned-resource drift, malformed shared documents, explicit `conflictsWith` packages, and missing Tier 3 MCP approval.

## Central execution and recovery

Only `executor.rs` mutates planned resources. It stages every path and complete rewritten document before writing `resource-transaction.json`. Activation is deterministic and checks the original whole-path/document digest again. The ledger is atomically replaced only after all resource activations succeed.

An activation or ledger error rolls back the complete operation. On launch, a journal whose transaction ID is absent from the ledger is rolled back; a journal already committed in the ledger is cleaned up. Bulk install/uninstall and source removal share the same all-or-nothing boundary.

Unmanaged replacement and force-removal of modified content create a persistent backup under `~/.agents/.skill-manager-backups`. Normal update and uninstall stop on drift. Shared config mutations preserve comments where the target format permits and never claim unrelated keys.

## Ledger v4

Ledger v4 has three indexes:

- installations retain package provenance, source digest, and removed-upstream identity;
- bindings record component, target, dialect, scope, capability, and resource IDs; and
- resources record physical identity, exact ownership digest, adapter/dialect, and consumer bindings.

Removing a binding deletes a resource only after its last consumer disappears. Migration maps each v3 destination to a legacy binding/resource. Previously recorded Cursor and Copilot plugin copies are adopted only when a `plugin.json` is present and the digest still matches, so leftover uninstall can remove those paths. Divergent or untracked content remains untouched, and the original v3 ledger is retained until v4 is written and reread successfully.

## Trust boundaries

Skill Manager never executes source content. Generic file trees are Tier 1, skills with invokable assets are Tier 2, and MCP servers are Tier 3. Tier 3 previews show command or URL, arguments, working directory, environment-variable names, and header names. Sensitive headers must reference an environment variable rather than embedding a secret.

Enabling an agent first previews the hypothetical profile and every installed portable package it would reconcile. Accepted resources are then applied in one batch transaction. Background source refresh never invents Tier 3 approval; an MCP-affecting update remains pending for manual review.

Hooks, monitors, in-process plugins, background services, LSP servers, and native agents/subagents remain target-qualified Tier 4 candidates. They are not portable components and require separate lifecycle, permission, threat-model, ownership, and runtime-verification decisions.

See [ADR 0001](decisions/0001-multi-agent-desired-state.md) for the product decisions and [the adapter contract](adapter-contract.md) for the pinned target matrix.

## Application and UI

The application service serializes mutations and profile reconciliation with one operation lock, while refresh uses a separate sync lock. IPC returns plain compatibility, preview, profile, catalog, and outcome data. React validates every response with Zod and does not own filesystem or manifest policy.

The catalog presents packages and their components. If no agent is enabled, the app prompts for that selection and skips portable background updates until at least one agent is chosen. Install review shows target compatibility, physical resources, coalesced consumers, trust details, warnings, backups, and the rollback boundary. Disabling a target first shows resources that will be removed versus retained.
