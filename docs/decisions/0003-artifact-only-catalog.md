# ADR 0003: Artifact-only catalog distribution

- Status: accepted
- Date: 2026-08-15
- Supersedes: Git locators and user-authored URLs from [ADR 0002](0002-source-repositories-and-locators.md)

## Context

ADR 0002 introduced source repositories and two locator kinds, Git and Artifact. Users pasted URLs and chose a kind. Company Git is private, so each source required a separate access request. Skillbook was a built-in Git source and is a personal library, not a company default. The company will publish sources from Nexus as HTTPS archives.

## Decision

Skill Manager fetches only HTTPS artifact URLs. Git acquisition, locator kinds, user-authored URLs, and the built-in Skillbook source are removed.

A source repository remains a catalog document. Listed sources are a name, a description, and an HTTPS URL. Adding a catalog never writes `sources[]`. Adding a listed source fetches that archive so its packages become available; it does not install them.

The company catalog URL is a build-time constant. When it is set, sync adds that catalog if it is missing. Users add and remove listed sources. They cannot add a catalog or a source by URL.

`sources.json` version 6 stores artifact locators as `{ "url" }`. Earlier versions, including Git sources, are refused.

Source identity stays `sha256("artifact:" + canonical URL)`. Repository identity stays `sha256("artifact\0" + canonical URL)`.

## Consequences

The desktop app no longer requires system Git. Publishers still author in Git if they want; CI uploads a zip to Nexus and the catalog points at that URL.

The default catalog is `https://repo.ragsdale.dev/repository/files/catalogs/skill-manager-repository.json`. It currently lists Skillbook at `…/sources/skillbook-latest.zip` and Hello World at `…/sources/hello-latest.zip`. Stored credentials, package-level access rules, LAN HTTP, and version pins remain later work.

Access control later attaches to package identity and to whether the HTTP client can fetch a URL, not to Git ACLs.
