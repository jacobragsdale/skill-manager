# Executable trust

Skill Manager manifests can declare lifecycle hooks plus source and item actions. These programs run unsandboxed as the current user, so the application treats repository code as an explicit security boundary rather than ordinary catalog metadata.

## What trust means

Granting executable trust accepts all of the following for one canonical repository URL and its URL-derived source key:

- full access available to your user account, including filesystem, processes, credentials exposed to child processes, and network;
- the source's current hooks and actions;
- future changed executable code from validated commits; and
- changed update hooks invoked by scheduled background synchronization.

The manifest namespace is not part of this authorization. Removing a source deletes its trust record. Reusing the released namespace from a different URL requires a new add and approval flow.

Skill Manager does not verify publisher identity, signatures, or script intent. Review the repository, its ownership, and its change controls before granting trust.

## When approval is required

Adding a source with any hook or action uses a prepare, preview, and confirm flow. The source is not configured if the warning is declined.

If a previously declarative source introduces executable behavior, synchronization keeps the last validated commit active and reports **Trust Required**. Granting trust permits a later refresh to activate the executable revision.

Source and item actions are always explicit user operations. They never run on a schedule. Lifecycle hooks run only for their declared operation:

- install hooks on initial install;
- update hooks on automatic or manual update;
- uninstall hooks on item or source-removal cleanup.

Missing update hooks mean the declarative files update without rerunning install hooks.

## Revocation

Revoking trust immediately blocks all hooks and actions for the repository identity. It does not remove installed files or roll back side effects from earlier commands.

A later trust grant for the same configured URL and source key restores execution. Removing and re-adding a different URL with the same namespace does not inherit trust.

## Source-removal cleanup

Removing a source first plans complete uninstall for every installed item. If uninstall hooks exist while normal trust is revoked, the confirmation can grant fresh one-time approval for cleanup only.

Cleanup also lists every owned path whose current digest differs from its installed digest. Confirming removal deletes those locally modified managed paths without backup. The source, namespace claim, trust record, and cache remain intact if any item cleanup fails, allowing a retry.

"Complete cleanup" means declarative destinations were removed and declared uninstall hooks succeeded. Skill Manager cannot prove that an opaque program removed every external side effect it previously created.

## Process controls

Trusted commands still run through a hardened process boundary:

- standard input is closed;
- arguments are passed directly, never parsed as an implicit shell string;
- source programs execute from the retained commit snapshot;
- environment variables identify the source, item, operation, commit, snapshot, and user anchors;
- stdout and stderr stream to the UI and are retained as durable logs;
- capture is capped at 1 MB per stream;
- each step has a 1-second to 60-minute timeout; and
- timeout or overflow terminates the process tree, including through Windows `taskkill`.

Windows child consoles are hidden. Commands are noninteractive; manifests cannot request a terminal or elevation.

These controls bound application behavior and logs. They are not a sandbox and do not limit what a successfully running command can access.

## Responsibilities for source authors

Write hooks so retries are safe. In particular:

- detect already-completed work;
- use atomic writes for external state when possible;
- clean up partial work before returning failure;
- make uninstall tolerate already-absent resources;
- avoid prompts and GUI authentication; and
- print actionable diagnostics without secrets.

Skill Manager rolls back its own declarative file transaction when a pre-hook or file activation fails. It does not infer rollback commands for arbitrary program side effects. A failed post-hook leaves activated files installed and marks the item **Incomplete** for an explicit retry.
