# ADR 0002: Source repositories and locators

- Status: superseded in part
- Date: 2026-08-15
- Superseded by: Git locators and user-authored URLs in [ADR 0003](0003-artifact-only-catalog.md)

## Context

Skill Manager treated a source as one Git URL whose default branch publishes `skill-manager.json`. Users added those URLs one at a time. Publishers also want a browseable catalog of sources and a way to serve the same tree from a raw HTTPS artifact host such as Nexus, without a Nexus-specific API, stored credentials, or version pins.

Calling a Git clone a "source repository" made the catalog idea hard to name. Acquisition, identity, and `sources.json` were Git-shaped end to end. The built-in Skillbook `sourceKey` is a hash of its Git URL and must not change.

## Decision

Skill Manager distinguishes three terms:

1. A **source** is a tree with top-level `skill-manager.json`.
2. A **source repository** is a catalog document that lists source locators. Adding it never writes `sources[]`. Removing it never uninstalls opted-in sources.
3. A **locator** is `git` or `artifact`. The client sends `{ kind, url }`; the app does not infer kind from the string.

Git identity stays `sha256` of the canonical URL with `.git` stripped. Artifact identity is `sha256("artifact:" + canonical HTTPS URL)`. A Git clone and a zip of the same tree are different identities. Artifact URLs are HTTPS only, keep query strings, and never carry credentials.

A catalog uses the filename `skill-manager-repository.json` so a Git URL cannot be both things. An artifact catalog is the JSON document at the URL, not an archive. After opt-in, the fetched `skill-manager.json` owns `source.id`, name, and packages. An optional listing `sourceId` is a hint; disagreement fails opt-in.

`sources.json` becomes version 5 with a `repositories` array and a `locator` on each source. Version 4 is read by wrapping each `url` as Git. Ledger v4 is unchanged: `source_url` stores the locator URL and `commit` stores the revision token (Git SHA or artifact digest).

## Consequences

Users can browse a catalog and add only the sources they want. Publishers can overwrite a stable `…-latest.zip` or raw JSON path. Failed refresh still leaves the last validated snapshot active.

This cut does not add stored credentials, `http://` LAN Nexus, Maven coordinates, version pins, auto-subscribe, or nested catalogs. Those remain follow-ups.
