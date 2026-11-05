# Run RustChan with Docker

Docker runs RustChan in a container with FFmpeg already installed. Your site data
lives in a separate volume, which you keep when replacing the container. You need
Docker running on your computer or server before starting.

[All guides](../README.md#documentation) · [Native app setup](../SETUP.md) · [Public deployment](deployment.md)

## Start a local site

Run in a shell that supports backslash line continuations, such as bash or zsh.
In PowerShell, enter the `docker run` command on one line without the backslashes.

```sh
docker pull ghcr.io/csd113/rustchan:latest
docker volume create rustchan-data
docker run -d --name rustchan --restart unless-stopped --stop-timeout 90 \
  -p 127.0.0.1:8080:8080 \
  -v rustchan-data:/data \
  ghcr.io/csd113/rustchan:latest
```

Open <http://localhost:8080/setup> on the same computer. Choose **Local/testing →
load defaults**, enter your site name, administrator details, and first board,
then select **review setup** and **finish setup**. Visit <http://localhost:8080>
for the site and <http://localhost:8080/admin> to manage it.

The port mapping above allows local access only. The `rustchan-data` volume stores
the site. Keep it. `latest` is a moving image tag and can differ from the latest
published native ZIP; see [image versions](#images-and-architectures).

## Everyday commands

```sh
docker logs -f rustchan   # Ctrl-C stops viewing logs, not the container
docker stop rustchan     # Stop the site
docker start rustchan    # Start the same site again
```

For command-line help:

```sh
docker exec rustchan rustchan-cli --data-dir /data admin --help
```

See [administration](administration.md#command-line-administration) for account
commands. Creating the first administrator from the command line closes the
browser setup wizard.

## Docker Compose

The repository includes [compose.yaml](../compose.yaml). Unlike the local command
above, its `8080:8080` port mapping exposes HTTP on the host's network interfaces.
For a local trial, change that entry to `127.0.0.1:8080:8080` before starting.
From the folder containing the Compose file:

```sh
docker compose up -d
docker compose ps
docker compose logs -f rustchan
```

Finish the same browser setup. Use `docker compose stop` and
`docker compose start` to stop and start the existing site. `docker compose down`
keeps named volumes; **`docker compose down -v` deletes them and their site data**.

To choose a particular image, set `RUSTCHAN_IMAGE` before running Compose. For
example, use `ghcr.io/csd113/rustchan:sha-<full-commit-SHA>` with an actual
published image SHA. `docker compose ps` shows the image's readiness health check.

## Data and settings

The image runs as `rustchan` (UID/GID 10001) and starts
`rustchan-cli --data-dir /data serve`. It includes CA certificates and FFmpeg;
templates and static website files are embedded in the binary.

`/data` contains `settings.toml`, `chan.db` and any SQLite sidecars, `boards`,
`backups`, `logs`, and `runtime` (including Tor identity and TLS material).
Keep the **whole volume** across upgrades. If you change `CHAN_DB`, `CHAN_UPLOADS`,
or `CHAN_BACKUP_DIRECTORY` to a path outside `/data`, mount that path persistently too.

The image sets these environment overrides:

| Variable | Image default |
| --- | --- |
| `CHAN_HOST` | `0.0.0.0` inside the container |
| `CHAN_PORT` | `8080` |
| `CHAN_TOR_SUPPORT` | `false` |

Environment variables take priority over `/data/settings.toml`. After saving
startup settings in the admin panel, stop and start the container. When changing
its environment or image, recreate it using the same volume instead. Check logs
and `/readyz` afterward. RustChan does not restart its own container or undo a
bad configuration automatically. See [settings and recovery](settings-restarts.md).

Named volumes receive the image's `/data` ownership automatically. For a bind
mount, create the host directory and give UID/GID 10001 write access first. Keep
the generated cookie secret stable and private; it does not need to be supplied
separately for a fresh site.

## Public access, HTTPS, and Tor

Finish setup before allowing visitors. A Docker port mapping provides HTTP, not
HTTPS. Use a TLS reverse proxy or configure built-in TLS in `/data/settings.toml`
and publish the intended TLS port. If a proxy terminates TLS, configure
`CHAN_BEHIND_PROXY=true` and trusted proxy CIDRs for its actual network. Follow
the [deployment guide](deployment.md#reverse-proxy) for headers and request bodies.

To enable the onion service, set `CHAN_TOR_SUPPORT=true`. Its identity persists
under `/data/runtime/tor/state`. Do not use `CHAN_TOR_ONLY=true` with a published
Docker HTTP port: that mode binds to loopback inside the container.

## Update the container

1. Choose the new image and record the old image revision.
2. Stop the site and take a complete [volume backup](#backups), including any
   external persistent mounts.
3. Pull the new image and recreate the container with the same volumes.
4. Check `/readyz`, logs, administrator sign-in, boards, posts, and media.

For Compose, after the stopped backup:

```sh
docker compose pull
docker compose up -d
```

For the standalone example, pull the chosen image, stop and remove the old
**container**, then repeat `docker run` with the same `rustchan-data` volume.
Do not delete the volume. Rollback requires restoring the matching stopped data
and old image together; see [software updates](software-updates.md#manual-upgrade-of-a-source-install).
The admin Software Updates page provides release links, not an install action.

## Backups

Use the [admin backup tools](backups.md) for everyday backups. For a complete
filesystem copy, stop the container first. The following POSIX-shell example
backs up the named volume from the standalone command above:

```sh
docker stop rustchan
install -d -m 0700 "$PWD/rustchan-backups"
docker run --rm -v rustchan-data:/data:ro -v "$PWD/rustchan-backups":/backup \
  debian:trixie-slim sh -c 'umask 077; tar -C /data -czf /backup/rustchan-data.tar.gz .'
docker start rustchan
```

Confirm the archive command succeeded before relying on the copy. Move each
successful archive to a separate private backup location; the example filename
is reused on the next run. The archive contains secrets and may be root-owned
on the host. Preserve permissions when copying or restoring it.

For Compose, use `docker compose stop/start` and the actual volume name shown by
`docker volume ls` (usually `<project>_rustchan-data`). Restore into a stopped
instance's volume. Include any custom external storage in the same backup plan.

## Images and architectures

The image is `ghcr.io/csd113/rustchan`, built for `linux/amd64` and `linux/arm64`.
Docker selects the matching architecture. Docker Desktop can run these Linux
containers on compatible Macs and Windows computers.

The publishing workflow assigns a `sha-<full-commit-SHA>` tag to each published
commit. Stable version tags produce version and major/minor image tags, such as
`1.7.5` and `1.7`; main-branch builds and stable semantic-version tags also update
`latest`. Pin a published SHA or image digest for repeatable deployment. A SHA
name alone does not prevent registry owners from moving or deleting a tag.
Not every working-branch commit produces an image.

The release tag must match the Cargo package version. GHCR package visibility is
controlled in GitHub's package settings. The health check requests `/readyz` on
the configured HTTP port; adapt your deployment's health check if you disable or
redirect that listener. `/readyz` reports the running package version, not update
availability.
