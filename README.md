<div align="center">

<p align="center">
  <img width="320" alt="rustchan mascot" src="https://github.com/user-attachments/assets/c22e3e72-c2f5-4932-8565-72839b67bce3" />
</p>

# RustChan

### A self-hosted imageboard for people who want their own corner of the web without adopting a whole infrastructure hobby.

One binary. One data folder. SQLite only. The rest is features.

RustChan is built in Rust, ships with bundled SQLite, and is designed to be understandable, movable, and fun to run.

Current development version: `1.1.6`.

[What is RustChan?](#what-is-rustchan) ·
[Why it exists](#why-it-exists) ·
[Core features](#core-features) ·
[Quick start](#quick-start) ·
[Configuration](#configuration-and-data) ·
[Admin panel](#admin-panel-and-cli) ·
[Tor and ChanNet](#tor-and-channet) ·
[Under the hood](#under-the-hood) ·
[More reading](#more-reading)

</div>

<p align="center">
  <img width="100%" alt="RustChan home page" src="https://github.com/user-attachments/assets/3993ae93-aa9f-4285-b623-7a8f286b60ae" />
</p>

<p align="center">
  <img width="100%" alt="RustChan board view" src="https://github.com/user-attachments/assets/ba89f0e2-0cee-4aa8-a085-4507575c0247" />
</p>

## What Is RustChan?

RustChan is a self-hosted imageboard server. It gives you boards, threads, replies, media uploads, moderation, backups, and the usual imageboard nonsense, all in a single Rust binary.

It stays deliberately small in the parts that matter:

- one binary
- one SQLite database
- one `rustchan-data/` directory
- one browser admin panel

No Docker. No Postgres. No Redis. No pile of sidecars hiding in the bushes.

## Why It Exists

RustChan is for people who want a forum they can actually understand and keep running without needing a distributed-systems side quest.

| If you want... | RustChan gives you... |
|---|---|
| Something simple to host | A single binary with bundled SQLite and very few moving parts |
| Something easy to manage | A browser admin panel for boards, moderation, themes, backups, and maintenance |
| Something with personality | Built-in themes, custom themes, board defaults, and banner support |
| Something media-friendly | Images, video, audio, thumbnails, waveforms, and optional embeds |
| Something resilient | Full-site backups, board backups, restore tools, and repair helpers |
| Something private by default | Hashed IPs, CSRF protection, secure sessions, and rate limiting |
| Something that can live on small hardware | A good fit for a VPS, homelab, or tiny server |

## Core Features

### Boards and posting

- Multiple boards with per-board settings and access controls
- Threads, replies, catalog view, archive view, and search
- Polls, tripcodes, sage, spoiler tags, poster IDs, edit windows, and a 60-second self-delete timer for your own posts
- Board-specific media rules, cooldowns, captcha, and archive behavior
- Mobile-friendly layouts that still make sense on a phone

### Media

- Image uploads: JPEG, PNG, GIF, WebP, HEIC, HEIF, BMP, TIFF, and SVG
- Video uploads: MP4 and WebM
- Audio uploads: MP3, OGG, FLAC, WAV, M4A, and AAC
- Automatic thumbnails and audio waveforms when `ffmpeg` is available
- Optional MP4 to WebM transcoding when the needed codecs are present
- Streaming uploads with validation so large files do not get buffered into RAM

### Admin and recovery

- Create, delete, and reorder boards from the browser
- Configure site settings, themes, favicons, board banners, and the home page banner
- Moderate posts, review reports, process appeals, and inspect IP history
- Run full-site backups and per-board backups
- Restore from uploaded backups or backups already on disk
- Run integrity checks, repair tools, VACUUM, and other maintenance jobs

### Safety and privacy

- Argon2id password hashing for admin accounts
- `HttpOnly` and `SameSite=Strict` sessions
- CSRF protection with constant-time token comparison
- Security headers and CSP-friendly page behavior
- Raw IPs are not stored or logged; RustChan uses a keyed hash instead
- Restore protections against zip bombs, path traversal, malformed archives, and oversized uploads

## Quick Start

```bash
git clone https://github.com/csd113/RustChan.git
cd RustChan
cargo build --release

./target/release/rustchan-cli admin create-admin admin "ChangeThisPasswordNow"
./target/release/rustchan-cli admin create-board b "Random" "General discussion"
./target/release/rustchan-cli admin create-board tech "Technology" "Programming and hardware"

./target/release/rustchan-cli
```

Then open:

- `http://localhost:8080`
- admin panel: `http://localhost:8080/admin`

On first run, RustChan creates `<exe-dir>/rustchan-data/settings.toml`, the database, logs, backups, and the rest of its runtime layout automatically.

Helpful notes:

- HTTPS is enabled by default on `https://localhost:8443` with a self-signed development certificate. Your browser will complain locally, which is normal.
- On Windows, the binary is `target/release/rustchan-cli.exe`.
- If you want another port, pass `--port`, for example `./target/release/rustchan-cli --port 9090`.
- If you want the optional second listener, add `--chan-net`.

## Configuration And Data

RustChan keeps its runtime state in `<exe-dir>/rustchan-data/` next to the binary. The live process reads `settings.toml` from that directory, not from the current working directory.

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

That folder is the thing to back up if you want to move the site or keep it safe.

`settings.toml` is generated automatically on first run and documents the available options inline. A few of the more important ones:

```toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
default_theme = "forest"
port = 8080
enable_tor_support = true
require_ffmpeg = false

[tls]
enabled = false
port = 8443
```

Worth knowing:

- These stay TOML-owned at runtime: ports, Tor, arbitrary-file upload gate, ffmpeg/ffprobe paths, backup cadence, and other operational toggles.
- `CHAN_*` environment variables still override the matching values from `settings.toml` at runtime.
- When testing a copied or temporary binary, edit that binary's adjacent `rustchan-data/settings.toml`; do not rely on a cwd-local decoy file.
- `enable_tor_support` is on by default in the generated config.
- `tor_only = true` makes RustChan bind to loopback only and serve through Tor.
- `require_ffmpeg = true` makes startup fail if `ffmpeg` is missing.
- `cookie_secret` is auto-generated on first run and should not be changed casually once the site is live.
- `auto_full_backup_interval_hours` and `auto_full_backup_copies_to_keep` control saved full-site backups.

If you want the full setup and deployment walkthrough, read [SETUP.md](SETUP.md). It covers Rust installation, `ffmpeg`, Linux service setup, reverse proxy notes, Tor, TLS, and troubleshooting.

## Admin Panel And CLI

RustChan is intended to be managed from the browser, but the CLI is there for bootstrap and shell-friendly admin work.

Browser admin panel features include:

- board creation and board settings
- moderation and ban management
- report review and appeals
- themes, banners, favicon, and site settings
- backup, restore, and maintenance tools

CLI admin commands include:

- `rustchan-cli admin create-admin`
- `rustchan-cli admin reset-password`
- `rustchan-cli admin list-admins`
- `rustchan-cli admin create-board`
- `rustchan-cli admin delete-board`
- `rustchan-cli admin list-boards`
- `rustchan-cli admin ban`
- `rustchan-cli admin unban`
- `rustchan-cli admin list-bans`

Run `rustchan-cli admin --help` for the full command list and flags.

## Tor And ChanNet

RustChan includes built-in Tor onion service support via Arti. You do not need to install or manage a separate `tor` daemon.

The generated config enables Tor support by default. On first start, RustChan creates the Tor runtime layout and persists the onion identity under `rustchan-data/runtime/tor/state/`. If you care about the onion address, back that directory up.

If you want Tor-only mode:

```toml
enable_tor_support = true
tor_only = true
```

RustChan also has an optional second listener for ChanNet and RustWave integration. It is off by default. When enabled, it listens on `127.0.0.1:7070` unless you change it in `settings.toml`.

The ChanNet interface is text-only by design and exposes endpoints for:

- exporting and importing ZIP snapshots
- refreshing from a remote peer
- polling for new content
- a typed JSON command gateway for RustWave

If you are not using federation, keep it off or firewall it properly.

## Under The Hood

For the technically curious, RustChan currently leans on:

| Layer | What it uses |
|---|---|
| Web framework | Axum |
| Runtime | Tokio |
| Database | SQLite via `rusqlite` |
| Rendering | Rust templates in `src/templates/` |
| Media | `image`, EXIF handling, and optional `ffmpeg` / `ffprobe` |
| TLS | `rustls`, self-signed dev certs, optional ACME or manual certs |
| Tor | Arti |
| Logging | `tracing` with daily file rotation |
| Background work | In-process worker queue |

The architecture is intentionally compact. That is the point.

## More Reading

- [SETUP.md](SETUP.md) for installation, deployment, Tor, TLS, and troubleshooting
- [CHANGELOG.md](CHANGELOG.md) for release history
- [LICENSE](LICENSE) for the MIT license

---

RustChan is for people who want to run their own little corner of the internet without turning it into a full-time maintenance ritual.
