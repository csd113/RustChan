<div align="center">

```
██████╗ ██╗   ██╗███████╗████████╗ ██████╗██╗  ██╗ █████╗ ███╗   ██╗
██╔══██╗██║   ██║██╔════╝╚══██╔══╝██╔════╝██║  ██║██╔══██╗████╗  ██║
██████╔╝██║   ██║███████╗   ██║   ██║     ███████║███████║██╔██╗ ██║
██╔══██╗██║   ██║╚════██║   ██║   ██║     ██╔══██║██╔══██║██║╚██╗██║
██║  ██║╚██████╔╝███████║   ██║   ╚██████╗██║  ██║██║  ██║██║ ╚████║
╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝    ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝
```

**A self-contained imageboard server — one binary, zero dependencies.**

[![Rust](https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![SQLite](https://img.shields.io/badge/database-SQLite-blue?style=flat-square&logo=sqlite)](https://www.sqlite.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green?style=flat-square)](LICENSE)

</div>

---

## What is RustChan?

RustChan is a fully-featured imageboard (think 4chan-style) compiled into a single Rust binary. Drop it on a Raspberry Pi, a VPS, or your local machine and it runs immediately — no containers, no package managers, no runtime dependencies.

**Features at a glance:**
- 📋 Multiple boards with SFW / NSFW tagging
- 🖼️ Image & video uploads with auto-generated thumbnails (jpg, png, gif, webp, mp4, webm)
- 🔐 Admin panel with ban management, word filters, thread moderation
- 🔍 Per-board full-text search
- 📖 Catalog view for every board
- 🚫 IP-based banning with optional expiry
- 🛡️ CSRF protection, rate limiting, tripcodes
- ⚙️ Simple `settings.toml` for persistent configuration
- 💾 All data in `./chan-data/` next to the binary — trivially portable

---

## Quick Start

```bash
# 1. Build
cargo build --release

# 2. Copy the binary anywhere you want
cp target/release/chan ~/chan

# 3. Create your first admin and boards
./chan admin create-admin admin "YourPassword123!"
./chan admin create-board b    "Random"      "Anything goes"
./chan admin create-board tech "Technology"  "Programming and hardware"

# 4. Start the server
./chan
```

Open **http://localhost:8080** in your browser.

On first launch, `settings.toml` is generated next to the binary with all configurable options.

---

## File Layout

```
chan                        ← the binary (self-contained)
chan-data/
  settings.toml             ← auto-generated config (edit me!)
  chan.db                   ← SQLite database
  uploads/                  ← original uploaded files
  uploads/thumbs/           ← auto-generated thumbnails
```

---

## settings.toml

Generated automatically on first run. Edit and restart to apply changes.

```toml
# RustChan — Instance Settings

# Name shown in the browser tab, page header, and home page title.
forum_name = "RustChan"

# Port the server listens on (binds to 0.0.0.0:<port>).
port = 8080

# Maximum size for image uploads in megabytes (jpg, png, gif, webp).
max_image_size_mb = 8

# Maximum size for video uploads in megabytes (mp4, webm).
max_video_size_mb = 50
```

### Advanced: Environment Variable Overrides

All settings can be overridden with environment variables (useful for Docker / systemd):

| Variable              | Default                         | Description                                 |
|-----------------------|---------------------------------|---------------------------------------------|
| `CHAN_FORUM_NAME`     | `"RustChan"`                    | Site name (overrides settings.toml)         |
| `CHAN_PORT`           | `8080`                          | Listen port (overrides settings.toml)       |
| `CHAN_MAX_IMAGE_MB`   | `8`                             | Max image upload (MiB)                      |
| `CHAN_MAX_VIDEO_MB`   | `50`                            | Max video upload (MiB)                      |
| `CHAN_BIND`           | `0.0.0.0:<port>`                | Full bind address (overrides host + port)   |
| `CHAN_DB`             | `<exe-dir>/chan-data/chan.db`    | SQLite database path                        |
| `CHAN_UPLOADS`        | `<exe-dir>/chan-data/uploads`   | Upload directory                            |
| `CHAN_COOKIE_SECRET`  | *(weak default)*                | **Set this in production!**                 |
| `CHAN_THUMB_SIZE`     | `250`                           | Thumbnail max dimension (px)                |
| `CHAN_BUMP_LIMIT`     | `500`                           | Replies before thread stops bumping         |
| `CHAN_MAX_THREADS`    | `150`                           | Threads per board before oldest is pruned   |
| `CHAN_RATE_POSTS`     | `10`                            | Max POSTs per rate-limit window             |
| `CHAN_RATE_WINDOW`    | `60`                            | Rate limit window in seconds                |
| `CHAN_SESSION_SECS`   | `28800` (8 h)                   | Admin session duration                      |
| `CHAN_BEHIND_PROXY`   | `false`                         | Set `true` behind nginx / Caddy             |
| `RUST_LOG`           | `chan=info`                     | Log verbosity                               |

---

## Admin Commands

All admin management is done through the CLI — no separate tool needed.

```bash
# ── User management ─────────────────────────────────────────────────────────
chan admin create-admin   <username> <password>
chan admin reset-password <username> <new-password>
chan admin list-admins

# ── Board management ────────────────────────────────────────────────────────
chan admin create-board <short> <name> [description] [--nsfw]
chan admin delete-board <short>
chan admin list-boards

# ── Ban management ──────────────────────────────────────────────────────────
chan admin ban      <ip_hash> <reason> [duration_hours]   # omit hours = permanent
chan admin unban    <ban_id>
chan admin list-bans
```

Example full setup:

```bash
./chan admin create-admin  admin   "Secur3P@ss"
./chan admin create-board  b       "Random"      "General discussion"
./chan admin create-board  tech    "Technology"  "Programming, hardware"
./chan admin create-board  meta    "Meta"        "About this board"
./chan admin create-board  nsfw    "NSFW"        "Adults only" --nsfw
```

---

## Cross-Compilation

### Raspberry Pi 4 (ARM64) from macOS / Linux

```bash
# Install the ARM64 target
rustup target add aarch64-unknown-linux-gnu

# Recommended: use the 'cross' tool (Docker-based, handles linker automatically)
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu

# Binary lives at:
target/aarch64-unknown-linux-gnu/release/chan

# Copy to your Pi
scp target/aarch64-unknown-linux-gnu/release/chan pi@raspberrypi.local:~/chan
```

### Apple Silicon (M1/M2/M3)

```bash
cargo build --release
# Binary: target/release/chan
```

---

## Running as a systemd Service

```ini
# /etc/systemd/system/rustchan.service
[Unit]
Description=RustChan Imageboard
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi
ExecStart=/home/pi/chan
Restart=on-failure
Environment=CHAN_COOKIE_SECRET=<output of: openssl rand -hex 32>

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rustchan
sudo journalctl -u rustchan -f
```

---

## nginx Reverse Proxy

```nginx
server {
    listen 80;
    server_name your-domain.com;

    client_max_body_size 55M;

    location / {
        proxy_pass         http://127.0.0.1:8080;
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
    }
}
```

Set `CHAN_BEHIND_PROXY=true` (or `behind_proxy` in settings) so RustChan reads the real client IP from the `X-Forwarded-For` header.

---

## Terminal Monitoring

Every 60 seconds the server prints a stats line to stdout:

```
── STATS  uptime 2h05m  │  requests 1,234  │  boards 3  threads 89  posts 412  │  db 2048 KiB  uploads 15.3 MiB ──
```

---

## Security Checklist

- [ ] Set `CHAN_COOKIE_SECRET` to a random 64-char hex string (`openssl rand -hex 32`)
- [ ] Change the default admin password immediately after first login
- [ ] Run as an unprivileged user — never as root
- [ ] For internet exposure: put behind nginx + Let's Encrypt, enable `CHAN_BEHIND_PROXY=true`
- [ ] Firewall port 8080 externally if using a reverse proxy

---

## Architecture

RustChan is intentionally minimal — everything compiles into one self-contained binary.

| Component       | Technology                                     |
|-----------------|------------------------------------------------|
| Web framework   | [Axum](https://github.com/tokio-rs/axum) 0.7  |
| Async runtime   | [Tokio](https://tokio.rs/)                     |
| Database        | SQLite (bundled via rusqlite)                  |
| Image processing| [image](https://github.com/image-rs/image) crate |
| Video thumbnails| ffmpeg (if available) or placeholder           |
| HTML templating | Plain Rust strings — no template engine        |
| Auth            | Argon2 password hashing, session cookies       |
| Config          | `settings.toml` + env var overrides            |

There is no template engine, no ORM, and no JavaScript framework. The CSS is embedded directly in the binary. The result is a single binary of roughly 12–18 MiB that starts in milliseconds.

---

<div align="center">
Built with Rust 🦀
</div>
