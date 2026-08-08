# Architecture

Skill Manager has four boundaries: Git source acquisition, manifest normalization, transactional installation, and a thin application/UI layer.

## Source identity and snapshots

`sourceId` is the short namespace published in `skill-manager.json`. `sourceKey` is a stable hash of the canonical repository URL. Keeping them separate prevents a repository from transferring cache or installation ownership by choosing another source's display namespace.

All sources use the same system-Git path:

1. query the remote default-branch commit;
2. make a shallow, blob-filtered sparse clone;
3. read the root manifest;
4. expand the sparse checkout to its explicit source paths;
5. remove `.git` metadata;
6. validate the tree; and
7. activate the commit under the URL-derived cache directory.

The previous validated commit stays active when refresh, validation, or namespace checks fail. Blocking Git work runs outside the async scheduler. The process helper closes standard input, bounds stdout and stderr, applies a timeout, and terminates the process tree on failure.

## Manifest normalization

The manifest parser validates the version and source metadata. The catalog normalizer then resolves each install independently into:

- canonical id and local id;
- display name and description;
- source path and one destination;
- content digest; and
- optional materialized Agent Skill name.

This separation lets a source expose valid siblings while reporting entry-level errors. Source-wide ambiguity—such as overlapping destinations—remains fatal.

Agent Skill staging rewrites only the frontmatter name to its source-prefixed installed name. The rest of the directory is copied like any other directory.

## Ownership and transactions

The installation ledger records source identity, commit, display data, item digest, and one owned destination with its installed digest. It is the authority for install status and removed-upstream entries.

Install and update stage a complete file or directory beside the destination. Existing owned content is moved aside, staged content is activated, and the ledger is atomically replaced. A failed activation or ledger write restores the previous destination.

The explicit Replace flow handles an unmanaged destination. It moves that destination into a timestamped backup before activation and never writes through a symlink.

Normal update and uninstall stop when the owned digest has changed. Source removal first returns a path-level plan; deleting modified managed content requires a separate acknowledgement.

## Application and UI

The application service serializes mutations with an operation lock and prevents concurrent refreshes with a sync lock. It coordinates source configuration, immutable snapshots, ownership reconciliation, bulk operations, and scheduled refresh.

The IPC layer exposes plain serialized state and commands. The React UI validates every response with Zod and contains no manifest or filesystem policy of its own.

Background synchronization refreshes source snapshots and updates only entries already installed and still unmodified. It never installs newly published entries or automatically removes entries that disappear upstream.
