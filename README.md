<div align="center">

<p align="center">
  <img width="1024" height="1105" alt="rustchan-mascot" src="https://github.com/user-attachments/assets/c22e3e72-c2f5-4932-8565-72839b67bce3" />
</p>

# RustChan

### A self-hosted imageboard that is easy to run, fun to manage, and built for real communities.

One binary. One data folder. Zero required runtime dependencies.  
Built with Rust, powered by SQLite, and designed for people who want their own corner of the web.

[![Version](https://img.shields.io/badge/Version-1.1.3-0ea5e9?style=for-the-badge)](CHANGELOG.md)
[![Rust](https://img.shields.io/badge/Rust-1.90%2B-orange?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![SQLite](https://img.shields.io/badge/SQLite-Bundled-003B57?style=for-the-badge&logo=sqlite&logoColor=white)](https://www.sqlite.org/)
[![Axum](https://img.shields.io/badge/Axum-0.8-16a34a?style=for-the-badge)](https://github.com/tokio-rs/axum)
[![License: MIT](https://img.shields.io/badge/License-MIT-22c55e?style=for-the-badge)](LICENSE)

[What Is RustChan?](#what-is-rustchan) ·
[Why People Like It](#why-people-like-it) ·
[New In 1.1.3](#new-in-113) ·
[Quick Start](#five-minute-quick-start) ·
[Feature Tour](#feature-tour) ·
[Setup](#setup-and-operations) ·
[ChanNet](#channet-and-rustwave-optional) ·
[Changelog](CHANGELOG.md)

</div>

<p align="center">
  <img width="100%" alt="RustChan desktop home page" src="https://github.com/user-attachments/assets/3993ae93-aa9f-4285-b623-7a8f286b60ae" />
</p>

<p align="center">
  <img width="100%" alt="RustChan desktop board view" src="https://github.com/user-attachments/assets/ba89f0e2-0cee-4aa8-a085-4507575c0247" />
</p>



## What Is RustChan?

RustChan is self-hosted imageboard software. It lets you run your own site with boards like `/b/`, `/tech/`, or `/music/`, where people can post threads, reply, upload media, vote in polls, and build a community.

RustChan keeps the moving parts small. You do not need Docker, Postgres, Redis, or a stack of extra services to get a board online:

- one Rust binary
- one SQLite database
- one `rustchan-data/` folder for the site's state
- one web admin panel for the day-to-day stuff

It is a compact, self-contained setup that is easy to host and easy to move.

## Why People Like It

| If you want... | RustChan gives you... |
|---|---|
| Something simple to host | A single binary with SQLite and bundled dependencies |
| Something easy to manage | A proper admin panel for boards, moderation, backups, themes, and maintenance |
| Something with personality | Built-in themes, custom themes, custom favicons, and board-by-board defaults |
| Something media-friendly | Images, video, audio, image+audio combo posts, embeds, thumbnails, and waveforms |
| Something resilient | Full-site backups, board backups, restore tools, scheduled saved backups, and repair tooling |
| Something private by default | Raw IPs are not stored or logged; hashed IPs are used instead |
| Something that works on small machines | Good fit for a VPS, local box, homelab, or Raspberry Pi |
| Something that still has toys | Polls, spoiler tags, dice, sage, poster IDs, hover previews, mobile reply tools, and more |

RustChan runs as a single program and can be managed from the browser.

## New In 1.1.3

Version `1.1.3` adds several quality-of-life improvements:

- **Per-board passwords**: a board can be password-protected for viewing, or left publicly readable while requiring a password for posting.
- **Automatic saved full-site backups**: the admin panel and `settings.toml` can now schedule saved backups and keep only the newest `N` copies.
- **Better backup confidence**: saved backups are verified, backup health is surfaced in the admin UI, and full backups can be used to derive single-board restores and downloads.
- **Cleaner mobile and admin layouts**: better responsive behavior, cleaner footer/theme controls, and fewer "why is this fighting my phone?" moments.
- **Stronger networking behavior**: better timeout coverage, safer proxy-aware HTTPS detection, better redirect handling, and more resilient self-signed TLS recovery.
- **Honest media status and safer posting**: pending and failed media work is surfaced clearly, and duplicate submissions on flaky connections are prevented.

The full release history lives in [CHANGELOG.md](CHANGELOG.md). This release focuses on polish, reliability, and day-to-day usability.

## Five-Minute Quick Start

If you are building from source, the binary ends up at `./target/release/rustchan-cli`.

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

On first run, RustChan creates `rustchan-data/settings.toml`, `rustchan-data/logs/`, the database, backup folders, and the rest of its runtime layout automatically.

A few helpful notes:

- HTTPS is enabled by default on `https://localhost:8443` with a self-signed development certificate. Your browser will warn about it locally, which is normal.
- If you are on Windows, the binary is `target/release/rustchan-cli.exe`.
- If you just want to run the server on another port, use `--port`, like `./target/release/rustchan-cli --port 9090`.

## Feature Tour

### Boards, posts, and community tools

- Multiple boards with per-board settings, limits, themes, and moderation controls.
- Threaded replies with globally unique post numbers.
- Catalog, archive, pagination, and full-text search.
- Polls, spoiler tags, dice rolls, sage, tripcodes, and user-editable posts.
- Draft autosave, "(You)" tracking, and cross-board quote links with hover previews.
- Optional poster IDs, greentext collapsing, video embeds, and PoW CAPTCHA on a per-board basis.
- Mobile-friendly board, thread, and reply flows with layouts that hold up well on phones.

### Media

- Images: JPEG, PNG, GIF, WebP, BMP, TIFF, and SVG.
- Video: MP4 and WebM.
- Audio: MP3, OGG, FLAC, WAV, M4A, and AAC.
- Image+audio combo posts for cover-art-style music threads.
- Streaming uploads with in-flight validation so large uploads do not get buffered into RAM.
- Client-side auto-compression for oversized media before upload.
- Automatic thumbnails, audio waveforms, and video poster frames when `ffmpeg` is available.
- If `ffmpeg` is unavailable, RustChan still runs and falls back to simpler media handling.

### Admin tools

- Create, delete, and reorder boards from the browser.
- Set board-level rules for media, editing, archiving, poster IDs, themes, cooldowns, and access protection.
- Moderate posts, review reports, process ban appeals, ban by post, and inspect IP history.
- Manage site settings, favicons, built-in themes, and custom themes from the admin panel.
- Run full-site backups and per-board backups from the admin panel.
- Restore from uploaded backup files or from backup files already on the server.
- Schedule saved full-site backups automatically and keep only the latest copies you want.
- Run integrity checks, repair tools, and database maintenance from the admin panel.

### Privacy and safety

- Argon2id password hashing for admin accounts.
- `HttpOnly` and `SameSite=Strict` sessions.
- CSRF protection with constant-time token comparison.
- Security headers, no inline JavaScript, and CSP-friendly page behavior.
- Raw IPs are never stored or logged. RustChan uses an HMAC-keyed hash instead.
- Rate limiting for reads and writes, plus replay protection for PoW nonces.
- File validation uses content type and magic bytes rather than extensions alone.
- Restore protections against zip bombs, oversized uploads, path traversal, and malformed data.

## Built-In Themes

RustChan ships with a stack of built-in looks, and admins can add custom themes too.

| Theme | Vibe |
|---|---|
| `fluorogrid` | Bright retro-futurist grid with loud accent colors. This is the current default. |
| `terminal` | Green CRT glow for the "I want my forum to boot up like a mainframe" crowd. |
| `aero` | Glossy blue Frutiger Aero nostalgia. |
| `dorfic` | Warm amber sci-fi terminal energy. |
| `forest` | Earthy woodland palette with calmer contrast. |
| `chanclassic` | Beige, maroon, and classic imageboard DNA. |
| `neoncubicle` | Soft office-futurist magenta and gray. |

Theme selection is user-facing, site defaults are admin-controlled, and boards can have their own defaults too.

## Setup And Operations

RustChan is straightforward to start and includes the tools needed for longer-term operation.

### Helpful settings

`settings.toml` is generated automatically at `rustchan-data/settings.toml`. A small sample:

```toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
default_theme = "fluorogrid"

port = 8080
enable_tor_support = true
require_ffmpeg = false

auto_full_backup_interval_hours = 24
auto_full_backup_copies_to_keep = 3

[tls]
enabled = true
port = 8443
```

Some especially useful settings:

- `default_theme`: the default look for new visitors.
- `enable_tor_support`: built-in onion service support via Arti.
- `require_ffmpeg`: refuse startup if `ffmpeg` is missing.
- `auto_full_backup_interval_hours`: how often RustChan creates saved full-site backups automatically.
- `auto_full_backup_copies_to_keep`: how many saved full backups stay on disk after rotation.

`cookie_secret` is generated for you on first run. Do not casually change it later unless you intentionally want to invalidate sessions, CSRF tokens, and IP hashes.

### Optional extras

- **ffmpeg**: strongly recommended if you want video thumbnails, WebM transcodes, and audio waveforms. See [SETUP.md#install-ffmpeg](SETUP.md#install-ffmpeg).
- **Tor onion service**: built in via Arti. No separate `tor` service required. See [SETUP.md#tor-onion-service](SETUP.md#tor-onion-service).
- **HTTPS / TLS**: enabled locally by default with a self-signed dev cert. For production, use a manual cert or build with `--features tls-acme` for Let's Encrypt support. See [SETUP.md#https-and-tls](SETUP.md#https-and-tls).
- **Linux service deployment**: there is a full service and reverse-proxy walkthrough in [SETUP.md](SETUP.md).

### Common commands

```bash
# Start the server
./target/release/rustchan-cli

# Start the server on a different port
./target/release/rustchan-cli --port 9090

# Create and manage admins
./target/release/rustchan-cli admin create-admin admin "StrongPassword"
./target/release/rustchan-cli admin reset-password admin "NewStrongPassword"
./target/release/rustchan-cli admin list-admins

# Create and inspect boards
./target/release/rustchan-cli admin create-board b "Random" "General discussion"
./target/release/rustchan-cli admin create-board tech "Technology" "Programming and hardware"
./target/release/rustchan-cli admin list-boards

# Ban management
./target/release/rustchan-cli admin list-bans
```

### Where the data lives

By default, RustChan keeps its runtime state in `rustchan-data/` next to the binary:

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

The data layout is compact and easy to back up. Copy the folder, and you have most of what matters.

## ChanNet And RustWave (Optional)

Most installs will not need this section.

RustChan can also expose an optional second listener for text-only federation and RustWave integration. It is **not enabled by default**. Start the server with:

```bash
./target/release/rustchan-cli --chan-net
```

By default, that listener binds to `127.0.0.1:7070`.

What it does:

- `/chan/export`: export posts as a ZIP snapshot
- `/chan/import`: import a ZIP snapshot
- `/chan/refresh`: pull from a remote peer
- `/chan/poll`: fetch only new content since a timestamp
- `/chan/command`: typed JSON command gateway for RustWave

Important details:

- ChanNet is text-only by design. No images or other media cross this interface.
- Payloads are ZIP archives containing structured text.
- If you are running a public instance and you do not need federation, keep the listener off or firewall it appropriately.

## Under The Hood

For the technically curious, RustChan currently looks like this:

| Layer | What RustChan uses |
|---|---|
| Web framework | Axum 0.8 |
| Runtime | Tokio |
| Database | SQLite via `rusqlite` |
| Rendering | Rust templates in `src/templates/` |
| Media | `image`, EXIF handling, optional `ffmpeg` and `ffprobe` |
| TLS | `rustls`, self-signed dev certs, optional ACME or manual certs |
| Tor | Arti |
| Logging | `tracing` with daily file rotation |
| Background work | In-process worker queue |

The architecture stays compact and self-contained.

## Deep Dives

- [SETUP.md](SETUP.md): installation, ffmpeg, Tor, TLS, Linux service setup, reverse proxy notes, and troubleshooting
- [CHANGELOG.md](CHANGELOG.md): full release history
- [LICENSE](LICENSE): MIT

---

<div align="center">

**RustChan is for people who want to run their own little corner of the internet with a manageable amount of overhead.**

</div>
