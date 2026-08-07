# Roadmap

Skill Manager's shipped foundation is the required namespaced source manifest v1. Future work should preserve its security and ownership invariants rather than create parallel source or installation paths.

## Current foundation

- strict Draft 2020-12 source manifest and generated public schema;
- unique manifest namespaces separated from URL-derived repository identities;
- prefixed Agent Skill directory and frontmatter materialization;
- explicit generic items and glob-generated collections;
- approved current-user destination anchors;
- immutable commit snapshots with manifest-first sparse or archive acquisition;
- atomic ownership ledger and retained uninstall revisions;
- per-item transactional file activation, exact adoption, and backup-first replacement;
- protected automatic updates that never install new items;
- ordered lifecycle hooks plus explicit source and item actions;
- repository-bound executable trust, revocation, one-time cleanup approval, logs, and bounded processes;
- destructive source-removal planning before namespace, trust, and cache release; and
- strict generic IPC and frontend Zod validation.

## Invariants

1. The manifest namespace is never a security identity.
2. Repository metadata can request executable trust but cannot grant it.
3. New upstream items are never installed automatically.
4. Normal update and uninstall never overwrite locally modified managed content.
5. Exact automatic adoption requires provable source attribution and content equality.
6. Every declarative destination resolves beneath a locally approved per-user anchor.
7. Removed upstream items remain uninstallable from their retained installed revision.
8. Partial source cleanup retains the source, namespace claim, trust, and snapshots for retry.
9. Opaque program side effects remain the source author's responsibility.
10. The active frontend validates every IPC response and event before use.

## Candidate next work

Add only in response to a concrete product need:

- signed source or release attestations that supplement, but do not replace, local trust;
- explicit retained-revision garbage collection proven not to remove installed or active-operation snapshots;
- export and import of selected canonical item IDs;
- richer action result metadata without weakening bounded logs;
- accessibility and native performance regression automation; or
- signed desktop application updates.

## Deliberate non-goals for v1

- workspace, absolute, system-wide, or elevated destinations;
- interactive terminals, prompts, or dependency-solving commands;
- a package dependency solver, version constraints, or source priority;
- sandbox claims for unsandboxed repository programs;
- automatic install of newly published items;
- automatic uninstall of removed upstream items;
- silent replacement of differing unmanaged content;
- namespace-based ownership or trust transfer; and
- proof that uninstall hooks reversed every opaque external side effect.
