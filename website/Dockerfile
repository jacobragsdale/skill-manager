FROM caddy:2.10.2-alpine

# The official image grants Caddy permission to bind privileged ports. This
# site listens on 8080, so remove that unused file capability to keep the
# container compatible with no-new-privileges and a fully dropped cap set.
RUN setcap -r /usr/bin/caddy

COPY website/Caddyfile /etc/caddy/Caddyfile
COPY website/index.html website/source-manifest.html website/source-repository.html website/styles.css website/app.js /srv/site/
COPY website/assets/ /srv/site/assets/
COPY src-tauri/icons/icon.png /srv/site/assets/icon.png
COPY schemas/v1/source-manifest.schema.json /srv/site/schema/v1/source-manifest.schema.json
COPY schemas/v2/source-manifest.schema.json /srv/site/schema/v2/source-manifest.schema.json
COPY schemas/v1/source-repository.schema.json /srv/site/schema/v1/source-repository.schema.json

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -qO- http://127.0.0.1:8080/healthz >/dev/null || exit 1
