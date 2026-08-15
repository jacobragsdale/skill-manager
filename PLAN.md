# Live Nexus catalog

The baked-in catalog is live on the raw Nexus `files` repository:

- Catalog: `https://repo.ragsdale.dev/repository/files/catalogs/skill-manager-repository.json`
- Skillbook: `https://repo.ragsdale.dev/repository/files/sources/skillbook-latest.zip`
- Hello World: `https://repo.ragsdale.dev/repository/files/sources/hello-latest.zip`

`DEFAULT_CATALOG_URL` in `src-tauri/src/locator.rs` points at the catalog JSON. The listing document is checked in at `catalogs/skill-manager-repository.json`. Nexus `files` uses `ALLOW_ONCE`, so a new snapshot must be published by deleting the old asset first, then uploading the same `…-latest.zip` path.

See [ADR 0003](docs/decisions/0003-artifact-only-catalog.md), [publish a catalog](docs/publish-source-repository.md), and [publish a source](docs/publish-source.md).

## Later, not this cut

- Stored Nexus credentials or SSO for restricted artifacts.
- Package-level access rules (`source.id` + `package.id`), not Git ACLs.
- `http://` LAN Nexus.
- Maven coordinates, version pins, user-authored URLs, a second user-facing catalog.
- Redeploy-in-place of `…-latest.zip` without a delete (Nexus write policy).
