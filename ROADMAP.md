# Roadmap

Skill Manager should grow from a skill installer into a small manager for
reusable agent configuration:

- **Skills** are model-invoked capabilities with their own instructions and
  resources.
- **Rules** are always-on instructions that apply without a skill trigger.
- **Bundles** are optional named selections of skills and rules. Users can
  install an entire source, every member of a bundle, or any item individually.

The goal is not to create a package manager or a new agent configuration
standard. Source repositories should remain understandable without Skill
Manager, and every installed file should remain inspectable and recoverable.

## Current foundation

The app already has most of the source and lifecycle machinery this work needs:

- one preconfigured default source plus user-added Git sources;
- source-aware catalog entries and ownership markers;
- validated, commit-pinned offline caches;
- safe install, update, conflict, backup, and uninstall behavior;
- duplicate skill names without automatic source switching; and
- automatic updates that never install newly discovered content.

Rules and bundles should extend these behaviors instead of creating separate
source, cache, or update systems.

## Design principles

1. **Keep the repository contract conventional.** A source should use obvious
   top-level directories and should not need generated indexes.
2. **Keep content portable.** Skills and rules are plain files. A bundle only
   refers to them; it does not duplicate them.
3. **Bundles never gate content.** Every skill and rule remains visible and
   independently installable whether or not it belongs to a bundle. A source
   does not need to define any bundles, including for an **Install all** action.
4. **Make the item kind explicit.** `skill:python-standards` and `rule:python`
   are different identities even when their names match.
5. **Resolve locally.** A bundle initially refers only to items in its own
   source. Cross-source dependencies would make a repository depend on sources
   the user may not have configured.
6. **Never hide conflicts.** Bulk operations must show the complete plan before
   changing anything when one or more members need adoption, replacement, or
   another manual decision.
7. **Do not broaden automatic installation.** Existing managed items may update
   automatically. A new rule or a new member added to a bundle must still
   require an explicit install.
8. **Preserve unmanaged content.** Rules need the same ownership, digest,
   backup, and locally-modified protections that skills have today.
9. **Prefer derived state over package-manager state.** A bundle is installed
   when its members are installed; it should not introduce lockfiles,
   dependency resolution, or reference counting in the first version.
10. **A default is not mandatory.** The default source should make first-run
    setup useful, but users must be able to remove it through the same source
    lifecycle as any source they add.

## Default source lifecycle

`skillbook` should remain the source included by default for a new user, but it
should no longer be permanent or receive a protected removal state.

- Seed it only when source configuration has never been initialized.
- Show the normal **Remove source** action and confirmation.
- Remove its registration and cache exactly as for a user-added source.
- Do not uninstall skills or rules that came from it. They remain visible as
  managed items from a removed source and can still be uninstalled safely.
- Treat an explicitly saved empty source list as valid. Restarting or upgrading
  the app must not silently restore the default.
- Offer an explicit way to add the default source again without automatically
  opting the user back in.

The source may still use its optimized built-in download transport and carry a
`default` or `recommended` label. Those are discovery and implementation
details, not restrictions on removal.

## Proposed source repository layout

A repository may contain any combination of the three top-level directories:

```text
skills/
  python-standards/
    SKILL.md
    references/
    scripts/

rules/
  python.md
  git.md

bundles/
  python-development.yaml
  all.yaml
```

This is an additive change to the existing source contract. Every top-level
directory is optional: a skills-only or rules-only repository works without a
bundle manifest, and a mixed repository may publish some or no bundles.

### Supported repository shapes

The same convention supports a few useful source styles:

- **Mixed personal library (recommended):** one repository owns related skills,
  rules, and bundles. This is the simplest way to publish a bundle containing
  both skills and rules.
- **Focused library:** a repository contains only `skills/` or only `rules/`.
  It may still publish bundles of that one item kind.
- **Curated catalog:** a repository contains a deliberately tested set of both
  kinds plus opinionated bundles for particular workflows or teams.

A source is the boundary for names and bundle references. If skills live in one
repository and rules in another, an initial bundle cannot combine them. The
publisher can either move or copy the intended content into one curated source,
or wait for explicit cross-source references in a later version. This tradeoff
keeps bundle installation reproducible from one validated commit.

### Skills

Skills keep the existing Agent Skills layout:

