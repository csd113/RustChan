# RustChan Setup Guide

RustChan runs on Linux, macOS, and Windows as one executable with embedded
SQLite, templates, and static assets. The source version is `1.7.1`.
[README.md](README.md) covers downloads and the quick start;
[the container guide](docs/containers.md) covers Docker and Compose.

## Requirements

- Rust 1.99 or newer and Git when building from source; CI uses Rust 1.99.0.
- A writable data directory, next to the executable by default or selected with
  an absolute `--data-dir` path.
- Optional FFmpeg for video thumbnails/transcoding and uncovered audio codecs.

No separate database server, Tor daemon, native image library, standalone
`ffprobe`, or external PDF renderer is required. Built-in Tor hosting uses Arti.
See [media capabilities](docs/media-capabilities.md) for formats and limitations.

## Build and run

Install Rust using [rustup](https://rustup.rs). On Linux and macOS:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version
```

On Windows, install `rustup-init.exe` and open a new PowerShell terminal.
Then build:

```sh
git clone https://github.com/csd113/RustChan.git
cd RustChan
cargo build --locked --release
./target/release/rustchan-cli
```

On Windows, run `./target/release/rustchan-cli.exe`. An interactive first run
prompts for an administrator and optionally a board. A headless deployment can
use the fresh-instance browser wizard at <http://localhost:8080/setup>. Complete
setup before allowing public access. The site is at <http://localhost:8080> and
administration is at <http://localhost:8080/admin>.

Explicit service arguments:

```sh
./target/release/rustchan-cli --data-dir /absolute/path/to/rustchan-data serve
./target/release/rustchan-cli --data-dir /absolute/path/to/rustchan-data --port 9090 serve
```

Running without a subcommand also starts the server. Use `--help` and
`admin --help` for the current command reference.

## Install FFmpeg

FFmpeg is optional unless `require_ffmpeg = true`. Install a codec-enabled build:

| Platform | Installation |
| --- | --- |
| Debian / Ubuntu / Raspberry Pi OS | `sudo apt install ffmpeg` |
| Fedora | `sudo dnf install ffmpeg` (codec availability depends on enabled repositories) |
| macOS with Homebrew | `brew install ffmpeg` |
| Windows with winget | `winget install --id Gyan.FFmpeg -e` |

Ensure the executable is on the service account's `PATH`, or configure
`ffmpeg_path` in `settings.toml`. Check the actual installed capabilities:

```sh
ffmpeg -version
ffmpeg -encoders
ffmpeg -muxers
ffmpeg -decoders
```

VP9/Opus WebM output requires `libvpx-vp9`, `libopus`, and the `webm` muxer.
AV1 inputs additionally need an AV1 decoder; output remains VP9/Opus, so no AV1
encoder is required. Rust handles WebP encoding. Development packages such as
`libvpx-dev` and `libopus-dev` are unnecessary for building RustChan.

Missing tools/codecs leave valid original uploads available and use preview
placeholders where needed. Image processing and supported audio/PDF previews
continue to work without FFmpeg. Review capability warnings in startup logs.

## First-run files and layout

Settings are read from the data directory regardless of the working directory.
Keep the entire directory when moving the site or replacing the executable:

```text
rustchan-data/
├── settings.toml
├── chan.db                 # SQLite WAL/SHM files may be present alongside it
├── boards/                 # Uploads and thumbnails
├── backups/
│   ├── full/
│   └── boards/
├── logs/
├── runtime/
│   ├── tls/
│   ├── tor/
│   │   ├── state/          # Persistent onion identity
│   │   └── cache/
│   ├── favicon/
│   └── tmp/
└── .software-updates/       # Eligible native Linux update/recovery state
```

RustChan generates `settings.toml` and a random `cookie_secret` on first run.
Keep the secret stable and private; rotation invalidates signed cookies.
Daily log files live under `logs/`. Custom database, upload, backup, certificate,
or other persistent paths outside this tree need their own backup and mounts.

## Important settings.toml options

The generated file is the complete setting reference. Matching `CHAN_*`
environment overrides and launcher arguments affect the effective configuration.
These common defaults apply to a generated native installation:

```toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
port = 8080
max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150
enable_tor_support = true
tor_only = false
require_ffmpeg = false
ffmpeg_timeout_secs = 600

[tls]
enabled = false
require_https = false
port = 8443
redirect_http = false
http_port = 8080
```

Most controls are available in the admin panel. Database-owned appearance and
board settings apply live. File-backed runtime settings generally require a
restart; saving never restarts the process automatically. The panel shows active,
saved, and next-start values, including environment overrides.
[Settings restarts](docs/settings-restarts.md) lists the classification and
supported supervision/recovery contracts. Changing storage paths does not move data.

New boards allow images and video by default; audio, PDF, arbitrary files,
CAPTCHA, and NSFW status are opt-in. Arbitrary uploads also require the global
file gate. Self-edit/delete permissions are board-controlled; the global signed
self-action window defaults to 60 seconds and can be set from 1 to 3,600 seconds.

## Tor onion service

Native generated settings enable the built-in onion service by default. Docker
sets `CHAN_TOR_SUPPORT=false`. With Tor enabled, startup bootstraps Arti and
creates or loads the persistent onion keypair. Outbound network access and write
access to `runtime/tor/` are required.

Back up `runtime/tor/state/` privately and preserve its permissions. Losing this
state changes the onion address. A normal board backup does not preserve it.
Automatic full backups include Tor keys according to the saved inclusion setting.

For a loopback listener with onion access:

```toml
enable_tor_support = true
tor_only = true
```

Tor-only mode binds locally; it does not eliminate the operator's ability to
observe or control the site. In containers it binds inside the container, so a
published Docker port is unsuitable for this mode.

## HTTPS and TLS

Native HTTPS is disabled until `[tls].enabled = true`. Default builds include
self-signed certificate support for local testing. Configure manual certificates
or build with `--features tls-acme` and configure `[tls.acme]` for ACME.

Enabling TLS normally adds an HTTPS listener while keeping the main HTTP
application listener. Set `require_https = true` to disable public plaintext
application access. Separately, `redirect_http = true` makes `tls.http_port` a
redirect listener. With built-in Tor, HTTPS-only mode retains a loopback backend
restricted to connections registered by the in-process Tor proxy.

For public access, use valid certificates and configure the intended public
hosts. Administrator restarts and updates require a usable local readiness probe;
review [settings restarts](docs/settings-restarts.md) before listener changes.

## Linux service setup

Use a dedicated unprivileged account. This example installs a root-owned binary;
software replacement is handled by the operator. Built-in native installation
requires the ownership layout in [software updates](docs/software-updates.md).

```sh
sudo useradd --system --home /var/lib/rustchan --create-home --shell /usr/sbin/nologin rustchan
cargo build --locked --release
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli
sudo install -d -o rustchan -g rustchan -m 0700 /var/lib/rustchan
```

Create `/etc/systemd/system/rustchan.service`:

```ini
[Unit]
Description=RustChan
After=network-online.target
Wants=network-online.target

[Service]
User=rustchan
Group=rustchan
WorkingDirectory=/var/lib/rustchan
StateDirectory=rustchan
StateDirectoryMode=0700
ExecStart=/usr/local/bin/rustchan-cli --data-dir /var/lib/rustchan serve
Restart=on-failure
RestartSec=5
TimeoutStopSec=90
SendSIGKILL=no
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

Then:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now rustchan
sudo journalctl -u rustchan -f
```

Complete the browser setup through trusted local access. `StateDirectory` keeps
the selected data directory writable by the service account. For overrides, use
`sudo systemctl edit rustchan`, for example:

```ini
[Service]
Environment=CHAN_BIND=127.0.0.1:8080
Environment=CHAN_REQUIRE_FFMPEG=true
```

The separate updater/polkit files in `deploy/systemd/` describe older managed
installations. Do not use them as the current single-executable service setup.

## Reverse proxy notes

Point nginx or Caddy at the local RustChan HTTP listener and terminate TLS at the
proxy. Set `CHAN_BEHIND_PROXY=true` and `CHAN_TRUSTED_PROXY_CIDRS` to the proxy's
actual network. Configure allowed public hosts. Keep the backend inaccessible
from untrusted networks; forward client/protocol headers only through trusted
proxies, and preserve private permissions on settings and runtime state.

RustChan accepts bounded `Content-Length` bodies and rejects `Transfer-Encoding`
at its application boundary. Configure the proxy to dechunk requests before
forwarding. Header limits are 32 KiB per value and 64 KiB in aggregate.

## Observability endpoints

`/healthz` is public and minimal. `/readyz` returns readiness status and the
running package version. Detailed readiness is disabled, and `/metrics` returns
404 unless enabled:

```toml
public_readiness_details = true
public_metrics_enabled = true
```

These expose database, backup, media, maintenance, and Tor operational state.
Enable them only behind a trusted scrape/network boundary. They never expose
software-update availability or transactions.

## Admin bootstrapping

The terminal or browser wizard avoids putting passwords in command arguments.
For scripted administration, use the same absolute data directory:

```sh
rustchan-cli --data-dir /var/lib/rustchan admin create-admin admin '<strong-password>'
rustchan-cli --data-dir /var/lib/rustchan admin create-board tech 'Technology' 'Programming and hardware'
rustchan-cli --data-dir /var/lib/rustchan admin list-admins
rustchan-cli --data-dir /var/lib/rustchan admin list-boards
rustchan-cli --data-dir /var/lib/rustchan admin reset-password admin '<new-strong-password>'
rustchan-cli --data-dir /var/lib/rustchan admin db-status
```

Run these as the service account with its configuration environment. Command
arguments may be visible to local process inspection and shell history. Creating
an administrator disables the fresh-instance browser wizard.

## Terminal console

Interactive terminals show six destinations: Overview, Tasks, Content, Logs,
System, and Configuration. Services without a terminal run without the console.
Use `1`–`6` or Tab to navigate and `?`/`H` for the built-in key reference.
`R` refreshes metrics; `/` filters lists; `S` changes sort/state/log level;
Enter opens details; `P`/`F` pauses/follows logs. `C` creates a board, `A` creates
an administrator, and `D`/`X` starts a confirmed thread deletion. `Q` prompts for
graceful shutdown; Ctrl-C stops the server.

The minimum terminal size is 44 columns by 14 rows. Configuration is read-only;
use the web admin or settings file to change it. Request and active-IP counts are
process-local observations, rather than user-session counts. Task ages do not
provide an ETA, and the console has no general job-cancellation operation.

## Banner artwork requirements

Board and home announcement banners accept supported bitmap formats, including
PNG, JPEG, GIF, and WebP, with the exact 468:60 aspect ratio and a minimum of
468×60 pixels; 936×120 is recommended. Animated GIF banners are limited to
60 frames. Processing targets WebP, with validated GIF retention on a supported
conversion fallback. The [offline banner maker](docs/rustchan-banner-maker.html)
can generate artwork in a browser.

Board banners appear on the board index and catalog according to their placement
settings, not on thread, archive, or search pages. Home announcements use a
separate centered banner. External banner links are opt-in and use an on-site
warning before redirecting.

## Backups and updating

The admin panel creates full-site or per-board application backups and supports
validated restore. Full-site backups include the database, settings, managed
media and appearance assets, and optionally Tor keys. They do not replace a
complete filesystem backup of runtime/TLS state and custom external paths.

Stop the service before copying its data directory so SQLite and files are
consistent. Preserve `chan.db` and any WAL/SHM files, settings, boards, runtime
secrets, and configured external storage. Keep independent/offsite copies and
exercise recovery on a disposable instance.

Use [software updates](docs/software-updates.md#manual-upgrade-of-a-source-install)
for stopped manual upgrades and matching data-and-binary rollback; containers use
[their deployment guide](docs/containers.md). Current startup verifies and repairs
recognized older schemas before recording the current package version. Unknown,
partial, or corrupt layouts are rejected. Never lower `schema_version` or run an
old executable against a migrated database. The [database guide](docs/sqlite-engineering.md)
explains migrations and maintenance.

Change backup storage in Admin → Backups or `backup_directory`, then restart.
Existing backups stay in the previous location; only the active location appears
in the panel. Mount external storage before startup. A Docker backup path outside
`/data` needs a separate persistent mount.

## Thread archives and retention

Background maintenance enforces each board's non-sticky active-thread cap.
Threads are retained by most recent bump, then descending thread ID; locked
threads count and sticky active threads do not. Overflow can be visible briefly
while durable jobs run. Replies stop bumping at the bump limit; sage replies
count toward it. There is no time-based thread expiry.

Automatic archival occurs when the board's **Archive overflow threads** setting
or global `archive_before_prune` safety net is enabled. Hard deletion requires
both to be off. **Max archived threads** is a count cap of 1–10,000, including
archived sticky threads; it retains recent bumps with ascending thread-ID ties.
Lowering the cap can permanently delete archives. Archived threads keep their
URLs/media and are read-only, including polls. Search includes them; active
index/catalog views do not. There is no public unarchive/preservation control.

Board settings apply live; changing the global file/environment safety net needs
a restart. Failed media deletion stays queued for restart-safe retry. Monitor
background-job failures when retention or cleanup is not progressing.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| FFmpeg warnings or missing video previews | Service `PATH`/`ffmpeg_path`, installed decoders/encoders/muxer, and logs |
| Tor never becomes ready | Outbound connectivity, state-directory write permission, and bootstrap timeout |
| Onion address changed | Persistent `runtime/tor/state/` was lost, replaced, or not mounted |
| HTTPS startup fails | TLS enablement, port availability, certificate paths, and ACME settings/features |
| Uploads fail | Data/upload/runtime write permission, free disk space, media limits, and worker errors |
| Database startup fails | `admin db-status`, startup schema error, backup integrity; preserve data and investigate |
| Settings saved but inactive | Restart status and environment/launcher overrides in the admin panel |

Inspect `logs/rustchan.YYYY-MM-DD.log` in the selected data directory. Sanitize
logs before sharing them; use [SUPPORT.md](SUPPORT.md) for help and
[SECURITY.md](SECURITY.md) for vulnerabilities and operational security limits.
