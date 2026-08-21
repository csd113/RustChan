<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/branding/rustchan-logo-dark.svg">
  <img width="660" alt="RustChan — self-hosted imageboard" src="docs/assets/branding/rustchan-logo-light.svg">
</picture>

**A self-hosted imageboard in one Rust binary, with bundled SQLite, built-in moderation, backups, and Tor support.**

[![CI](https://github.com/csd113/RustChan/actions/workflows/ci.yml/badge.svg)](https://github.com/csd113/RustChan/actions/workflows/ci.yml)
[![Browser E2E](https://github.com/csd113/RustChan/actions/workflows/e2e.yml/badge.svg)](https://github.com/csd113/RustChan/actions/workflows/e2e.yml)

[Features](#features) · [Screenshots](#screenshots) · [Quick start](#quick-start) · [Configuration](#configuration-and-runtime-data) · [Operations](#administration) · [Development](#development)

<img width="150" alt="Rust Chan, the anthropomorphized RustChan mascot" src="docs/assets/branding/rust-chan-mascot-chibi.png">

<sub>Rust Chan — the website brought to life as its terminal-wrangling, thread-keeping mascot.</sub>

</div>

## What is RustChan?

RustChan is a self-hosted imageboard server for people who want their own corner of the web without adopting an infrastructure hobby. It provides boards, threads, replies, media uploads, moderation, backups, themes, and an operator-friendly admin panel while keeping deployment compact.

| | RustChan keeps it simple |
|---|---|
| **Runtime** | One Rust binary; no application sidecars |
| **Storage** | Bundled SQLite and one movable data directory |
| **Management** | Browser admin panel plus bootstrap and maintenance CLI commands |
| **Networking** | HTTP, optional native TLS, built-in Arti onion service, and optional ChanNet listener |
| **Media** | Images, video, audio, thumbnails, waveforms, and optional transcoding |
| **Deployment** | Suitable for a VPS, homelab, or small server; Docker, Postgres, and Redis are not required |

Current development version: `1.4.0`. Minimum supported Rust version: `1.91`.

## Features

### Boards and conversations

- Multiple boards with per-board access, posting, media, cooldown, captcha, and archive settings
- Threads, replies, catalog and archive views, and board search
- Polls, tripcodes, sage, spoiler tags, poster IDs, quote links, and thread update controls
- A shared 60-second window for editing or self-deleting a newly created post
- Responsive layouts with JavaScript-enhanced behavior and supported no-JavaScript fallbacks

### Media pipeline

- Image uploads: JPEG, PNG, GIF, WebP, HEIC, HEIF, BMP, TIFF, and SVG
- Video uploads: MP4 and WebM
- Audio uploads: MP3, OGG, FLAC, WAV, M4A, and AAC
- Opt-in PDF uploads with a separate per-board limit and preview/thumbnail fallback
- Generic file uploads behind both a global feature gate and per-board opt-in
- Streaming validation so large uploads are not buffered entirely in memory
- Optional `ffmpeg`/`ffprobe` thumbnails, WebP conversion, audio waveforms, and MP4-to-WebM transcoding

### Operator tools

- Browser-based board, appearance, theme, favicon, banner, moderation, report, and appeal management
- Full-site and per-board backup workflows, saved-backup management, uploaded restores, and scheduled backups
- Database status, integrity checking, repair helpers, media reconciliation, and `VACUUM`
- Minimal public liveness/readiness endpoints and opt-in detailed readiness and Prometheus metrics

### Security and privacy controls

- Argon2id admin password hashing
- `HttpOnly`, `SameSite=Strict` sessions and CSRF protection with constant-time token comparison
- Keyed IP hashing instead of storing or logging raw client IP addresses
- Rate limiting, security headers, and CSP-friendly rendering
- Restore preflights for traversal, oversized uploads, malformed archives, and archive-expansion abuse

## Screenshots

<p align="center">
  <img width="100%" alt="RustChan home page with boards and site statistics" src="docs/screenshots/rustchan-home.png">
</p>

<p align="center">
  <img width="100%" alt="RustChan thread with replies, quote links, media, and post controls" src="docs/screenshots/rustchan-thread.png">
</p>

<details>
<summary><strong>More views: catalog, admin panel, and mobile</strong></summary>

<p align="center">
  <img width="100%" alt="RustChan board catalog with thread cards and media thumbnails" src="docs/screenshots/rustchan-catalog.png">
</p>

<p align="center">
  <img width="100%" alt="RustChan admin dashboard settings view" src="docs/screenshots/rustchan-admin.png">
</p>

<p align="center">
  <img width="420" alt="RustChan mobile thread view" src="docs/screenshots/rustchan-mobile.png">
</p>

</details>

## Quick start

You need Rust `1.91` or newer. `ffmpeg` is optional but recommended for the complete media pipeline.

```bash
git clone https://github.com/csd113/RustChan.git
cd RustChan
cargo build --release

./target/release/rustchan-cli admin create-admin admin "ChangeThisPasswordNow"
./target/release/rustchan-cli admin create-board b "Random" "General discussion"
./target/release/rustchan-cli admin create-board tech "Technology" "Programming and hardware"

./target/release/rustchan-cli
```

Open `http://localhost:8080`; the admin panel is at `http://localhost:8080/admin`.

On Windows, use `target/release/rustchan-cli.exe`. Replace the example password before using a real deployment.

For Rust installation, `ffmpeg` setup, TLS, reverse proxies, systemd, updates, and troubleshooting, follow the complete [setup guide](SETUP.md).

## Configuration and runtime data

On first run, RustChan creates a complete runtime root at `<exe-dir>/rustchan-data/`. Use `--data-dir /absolute/path` to select a different non-root location; the live process reads `settings.toml` from that selected directory, not from the current working directory.

```text
rustchan-data/
├── settings.toml
├── chan.db
├── logs/
├── backups/
│   ├── full/
│   └── boards/
├── runtime/
│   ├── tls/
│   ├── tor/
│   ├── favicon/
│   └── tmp/
└── boards/
```

That directory is the unit to move, protect, and back up.

Fresh configuration is generated with inline documentation. Common settings include:

```toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
default_theme = "forest"
port = 8080
enable_tor_support = true
require_ffmpeg = false

[tls]
enabled = false
require_https = false
port = 8443
```

| Setting or flag | Purpose |
|---|---|
| `--data-dir /absolute/path` | Select the complete runtime root |
| `--port 9090` | Override the main listener port |
| `enable_tor_support` | Start the built-in Arti onion service; enabled in generated config |
| `tor_only` | Bind the application to loopback and serve it through Tor |
| `require_ffmpeg` | Fail startup when the enhanced media toolchain is unavailable |
| `enable_any_file_uploads_feature` | Let boards opt into generic non-media uploads; disabled by default |
| `[tls].enabled` | Enable RustChan's native TLS listener |
| `[tls].require_https` | Disable public plaintext application access when native TLS is active |
| `auto_full_backup_interval_hours` | Set the scheduled full-backup cadence |

`CHAN_*` environment variables override their matching TOML-owned values at runtime. Keep `cookie_secret` stable after launch, and treat configuration, database files, backups, TLS keys, and Tor identity keys as secrets.

<details>
<summary><strong>Database compatibility note</strong></summary>

Fresh `1.4.0` databases are created directly from the `1.4.0` baseline schema. A database already matching that baseline is marked as schema version `1.4.0`; partial or unknown schemas fail closed without deleting data.

</details>

## Running RustChan

| Mode | Command |
|---|---|
| Standard server | `./target/release/rustchan-cli` |
| Explicit server mode | `./target/release/rustchan-cli serve` |
| Different port | `./target/release/rustchan-cli --port 9090` |
| Dedicated runtime root | `./target/release/rustchan-cli --data-dir /var/lib/rustchan` |
| Server plus ChanNet | `./target/release/rustchan-cli serve --chan-net` |
| CLI help | `./target/release/rustchan-cli --help` |

Generated configuration leaves native HTTPS disabled. For local testing, enable `[tls].enabled = true` to use the self-signed development certificate on `https://localhost:8443`; browsers will warn because that certificate is not publicly trusted.

## Tor and ChanNet

RustChan includes onion-service support through Arti, so a separate `tor` daemon is not required. The generated configuration enables Tor support by default. The persistent onion identity lives under `rustchan-data/runtime/tor/state/`; back that directory up if retaining the onion address matters.

For a Tor-only deployment:

```toml
enable_tor_support = true
tor_only = true
```

<details>
<summary><strong>Optional ChanNet / RustWave listener</strong></summary>

ChanNet is off by default and listens on `127.0.0.1:7070` unless reconfigured. Its text-oriented interface supports ZIP snapshot import/export, remote refresh, content polling, and a typed JSON gateway for RustWave.

New gateways should attach a stable, globally unique UUID `message_id` to each `reply_push`. RustChan uses it as the durable replay key. Older gateways may omit it, but their content-and-timestamp fingerprint cannot distinguish identical replies created within the same timestamp second.

If federation is unused, leave the listener disabled. Otherwise, keep it on a trusted boundary or firewall it explicitly.

</details>

## Media support

RustChan runs without `ffmpeg`, but enhanced processing is capability-driven:

| Capability | Basic install | With compatible `ffmpeg` / `ffprobe` |
|---|---:|---:|
| Validated image, video, and audio storage | Yes | Yes |
| Opt-in PDF storage and preview | Yes | Yes |
| Image thumbnails | Yes | Enhanced WebP path when available |
| Video thumbnails | Limited | Yes |
| Audio waveform thumbnails | No | Yes |
| MP4-to-WebM transcoding | No | Yes, when VP9 and Opus encoders are present |

RustChan checks the actual encoder set. Missing optional codecs produce warnings and degrade gracefully unless `require_ffmpeg = true`.

## Administration

The browser admin panel covers:

- boards, access rules, per-board media limits, archive behavior, and ordering;
- reports, appeals, bans, moderation history, and IP-hash history;
- themes, custom theme values, global and board favicons, rotating board banners, and the home-page banner;
- site health, background media jobs, storage statistics, and maintenance actions; and
- full-site and board backups, restore workflows, retention, and progress tracking.

Bootstrap and shell-friendly commands include:

```text
rustchan-cli admin create-admin
rustchan-cli admin reset-password
rustchan-cli admin list-admins
rustchan-cli admin create-board
rustchan-cli admin delete-board
rustchan-cli admin list-boards
rustchan-cli admin ban
rustchan-cli admin unban
rustchan-cli admin list-bans
rustchan-cli admin db-status
```

Run `rustchan-cli admin --help` for arguments and flags.

### Backup and restore

- Full-site backups cover the database and runtime content; board backups support narrower transfers.
- Restores can use uploaded archives or backups already saved on disk.
- Automatic full backups support cadence, retention, directory or split-ZIP storage, and optional Tor identity inclusion.
- Integrity checks, repair helpers, filesystem journaling, and restore preflights are designed to avoid partial or unsafe mutations.

Backups can contain private content and operational secrets. Store them with the same care as the live data directory, and test restores before relying on them.

<details>
<summary><strong>Observability endpoints</strong></summary>

`/healthz` is intentionally public and minimal. `/readyz` returns only readiness status by default, and `/metrics` returns `404` unless explicitly enabled.

```toml
public_readiness_details = true
public_metrics_enabled = true
```

Detailed output can expose database schema health, backup freshness, media backlog, maintenance state, and Tor readiness. Restrict it at a trusted reverse proxy or network boundary; do not expose it directly to public internet or onion visitors.

</details>

## Security and privacy

RustChan minimizes stored network identifiers and hardens common application boundaries, but software cannot make an operator, host, proxy, or deployment trustworthy by itself.

| Boundary | Current behavior |
|---|---|
| Admin credentials | Argon2id password hashes |
| Sessions | `HttpOnly` and `SameSite=Strict` cookies with secure transport handling |
| State-changing forms | CSRF validation and constant-time token comparison |
| Client addresses | Keyed hashes; raw IPs are not stored or logged by RustChan |
| Uploads and restores | Type, size, path, archive, and expansion checks before mutation |
| Public diagnostics | Minimal defaults; detailed readiness and metrics are opt-in |

Operators remain responsible for secrets, moderation, backups, network boundaries, reverse-proxy correctness, applicable law, and independently hosted content. RustChan does not promise anonymity, perfect security, availability, or protection from a malicious operator. See [SECURITY.md](SECURITY.md) for reporting scope and current contact limitations.

## Development

RustChan uses Axum, Tokio, `rusqlite` with bundled SQLite, server-rendered Rust templates, `rustls`, Arti, and an in-process worker queue.

Run the Rust checks used by the repository:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
```

Browser coverage uses Playwright:

```bash
npm ci
npx playwright install
npm run test:e2e:ci
```

Some focused media tests require `ffmpeg`, `ffprobe`, and specific codecs. See [CONTRIBUTING.md](CONTRIBUTING.md) for branch, test, fixture, security, and pull-request expectations.

### Project structure

| Path | Responsibility |
|---|---|
| `src/server/` | CLI, listeners, routing, startup, and terminal console |
| `src/handlers/` | Public, posting, thread, setup, and administration request handling |
| `src/db/` | SQLite pool, schema, migrations, and persistence operations |
| `src/media/` | Inspection, thumbnails, conversion, reconciliation, and pruning |
| `src/templates/` | Server-rendered public and admin HTML |
| `src/chan_net/` | ChanNet snapshots, import/export, polling, and command gateway |
| `src/middleware/` | CSRF, rate limiting, transport, IP hashing, and shared state |
| `src/tls/` | Self-signed, manual, and ACME-related TLS support |
| `tests/e2e/` | Cross-browser public, admin, media, backup, security, and accessibility tests |
| `docs/` | Screenshots, branding assets, and project notes |

## Project identity

<p align="center">
  <img width="260" alt="Rust Chan, the blue-eyed anthropomorphized form of the RustChan website and app" src="docs/assets/branding/rust-chan-mascot.png">
</p>

The RustChan mark combines an imageboard thread card with a terminal prompt: conversation on the outside, a compact operator-friendly core inside. **Rust Chan** is RustChan itself anthropomorphized—the website and app imagined as a confident hacker/sysadmin who keeps its boards, threads, media, and server running. She carries the same rust-orange, charcoal, cream, and terminal-blue palette as the broader identity.

Logo variants, mascot guidance, colors, and asset filenames are documented in [docs/BRANDING.md](docs/BRANDING.md).

## Contributing and license

Focused, testable contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md), the [Code of Conduct](CODE_OF_CONDUCT.md), and the [support boundaries](SUPPORT.md) before opening project material. Security issues should follow [SECURITY.md](SECURITY.md).

RustChan is available under the [MIT License](LICENSE).

## More reading

- [SETUP.md](SETUP.md) — installation, deployment, media tools, Tor, TLS, and troubleshooting
- [CHANGELOG.md](CHANGELOG.md) — release history
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution and validation workflow
- [SECURITY.md](SECURITY.md) — vulnerability-reporting scope and safe handling
- [SUPPORT.md](SUPPORT.md) — operator and project support boundaries
- [docs/BRANDING.md](docs/BRANDING.md) — logo, mascot, palette, and usage