```text
skills/
  <name>/
    SKILL.md
    ...optional resources
```

The directory name remains the source-local skill ID and must match the
`SKILL.md` metadata.

### Rules

Rules should be standalone Markdown files:

```text
rules/
  <name>.md
```

Use YAML frontmatter for catalog metadata and Markdown for the actual
instructions:

```markdown
---
name: python
description: Always-on constraints for high-integrity Python work.
---

# Python rules

...
```

The filename is the source-local rule ID and must match `name`. Requiring a
description gives the UI useful text without interpreting the instruction body.
Rules should not have scripts or install hooks. If rules later need supporting
material, a directory form can be introduced deliberately rather than accepted
implicitly now.

### Bundles

Bundles should be small declarative YAML files:

```yaml
name: python-development
description: Python implementation standards and the rules that always apply.
skills:
  - python-standards
  - git-ops
rules:
  - python
  - git
```

The filename is the source-local bundle ID and must match `name`. Separate
`skills` and `rules` lists are intentionally simpler than a generic dependency
language. Bundle membership does not change the underlying items: every listed
skill and rule also appears in the normal catalog and keeps its individual
install action.

Initial bundle validation should require:

- a valid unique name and non-empty description;
- at least one member;
- no duplicate member within either list;
- every member to exist in the same validated source and commit; and
- no nested bundles.

Invalid bundles should be reported without hiding otherwise valid skills and
rules from the same source. The app can reject the source only when its catalog
has no valid installable content, matching the current source behavior.

### Why not a repository manifest yet?

The three conventional directories are enough for the initial feature and keep
existing repositories easy to migrate. A top-level `skill-manager.yaml` could
later add repository display metadata or alternate content roots, but it should
not be required until there is a concrete use for it.

If subdirectory catalogs become important, a future manifest could look like:

```yaml
version: 1
catalogs:
  - path: personal
  - path: work
```

That is deliberately deferred. Arbitrary globs and per-item paths would make
sparse checkout, validation, and repository review harder.

## Rules need an installation contract

The source layout is the easy part. Unlike Agent Skills, always-on rules do not
currently have one portable destination and scope across agent products. Before
shipping rule installation, the app should prove the behavior of each supported
target rather than copying files into a directory that an agent may ignore.

The first rules milestone should answer:

1. Which agent is the first supported target?
2. Are rules user-wide, project-scoped, or both?
3. Does that agent load independent rule fragments, or must Skill Manager
   maintain a marked section in a larger instructions file?
4. What reload or restart is required after a rule changes?
5. How can the app detect that the user edited generated or managed content?

The recommended first scope is **user-wide rules for one agent target**, because
Skill Manager currently manages user-level skills. Project selection and
multi-agent rendering can follow after the ownership model is proven.

Regardless of the first target, the adapter should:

- render the canonical Markdown rule without changing its meaning;
- keep Skill Manager metadata outside the instruction text where possible;
- edit only a clearly bounded, Skill Manager-owned file or section;
- hash the installed representation and protect local modifications;
- stage replacements and retain backups before destructive changes; and
- leave unrelated user instructions byte-for-byte unchanged.

Supporting a second agent should mean adding another destination adapter, not
changing the repository format.

## Bundle behavior

A bundle is a catalog convenience, not an ownership container.

### Install

Bulk installation should work at three scopes:

- a source offers **Install all** for every skill and rule it contains, even
  when it defines no bundles;
- a bundle offers **Install all** for its selected members; and
- every skill and rule offers its own install action.

The bundle detail view therefore offers both:

- **Install all**, which installs every missing member; and
- an individual install action beside each skill or rule.

The same items remain independently installable from the main catalog. A user
can therefore use a bundle as a recommendation without accepting the entire
selection.

Selecting **Install all** should first produce a member plan:

```text
Install     skill:python-standards
Installed   skill:git-ops
Install     rule:python
Conflict    rule:git
```

If the plan has no manual conflicts, the app installs the missing members using
the normal per-kind install path. If a member fails unexpectedly, already
completed members remain managed and the bundle becomes **Partially installed**.
The user can retry safely. Full cross-item rollback is possible later, but is
not necessary for a useful first version.

If the plan contains a conflict, no bulk changes should begin. The UI should
link to the existing adopt or replace flow for each conflicting member.

### Status

Bundle status is derived from current member state:

