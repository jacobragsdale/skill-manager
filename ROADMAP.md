# Roadmap

Skill Manager is in initial development. The current goal is a narrow, reliable file installer rather than a package manager or automation runtime.

## Current foundation

- one strict manifest version;
- one source path and one destination per install;
- file or recursive directory copying with permission bits preserved;
- Agent Skill namespacing by source id;
- absolute destinations with portable `~/` home expansion;
- immutable Git snapshots and offline cached state;
- transactional install, update, replace, uninstall, and source removal;
- digest-based local-change protection and backup-first unmanaged replacement;
- manual install-all and uninstall-all; and
- background refresh of already-installed items.

## Safety invariants

1. A repository must choose an explicit absolute or `~/` destination and cannot target Skill Manager's own state.
2. Symlinks, non-portable paths, case-insensitive collisions, overlapping destinations, and writes into Skill Manager state are rejected.
3. A source namespace cannot be reused by another configured repository.
4. New upstream installs are never applied automatically.
5. Modified managed content is never updated or normally uninstalled without an explicit destructive confirmation.
6. Source content is copied, never executed.

## Possible next work

- clearer diagnostics for partially valid sources;
- cache cleanup that proves no active or installed revision is referenced;
- signed source attestations;
- exportable diagnostic reports; and
- focused performance work for very large directory installs.

## Explicit non-goals

- manifest hooks, actions, or command execution;
- platform or architecture selectors;
- glob expansion, templates, collections, or multi-path install groups;
- dependency solving or installation ordering;
- workspace-relative destinations and elevated installation workflows;
- automatic installation of newly published entries; and
- automatic deletion of entries removed upstream.
