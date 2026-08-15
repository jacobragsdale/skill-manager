# Source repository reference

A source repository is a catalog document that lists sources. It is not installable and does not contribute packages. The conventional filename is `skill-manager-repository.json`; an artifact catalog is the JSON document at the URL, not an archive. Unknown fields are rejected. The generated schema is [`schemas/v1/source-repository.schema.json`](../schemas/v1/source-repository.schema.json).

## Document

```json
{
  "version": 1,
  "repository": { "id": "acme", "name": "Acme sources", "description": "Official portable sources." },
  "sources": [{ "name": "Review workflows", "description": "Review skill and database MCP server.", "url": "https://nexus.example.com/repository/raw/sources/review-latest.zip" }]
}
```

| Field                    | Rules                                                                                                                                     |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `version`                | Required integer. Must be `1`.                                                                                                            |
| `repository.id`          | 2–32 lowercase ASCII letters, digits, or single hyphens; starts with a letter. Does not namespace packages.                               |
| `repository.name`        | 1–120 characters.                                                                                                                         |
| `repository.description` | 1–1,024 characters.                                                                                                                       |
| `sources`                | 1–200 entries. Duplicate URLs after canonicalization are fatal.                                                                           |
| `sources[].name`         | 1–120 characters. Display only.                                                                                                           |
| `sources[].description`  | 1–1,024 characters. Display only.                                                                                                         |
| `sources[].url`          | HTTPS artifact URL of a source archive. No credentials. Query strings are kept.                                                           |
| `sources[].sourceId`     | Optional hint, same charset as `source.id`. After opt-in, the fetched `skill-manager.json` is authoritative. A disagreement fails opt-in. |

Listing metadata is display-only. Installed names, conflicts, and `sourceKey` come from the opted-in source. Nested catalogs are not accepted.

The document is limited to 1 MB.

## Locators and identity

A locator is an HTTPS URL. Agent Plugins downloads the bytes and uses the SHA-256 digest as the revision.

| Kind     | Fetch                | Revision                        | Identity                                                                                            |
| -------- | -------------------- | ------------------------------- | --------------------------------------------------------------------------------------------------- |
| Artifact | HTTPS GET of the URL | SHA-256 of the downloaded bytes | `sourceKey` is `sha256("artifact:" + canonical URL)`, rendered as `source-` plus 16 hex characters. |

A repository key is `repo-` plus 16 hex characters of `sha256("artifact\0" + canonical URL)`.

Artifact downloads follow at most five HTTPS, credential-free redirects, cap at 50 MB, and reject zip-slip, absolute paths, `..`, symlinks, and special entries. Source archives may be zip, tar, or tar.gz, detected by magic bytes. Source-repository artifacts are the JSON document itself, not an archive.

Refresh of an artifact uses `HEAD` `ETag` and `Last-Modified` when both match the stored validators. If the server sends no validators, Agent Plugins GETs and compares the digest. Failed refresh leaves the last validated snapshot active.

## Configuration

`sources.json` version 6 stores repositories and sources as artifact locators. Earlier versions, including Git sources, are refused.

`repositoryKey` on a source is optional provenance for the UI. Removing a repository drops its config and cache only; opted-in sources stay.

The company catalog URL is a build-time constant. When it is set, sync adds that catalog if it is missing. Users add and remove listed sources from Manage Sources; they do not paste URLs.

See [the source manifest reference](manifest-reference.md) for package fields and [architecture](architecture.md) for acquisition.
