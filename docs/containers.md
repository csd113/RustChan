# Container deployment

RustChan's container listens on HTTP port **8080**. The image includes `ffmpeg`, `ffprobe`, and CA certificates. It runs as the unprivileged `rustchan` user (UID/GID 10001). Templates and static assets are embedded in the server binary.

## Pull and run

```bash
docker pull ghcr.io/csd113/rustchan:latest
docker volume create rustchan-data
docker run -d --name rustchan --restart unless-stopped \
  -p 8080:8080 \
  -e CHAN_HOST=0.0.0.0 -e CHAN_PORT=8080 -e CHAN_TOR_SUPPORT=false \
  -v rustchan-data:/data \
  ghcr.io/csd113/rustchan:latest
```

Open <http://localhost:8080>. Create an administrator with `docker exec rustchan rustchan-cli --data-dir /data admin create-admin admin '<strong-password>'`; use a unique password. For a public site, put RustChan behind a TLS reverse proxy or configure its built-in TLS in `/data/settings.toml` and publish the TLS port as well. For a TLS-terminating proxy, set `CHAN_BEHIND_PROXY=true` and configure `trusted_proxy_cidrs` for the proxy's network. The Docker port mapping alone serves HTTP.

The image starts `rustchan-cli --data-dir /data serve`. On first start RustChan creates `/data/settings.toml` with a random cookie secret and initializes `/data/chan.db`. The `/data` volume also contains `/data/boards` (uploads), `/data/backups`, `/data/logs`, and `/data/runtime` (including Tor identity and TLS material). Keep the **whole directory** across upgrades. SQLite journal files live beside `chan.db` and must stay on the same volume. If you override `CHAN_DB`, `CHAN_UPLOADS`, or `CHAN_BACKUP_DIRECTORY`, keep those paths under `/data` or mount them separately.

The image sets `CHAN_HOST=0.0.0.0`, `CHAN_PORT=8080`, and `CHAN_TOR_SUPPORT=false`. `CHAN_*` variables override `/data/settings.toml`; that file documents the other settings. To enable the built-in onion service, set `CHAN_TOR_SUPPORT=true`; its identity is persisted under `/data/runtime/tor/state`. Do not set `CHAN_TOR_ONLY=true` for a published Docker port: that mode binds to loopback inside the container. Keep the generated cookie secret stable. No database service or external secret is needed for the basic setup.

For a bind mount, create the host directory and give UID/GID 10001 write access before starting the container. A Docker named volume is initialized with the image's `/data` ownership automatically.

## Docker Compose

The repository's [compose.yaml](../compose.yaml) provides the same port, environment, restart policy, and persistent named volume. From the repository root:

```bash
docker compose up -d
docker compose ps
docker compose logs -f rustchan
```

To run a specific published SHA image before `latest` is available, set `RUSTCHAN_IMAGE=ghcr.io/csd113/rustchan:sha-<full-commit-SHA>` before `docker compose up -d`. The same override can pin later deployments to a tested revision.

Create an administrator with `docker compose exec rustchan rustchan-cli --data-dir /data admin create-admin admin '<strong-password>'`. The image health check requests `/readyz`, which checks database readiness. `docker compose ps` shows its status.

To upgrade the Compose deployment:

```bash
docker compose pull
docker compose up -d
```

The named volume survives `docker compose down`; avoid `docker compose down -v` unless you intend to delete all site data. Use `docker compose stop` and `docker compose start` to stop and restart. For the standalone container, use `docker logs -f rustchan`, `docker stop rustchan`, and `docker start rustchan`. To replace it after pulling a new image, stop and remove the container, then repeat the `docker run` command with the **same** `rustchan-data` volume.

## Backups

Stop RustChan before taking a filesystem copy of `/data` so the SQLite database and uploads are consistent. Back up the complete named volume, including `settings.toml`, `chan.db` and any journal files, `boards`, `backups`, and `runtime/tor/state`. For example, after `docker stop rustchan`:

```bash
docker run --rm -v rustchan-data:/data:ro -v "$PWD":/backup \
  debian:bookworm-slim tar -C /data -czf /backup/rustchan-data.tar.gz .
docker start rustchan
```

For Compose, use `docker compose stop` and `docker compose start` and replace `rustchan-data` in the backup command with the volume name shown by `docker volume ls` (normally `rustchan_rustchan-data`). Restore into a stopped instance's volume before starting it. RustChan also has application-level backup tools in the admin panel; keeping an independent copy of the entire volume covers the instance configuration and onion identity.

## Images and architectures

GHCR image: `ghcr.io/csd113/rustchan`. The publishing workflow builds `linux/amd64` and `linux/arm64`. `latest` tracks the main branch and stable semantic-version tags. Every published commit receives an immutable `sha-<full-commit-SHA>` tag. A tag such as `v1.5.0` also produces `1.5.0` and `1.5` tags; the release tag must match the Cargo package version under the repository's release workflow. Pin a SHA tag for a repeatable deployment. Package visibility in GHCR is controlled in GitHub's package settings.
