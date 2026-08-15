# Next: live Nexus catalog

Git is gone. Manage Sources lists one baked-in catalog by name and description. Adding a listed source fetches its archive and does not install packages. What remains is wiring that catalog to the company Nexus and proving artifact fetch against the real host.

See [ADR 0003](docs/decisions/0003-artifact-only-catalog.md), [publish a catalog](docs/publish-source-repository.md), and [publish a source](docs/publish-source.md).

## Constraints

- HTTPS only. No credentials, no `http://` LAN Nexus, no version pins.
- Catalog JSON is the document itself, not a zip. Each listed source is a zip/tar/tar.gz with top-level `skill-manager.json`.
- The catalog URL is a build-time constant: `DEFAULT_CATALOG_URL` in `src-tauri/src/locator.rs`. It is empty today, so Manage Sources shows “catalog is not configured.”
- Old Git `sources.json` (v4/v5) is refused. Reset this app’s config/cache before testing.

## 1. Publish on Nexus

Host a raw HTTPS repository that anonymous clients can GET.

1. Upload one or two source archives at stable `…-latest.zip` URLs. Overwrite those paths when a new snapshot should ship.
2. Upload `skill-manager-repository.json` at a stable catalog URL. Each listing is `name`, `description`, and the archive URL.
3. Confirm both URLs return 200 over HTTPS without auth. `HEAD` should send `ETag` and/or `Last-Modified` if you want refresh to skip the body.

Validate before pointing the app at them:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --bin validate-source-repository -- /path/to/skill-manager-repository.json
cargo run --manifest-path src-tauri/Cargo.toml --bin validate-source -- /path/to/example-source
```

After they are live, the same binaries accept the HTTPS URLs.

## 2. Point the app at the catalog

Set `DEFAULT_CATALOG_URL` to the catalog JSON URL and rebuild. Sync adds that catalog when it is missing. Users still only add and remove listed sources.

If a previous Git-era build wrote `sources.json`, reset Skill Manager’s config and cache directories first.

## 3. Test against the live host

Exercise the real path. Do not treat unit tests as a substitute.

- First launch / Refresh loads the catalog. Manage Sources shows name and description, not URLs or locator kinds.
- Add a listed source. Confirm the dialog. Packages appear. Nothing is installed.
- Install one package, then Refresh with an unchanged archive (should reuse validators / digest).
- Overwrite a `…-latest.zip` and Refresh. The source updates; installed packages follow the existing update flow.
- Remove the source. Its packages uninstall. The catalog row stays and can be added again.
- Kill Nexus or serve 404/401 and Refresh. The last validated snapshot stays active. `catalogMessage` or the catalog refresh-failed badge explains the failure.

Fix whatever live fetch actually breaks: redirects, content types, missing `HEAD`, TLS, zip layout, digest reuse.

## 4. Later, not this cut

- Stored Nexus credentials or SSO for restricted artifacts.
- Package-level access rules (`source.id` + `package.id`), not Git ACLs.
- `http://` LAN Nexus.
- Maven coordinates, version pins, user-authored URLs, a second user-facing catalog.
