# Roadmap

Skill Manager is intentionally focused on independently managed Agent Skills.
The current source contract is the conventional `skills/<name>/SKILL.md`
layout; bundles and other installable content types are outside the product
scope.

## Current foundation

- one removable default source plus user-added Git sources;
- source-aware catalog entries and ownership markers;
- validated, commit-pinned offline caches;
- manual **Manage** for exact unmanaged matches;
- confirmed, backup-first **Replace…** for differing unmanaged directories;
- safe staged install, conservative update, and protected uninstall behavior;
- duplicate skill names without automatic source switching;
- source-level installation and removal plans; and
- automatic updates that never install newly discovered skills.

## Design principles

1. Keep every skill visible and independently installable.
2. Normalize repository content before installation code consumes it.
3. Represent destinations as an approved anchor plus a validated relative path.
4. Keep planning read-only and centralize filesystem changes in the install
   subsystem.
5. Show the complete preflight plan before a source-wide operation.
6. Preserve unmanaged and locally modified skill directories.
7. Bind ownership and any future trust to a stable source identity.
8. Keep the default source removable and preserve an explicitly empty source
   list.

## Possible next steps

Add these only in response to a concrete need:

- an optional top-level source manifest that feeds the existing normalized
  catalog;
- additional approved destination anchors selected by manifest metadata;
- a local ownership ledger for installations that cannot use an embedded
  marker;
- executable plan steps authorized by locally persisted trust bound to the
  stable source identity;
- import and export of the user's selected skills; or
- signed desktop application updates.

A repository manifest must never authorize its own executable actions. No
manifest parser, trust state, scripts, or additional destination support exists
today.

## Deliberate non-goals

- bundles or additional installable content types;
- a dependency solver, version constraints, or lockfile;
- automatic installation of newly published skills;
- automatic uninstall when a skill disappears upstream;
- silent adoption or replacement of unmanaged skill entries; and
- repository-controlled trust.