- **Available** — no members are installed;
- **Partially installed** — some, but not all, members are installed;
- **Installed** — every member is installed from this source;
- **Update available** — every member is installed and at least one can update;
- **Needs attention** — at least one member is modified, unmanaged, missing from
  its recorded source, or owned by another source.

The bundle detail view should retain the individual status of every member.

### Updates

Installed skills and rules continue to update as individual managed items.
Changing a bundle upstream does not silently change the user's selected set:

- adding a member does not install it automatically;
- removing a member does not uninstall it; and
- deleting the bundle does not remove any installed member.

Opening the bundle shows the difference and lets the user explicitly install
new members. This matches the app's current rule that newly discovered skills
are never installed automatically.

### Uninstall

The first bundle version should not offer blind one-click uninstall. A member
may have been installed individually or may appear in several bundles, and the
bundle is not intended to track dependency ownership.

Instead, **Review installed members** should open a preselected list. The user
can then uninstall any managed, unmodified members through the normal flow.
Reference counting and “installed because of bundle” metadata can be considered
only if this proves too limiting in real use.

## Delivery milestones

### 1. Make the default source removable

- Separate first-run source seeding from normal configuration loading so an
  initialized empty source list is preserved.
- Give the default source the same remove action and confirmation as every
  other source.
- Reuse the existing removed-source behavior for installed managed items.
- Add an explicit **Add default source** or equivalent recovery action.
- Test first launch, removal, restart, upgrade, re-addition, and removal while
  managed items from the source are still installed.

### 2. Generalize the catalog model

- Replace the skill-only catalog model with typed skill, rule, and bundle
  entries.
- Extend built-in archive extraction and custom Git sparse checkout to include
  `rules/` and `bundles/` when present.
- Validate each item kind independently and preserve source-local errors.
- Add kind filters and badges to the catalog without changing existing skill
  actions.
- Keep skills-only repositories and caches backward compatible.

### 3. Ship one rule target end to end

- Complete the target and scope spike described above.
- Implement rule discovery, detail display, install, update, conflict,
  replacement, backup, and uninstall.
- Record source ID, source URL, source commit, content digest, target, and scope
  in rule ownership metadata.
- Test unmanaged matches, unmanaged differences, local modification, upstream
  removal, source removal, failed replacement, and cache migration.
- Confirm the supported agent actually loads the installed rule.

### 4. Add bundles

- Parse and validate bundle manifests after skills and rules are known.
- Add bundle cards and a detail view with member-by-member status.
- Keep every bundle member visible and individually installable in the main
  catalog and bundle detail view.
- Add the same preflight planning to source-level and bundle-level **Install
  all** actions.
- Derive partial, installed, update, and attention states.
- Show upstream membership changes without automatically applying them.

### 5. Expand rule destinations only when proven

- Add another user-wide agent adapter.
- Add project-scoped rules with an explicit project picker and a clear preview
  of files to be changed.
- Consider source metadata for compatible targets only if the same rule cannot
  be rendered faithfully everywhere.

### 6. Optional repository features

Add these only in response to concrete repository needs:

- a top-level manifest for multiple catalog roots or display metadata;
- bundle categories, icons, or longer descriptions;
- nested bundles with cycle detection;
- explicit cross-source references pinned to a source URL; or
- import/export of the user's selected item set.

## Deliberate non-goals for the first release

- No arbitrary install scripts or lifecycle hooks.
- No rule templating, variables, conditional evaluation, or secret injection.
- No dependency solver, version constraints, or lockfile.
- No nested or cross-source bundles.
- No automatic install of new bundle members.
- No automatic uninstall when an item or bundle disappears upstream.
- No silent edits to unmanaged agent or project instruction files.
- No requirement that a repository contain all three content kinds.

## Decisions to make before implementation

The repository shape and bundle model can move forward as proposed. Rule
delivery needs three explicit product decisions:

1. **First target:** which agent should receive installed rules first?
2. **First scope:** user-wide only, or project-scoped rules in the first release?
3. **Rule composition:** does the target support managed fragments, or will the
   app own a marked section within an existing instruction file?

The narrow recommendation is one target, user-wide scope, and the smallest
managed surface the target actually loads. Once that path is verified end to
end, the same canonical `rules/<name>.md` files can support additional adapters
without changing source repositories.
