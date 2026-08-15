# Source repository reference

A source repository is a catalog document that lists sources. It is not installable and does not contribute packages. The filename is `skill-manager-repository.json` so a Git URL cannot be both a source and a catalog. Unknown fields are rejected. The generated schema is [`schemas/v1/source-repository.schema.json`](../schemas/v1/source-repository.schema.json).

## Document

```json
{
  "version": 1,
  "repository": { "id": "acme", "name": "Acme sources", "description": "Official portable sources." },
  "sources": [{ "name": "Review workflows", "description": "Review skill and database MCP server.", "locator": { "kind": "git", "url": "https://github.com/acme/review-source.git" } }]
}
```

| Field                    | Rules                                                                                                                                     |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `version`                | Required integer. Must be `1`.                                                                                                            |
| `repository.id`          | 2–32 lowercase ASCII letters, digits, or single hyphens; starts with a letter. Does not namespace packages.                               |
| `repository.name`        | 1–120 characters.                                                                                                                         |
| `repository.description` | 1–1,024 characters.                                                                                                                       |
| `sources`                | 1–200 entries. Duplicate locators after canonicalization are fatal.                                                                       |
| `sources[].name`         | 1–120 characters. Display only.                                                                                                           |
| `sources[].description`  | 1–1,024 characters. Display only.                                                                                                         |
| `sources[].locator.kind` | `git` or `artifact`. The client sends the kind; it does not infer it from the URL.                                                        |
| `sources[].locator.url`  | Git: `https://` or `ssh://`, no credentials, no query or fragment. Artifact: `https://` only, no credentials; query strings are kept.     |
| `sources[].sourceId`     | Optional hint, same charset as `source.id`. After opt-in, the fetched `skill-manager.json` is authoritative. A disagreement fails opt-in. |

Listing metadata is display-only. Installed names, conflicts, and `sourceKey` come from the opted-in source. Nested catalogs are not accepted.

The document is limited to 1 MB.

## Locators and identity

A **locator** is how Skill Manager fetches a source or a catalog.

| Kind       | Fetch                                                  | Revision                        | Identity                                                                                                                          |
| ---------- | ------------------------------------------------------ | ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `git`      | Default-branch HEAD, sparse clone of the required file | Git commit SHA                  | `sourceKey` is `sha256` of the URL with `.git` stripped, rendered as `source-` plus 16 hex characters. This formula is unchanged. |
| `artifact` | HTTPS GET of the URL                                   | SHA-256 of the downloaded bytes | `sourceKey` is `sha256("artifact:" + canonical URL)`, same `source-` prefix.                                                      |

A repository key is `repo-` plus 16 hex characters of `sha256(kind + "\0" + identity key)`. A Git URL and a zip of the same tree are different identities.

Artifact downloads follow at most five HTTPS, credential-free redirects, cap at 50 MB, and reject zip-slip, absolute paths, `..`, symlinks, and special entries. Source archives may be zip, tar, or tar.gz, detected by magic bytes. Source-repository artifacts are the JSON document itself, not an archive.

Refresh of an artifact uses `HEAD` `ETag` and `Last-Modified` when both match the stored validators. If the server sends no validators, Skill Manager GETs and compares the digest. Failed refresh leaves the last validated snapshot active.

## Configuration

`sources.json` version 5 stores repositories and sources. Version 4 files are read by wrapping each `url` as `{ "kind": "git", "url" }` and using an empty `repositories` array. Other versions are refused.

`repositoryKey` on a source is optional provenance for the UI. Removing a repository drops its config and cache only; opted-in sources stay. The UI clears provenance when that catalog is no longer configured.

See [the source manifest reference](manifest-reference.md) for package fields and [architecture](architecture.md) for acquisition.
