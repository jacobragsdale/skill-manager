# Roadmap

Skill Manager is intentionally focused on Agent Skills. Bundles organize related
skills and provide bulk installation, but they do not introduce a second
installable content type, dependency ownership, or package-manager state.

## Current foundation

- one removable default source plus user-added Git sources;
- source-aware catalog entries and ownership markers;
- validated, commit-pinned offline caches;
- safe install, update, conflict, backup, and uninstall behavior;
- duplicate skill names without automatic source switching;
- optional, source-local skill bundles;
- source-level and bundle-level installation plans; and
- automatic updates that never install newly discovered skills.

## Design principles

1. Keep the repository contract to conventional `skills/` and optional
   `bundles/` directories.
2. Keep every skill visible and independently installable.
3. Resolve bundle membership within one validated source commit.
4. Show the complete preflight plan before a bulk operation.
5. Preserve unmanaged and locally modified skill directories.
6. Derive bundle state from member state instead of adding lockfiles or
   reference counting.
7. Keep the default source removable and preserve an explicitly empty source
   list.

## Possible next steps

Add these only in response to concrete needs:

- a top-level manifest for multiple catalog roots or display metadata;
- bundle categories, icons, or longer descriptions;
- nested bundles with cycle detection;
- explicit cross-source references pinned to a source URL;
- import and export of the user's selected skills; or
- signed desktop application updates.

## Deliberate non-goals

- arbitrary install scripts or lifecycle hooks;
- a dependency solver, version constraints, or lockfile;
- automatic installation of new bundle members;
- automatic uninstall when a skill or bundle disappears upstream;
- silent edits to unmanaged skill directories; and
- additional installable content types.
