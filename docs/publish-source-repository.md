# Publish a source repository catalog

This tutorial publishes a catalog that lists sources. Skill Manager can browse the catalog; packages appear only after a listed source is added.

## Create the catalog document

Use this layout for a Git catalog:

```text
example-catalog/
└── skill-manager-repository.json
```

Create `skill-manager-repository.json`:

```json
{
  "version": 1,
  "repository": { "id": "acme", "name": "Acme sources", "description": "Official portable sources." },
  "sources": [
    { "name": "Review workflows", "description": "Review skill and database MCP server.", "locator": { "kind": "git", "url": "https://github.com/acme/review-source.git" } },
    { "name": "Data tools", "description": "Published from Nexus as a zip.", "locator": { "kind": "artifact", "url": "https://nexus.example.com/repository/raw/sources/data-latest.zip" } }
  ]
}
```

The document lists locators. It is not installable and does not contribute packages by itself.

## Validate locally

Validate the JSON shape against the checked-in schema:

```bash
jq empty skill-manager-repository.json
npx ajv-cli validate --spec=draft2020 --strict=false \
  -s https://raw.githubusercontent.com/jacobragsdale/skill-manager/main/schemas/v1/source-repository.schema.json \
  -d skill-manager-repository.json
```

Then run the catalog validator:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source-repository -- /path/to/example-catalog
```

You can also validate a published Git default branch or a raw HTTPS JSON URL:

```bash
cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source-repository -- --kind git \
  https://github.com/acme/source-catalog.git

cargo run --manifest-path /path/to/skill-manager/src-tauri/Cargo.toml \
  --bin validate-source-repository -- --kind artifact \
  https://nexus.example.com/repository/raw/catalogs/acme.json
```

## Publish and browse

Commit and push the catalog, or host the JSON at a stable HTTPS URL. In Skill Manager:

1. Open **Manage Sources**.
2. Under **Source repositories**, choose Git or Artifact and add the catalog locator.
3. Review the repository id, revision, and listed source count.
4. Expand the catalog and select **Add** on one listed source.
5. Confirm that source. Its `skill-manager.json` owns the namespace and packages.

Removing the catalog later does not uninstall opted-in sources. Listing changes on refresh update the browse list only.

For the field rules, see [the source-repository reference](source-repository-reference.md). To publish one of the listed sources, see [Publish a portable source](publish-source.md).
