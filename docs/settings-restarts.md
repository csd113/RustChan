# Change settings and restart

Use the admin panel for everyday changes. Saving a setting does not restart
RustChan. Some changes take effect immediately; others take effect when you stop
and start the app.

[All guides](../README.md#documentation) · [Administration](administration.md) · [Deployment](deployment.md)

## Apply a change

1. Save the setting in the admin panel.
2. Read **Configuration restart**. It compares the running, saved, and next-start
   values and shows overrides.
3. If a restart is needed, stop RustChan normally and wait for it to exit.
4. Start it with the same launcher, account, environment, and data directory.
   Check the site and `/readyz`.

For Docker, use `docker stop rustchan` then `docker start rustchan`; for Compose,
use `docker compose stop` then `docker compose start`. If changing container
configuration such as environment variables, recreate the container with the
same volume. A restart never includes unsaved form edits.

## Where settings live

Server settings are in `settings.toml` inside your data directory. RustChan
creates it with comments on first run. Read that generated file for the complete
list. Board settings and many appearance options are stored in the database and
managed through the admin panel.

For example, to keep a native trial local with Tor off, stop the app and set
these top-level entries in `settings.toml` (before any `[section]`):

```toml
bind_addr = "127.0.0.1:8080"
enable_tor_support = false
```

Restart with the same data directory. Matching `CHAN_*` environment variables
can override file values. `CHAN_HOST` changes the listener interface,
`CHAN_BIND` supplies an address and port, and launcher `--port` overrides the
final port. Docker's `CHAN_TOR_SUPPORT=false`, for example, keeps Tor off even
if the file enables it. Review the admin panel's effective values.

Common generated native defaults are:

| Setting | Default | Purpose |
| --- | --- | --- |
| `forum_name` | `RustChan` | Site name |
| `site_subtitle` | `select board to proceed` | Initial subtitle |
| `port` | `8080` | HTTP port; default interface is `0.0.0.0` |
| `max_image_size_mb` | `8` | Default image limit in MiB |
| `max_video_size_mb` | `50` | Default video limit in MiB |
| `max_audio_size_mb` | `150` | Default audio limit in MiB |
| `enable_tor_support` | `true` | Built-in onion service |
| `tor_only` | `false` | Restrict HTTP to loopback for onion access |
| `require_ffmpeg` | `false` | Require FFmpeg at startup |
| `ffmpeg_timeout_secs` | `600` | Media-processing time budget |
| `[tls].enabled` | `false` | Built-in HTTPS |
| `[tls].require_https` | `false` | Disable public plaintext application access |
| `[tls].port` | `8443` | HTTPS port |
| `[tls].redirect_http` | `false` | Enable HTTP-to-HTTPS redirect |
| `[tls].http_port` | `8080` | Redirect listener port |

Changing a storage path does not move data. Keep a private copy of working
settings before changing startup controls. Rotating the cookie secret changes
signed-cookie/CSRF validation and client IP hashes used by bans; keep it stable
unless rotation is intentional. Existing administrator sessions retain their
stored expiry when only the configured session lifetime changes.

## Recover from a bad setting

If RustChan will not start after a change, stop it, restore your known-good
settings, and check environment/launcher overrides before starting again. A
settings-only problem does not require rolling back the database or media.
Listener changes may also need new browser addresses, proxy rules, or firewall
settings. Old unfinished restart journals require matching-version recovery;
see [older updater recovery](software-updates.md#retiring-an-older-updater).

## Which settings need a restart?

The table is a reference for operators and developers. The authoritative mapping
is `SettingDefinition.application` in [`src/config/admin.rs`](../src/config/admin.rs)
and its runtime registry. Browser flags do not determine whether a restart is needed.

**These settings apply live when saved through the admin panel:**

- Site name/subtitle, activity badges, default NSFW visibility, themes/custom CSS,
  favicon, banners, rotation, and external banner links.
- Board names, ordering, access, upload/posting/display rules, and retention.
- Global active-media pruning and its cap; FFmpeg subprocess timeout.
- Automatic full-backup interval, retention, Tor-key inclusion, storage format,
  and split-part size.

**These settings require a restart:**

| Group | Controls |
| --- | --- |
| Network & Security | Proxy trust/CIDRs, public hosts, secure cookies, HTTP port/bind address, browsing rate limits/windows/policy, admin session lifetime, readiness detail, metrics |
| Tor | Enablement, Tor-only mode, bootstrap timeout, concurrent streams, service nickname |
| Media | File-backed image/video/audio defaults, arbitrary-file switch, archive-before-prune, FFmpeg requirement/path and detected capabilities, thumbnail size, job-queue capacity, waveform cache cap, all managed-media reconciliation toggles/intervals/budgets |
| Maintenance | WAL checkpoint, vacuum and poll-cleanup intervals; database size warning threshold |
| Worker capacity | Blocking threads and SQLite pool size |
| HTTPS | Every TLS setting: enablement, HTTPS-only policy, HTTPS/redirect ports and redirect toggle, ACME domains/email/staging/cache, certificate/key paths |
| Access and display | Failed-login and board-password limits/windows, board access-cookie lifetime, self-action window, board page size/reply previews, log filter, normal read/write deadlines |
| Storage and secrets | Backup directory and cookie-secret rotation; other data/database/upload paths remain operator-managed and changing them never moves files |

Startup aliases for database-owned appearance, board, and pruning settings seed
initial values; they do not overwrite existing database values. Runtime controls
initialize listeners, executors, database pools, tool discovery, logging, or worker
schedules. Manual database, content, and moderation actions apply immediately.

## Allow time to shut down

Ctrl-C, SIGTERM, and service-manager stops close new work and stop listeners,
media workers, scheduled tasks, and Tor. Accepted HTTP requests get a ten-second
drain window; tracked persistent work shares a 60-second shutdown budget. A final
WAL checkpoint is attempted, and interrupted durable jobs can resume through
startup reconciliation.

Give services and containers **90 seconds** to stop. Confirm exit before changing
settings on disk, replacing the executable, or copying/restoring data.
