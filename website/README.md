# Download website

This directory contains the standalone Skill Manager download site. It serves a static landing page and reads release metadata from `releases/manifest.json`.

## Run locally

```bash
docker compose -f website/compose.yml up -d --build
```

Open <http://127.0.0.1:8080>. Set `SKILL_MANAGER_SITE_PORT` before starting Compose to use another port.

## Publish files manually

1. Create `website/releases/releases/<version>/`.
2. Copy the Windows `.exe` and Linux `.AppImage` into that directory.
3. Calculate each file's byte size and SHA-256 digest.
4. Update `website/releases/manifest.json` with paths relative to `website/releases/`, for example `releases/0.2.0/Skill-Manager-Setup.exe`.

Release binaries are intentionally ignored by Git. The tracked manifest starts in preview mode with both download buttons disabled. Changes to the mounted release directory appear without rebuilding or restarting the container.

Put an HTTPS reverse proxy in front of port 8080 for public use. The container itself serves HTTP only.
