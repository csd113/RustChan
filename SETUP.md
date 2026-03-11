# RustChan — Setup Guide

Complete setup instructions for Linux, macOS, and Windows.

---

## Contents

1. [Prerequisites](#prerequisites)
2. [Installing ffmpeg (Optional)](#installing-ffmpeg)
3. [Installing Tor (Optional)](#installing-tor)
4. [Building](#building)
5. [System Setup (Linux)](#system-setup-linux)
6. [Running as a Service](#running-as-a-service)
7. [First-Run Configuration](#first-run-configuration)
8. [nginx + TLS](#nginx--tls)
9. [Tor Hidden Service](#tor-hidden-service)
10. [Configuration Reference](#configuration-reference)
11. [Admin Panel](#admin-panel)
12. [Backups](#backups)
13. [Raspberry Pi Tips](#raspberry-pi-tips)
14. [Security Checklist](#security-checklist)
15. [Updating](#updating)
16. [Troubleshooting](#troubleshooting)

---

## Prerequisites

### Supported Platforms

- Linux x86-64 (Debian, Ubuntu, Fedora)
- Linux ARM64 (Raspberry Pi OS 64-bit)
- macOS 13+ (Apple Silicon and Intel)
- Windows 10/11 x86-64

### Install Rust

```bash
# Linux / macOS
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version   # 1.75+

# Windows — download rustup-init.exe from https://rustup.rs
```

### System Packages (Linux)

```bash
# Debian / Ubuntu / Raspberry Pi OS
sudo apt update && sudo apt install -y \
    build-essential pkg-config libssl-dev sqlite3 nginx certbot python3-certbot-nginx

# Fedora / RHEL
sudo dnf install -y gcc openssl-devel sqlite nginx certbot python3-certbot-nginx
```

---

## Installing ffmpeg

**Optional.** Enables MP4→WebM transcoding, AV1→VP9 re-encoding, audio waveform thumbnails, and video thumbnail extraction. Without it, videos are served as-is and audio posts show a generic icon.

**Linux:**
```bash
# Debian / Ubuntu
sudo apt install -y ffmpeg

# Fedora (enable RPM Fusion first)
sudo dnf install -y ffmpeg

# Alpine
apk add ffmpeg
```

**macOS:**
```bash
brew install ffmpeg
```

**Windows:**
```powershell
winget install --id Gyan.FFmpeg -e
# Then add C:\Program Files\FFmpeg\bin to your PATH
```

Verify: `ffmpeg -version`

Set `require_ffmpeg = true` in `settings.toml` if you want startup to fail when ffmpeg is missing. The maximum time any single ffmpeg job may run is controlled by `ffmpeg_timeout_secs` (default: 120); increase it for large video files on slow hardware.

---

## Installing Tor

**Optional.** When enabled, RustChan reads your `.onion` address and displays it on the home page and admin panel. Tor handles all routing — RustChan just reads the hostname file.

**Linux:**
```bash
# Debian / Ubuntu
sudo apt install -y tor
sudo systemctl enable --now tor
```

**macOS:**
```bash
brew install tor
brew services start tor
```

**Windows:**
1. Download the [Tor Expert Bundle](https://www.torproject.org/download/tor/)
2. Extract to `C:\tor` and add it to PATH

Verify: `tor --version`

---

## Building

```bash
git clone https://github.com/csd113/RustChan.git
cd RustChan
cargo build --release
```

Binary output: `target/release/rustchan-cli` (or `.exe` on Windows). Fully self-contained — copy it anywhere.

### Cross-Compile for ARM64 (Raspberry Pi)

```bash
rustup target add aarch64-unknown-linux-gnu
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu
```

---

## System Setup (Linux)

Run as a dedicated unprivileged user — never as root.

```bash
# Create system user
sudo useradd --system --shell /usr/sbin/nologin --home /var/lib/chan --create-home chan

# Create directories
sudo mkdir -p /var/lib/chan/rustchan-data/boards/thumbs
sudo mkdir -p /var/lib/chan/static
sudo chown -R chan:chan /var/lib/chan

# Install binary and static assets
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli
sudo cp static/style.css /var/lib/chan/static/
sudo chown -R chan:chan /var/lib/chan/static
```

---

## Running as a Service

### Generate a Secret Key

```bash
openssl rand -hex 32
```

Save this value — it's used for CSRF tokens and IP hashing. **Do not change it after your instance has posts.** Changing it invalidates all existing bans, IP history, and session cookies.

### systemd (Linux)

```bash
sudo cp deploy/rustchan-cli.service /etc/systemd/system/rustchan-cli.service
sudo systemctl edit rustchan-cli
```

Add your configuration:

```ini
[Service]
Environment=CHAN_COOKIE_SECRET=<your-secret>
Environment=CHAN_DB=/var/lib/chan/rustchan-data/chan.db
Environment=CHAN_UPLOADS=/var/lib/chan/rustchan-data/boards
WorkingDirectory=/var/lib/chan
```

Start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rustchan-cli
sudo journalctl -u rustchan-cli -f
```

The included unit file runs as the `chan` user with `NoNewPrivileges`, `PrivateTmp`, and `ProtectSystem=strict`.

### Windows

Use [NSSM](https://nssm.cc):

```powershell
nssm install RustChan "C:\path\to\rustchan-cli.exe"
nssm set RustChan AppDirectory "C:\rustchan"
nssm set RustChan AppEnvironmentExtra "CHAN_COOKIE_SECRET=<your-secret>"
nssm start RustChan
```

---

## First-Run Configuration

On first start, `rustchan-data/settings.toml` is auto-generated. Review it:

```bash
sudo -u chan nano /var/lib/chan/rustchan-data/settings.toml
```

Key settings:

```toml
# Site identity
forum_name = "My Chan"
site_subtitle = "A self-hosted imageboard"

# Default theme for new visitors.
# Options: terminal, frutiger-aero, dorific-aero, fluorogrid, neoncubicle, chan-classic
default_theme = "terminal"

# Network
port = 8080

# Upload size limits (MiB)
max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150

# Optional integrations
enable_tor_support = false
require_ffmpeg = false
ffmpeg_timeout_secs = 120

# Database maintenance
wal_checkpoint_interval_secs = 3600
auto_vacuum_interval_hours = 24
poll_cleanup_interval_hours = 72
db_warn_threshold_mb = 2048

# Background worker tuning
job_queue_capacity = 1000
waveform_cache_max_mb = 200
archive_before_prune = true

# Uncomment to tune the blocking thread pool (default: logical_cpus × 4)
# blocking_threads = 8
```

Restart after editing: `sudo systemctl restart rustchan-cli`

### Create Admin and Boards

```bash
# Create admin
sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-admin admin "YourSecurePassword!"

# Create boards
sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-board b    "Random"     "General discussion"

sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-board tech "Technology" "Programming and hardware"
```

Boards can also be created from the admin panel at `/admin`.

---

## nginx + TLS

### Configure nginx

```bash
sudo cp deploy/nginx.conf /etc/nginx/sites-available/rustchan-cli
sudo nano /etc/nginx/sites-available/rustchan-cli   # set your domain
sudo ln -sf /etc/nginx/sites-available/rustchan-cli /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx
```

Key directives:

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

### TLS via Let's Encrypt

```bash
sudo certbot --nginx -d your-domain.com
sudo certbot renew --dry-run
```

### Enable Proxy Mode

```bash
sudo systemctl edit rustchan-cli
```

```ini
[Service]
Environment=CHAN_BEHIND_PROXY=true
```

This tells RustChan to trust `X-Forwarded-For` and automatically sets `Secure` on all cookies. Without this flag, bans and rate limits will not function correctly behind nginx. Restart to apply.

### Firewall

```bash
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw deny 8080/tcp
sudo ufw enable
```

---

## Tor Hidden Service

Assumes Tor is installed and RustChan is running on port 8080.

### Linux

```bash
sudo nano /etc/tor/torrc
```

Add:

```
HiddenServiceDir /var/lib/tor/rustchan/
HiddenServicePort 80 127.0.0.1:8080
```

```bash
sudo systemctl restart tor
sudo cat /var/lib/tor/rustchan/hostname   # your .onion address
```

### macOS

Edit `/opt/homebrew/etc/tor/torrc` (Apple Silicon) or `/usr/local/etc/tor/torrc` (Intel):

```
HiddenServiceDir /usr/local/var/lib/tor/rustchan/
HiddenServicePort 80 127.0.0.1:8080
```

```bash
mkdir -p /usr/local/var/lib/tor/rustchan && chmod 700 /usr/local/var/lib/tor/rustchan
brew services restart tor
cat /usr/local/var/lib/tor/rustchan/hostname
```

### Windows

Edit `C:\tor\torrc`:

```
HiddenServiceDir C:\tor\hidden_service\rustchan
HiddenServicePort 80 127.0.0.1:8080
```

Restart Tor and read `C:\tor\hidden_service\rustchan\hostname`.

### Finish

Set `enable_tor_support = true` in `settings.toml` and restart RustChan. The `.onion` address will appear on the home page and admin panel.

If your hostname file is in a non-standard location:

```ini
Environment=CHAN_TOR_HOSTNAME_FILE=/custom/path/hostname
```

---

## Configuration Reference

All settings can be overridden via environment variables (take precedence over `settings.toml`).

| Variable | Default | Description |
|---|---|---|
| `CHAN_FORUM_NAME` | `RustChan` | Site display name |
| `CHAN_SITE_SUBTITLE` | *(from settings.toml)* | Home page subtitle |
| `CHAN_DEFAULT_THEME` | `terminal` | Default theme for new visitors (`terminal`, `frutiger-aero`, `dorific-aero`, `fluorogrid`, `neoncubicle`, `chan-classic`) |
| `CHAN_PORT` | `8080` | TCP port |
| `CHAN_BIND` | `0.0.0.0:8080` | Full bind address (overrides port) |
| `CHAN_DB` | `rustchan-data/chan.db` | Database path |
| `CHAN_UPLOADS` | `rustchan-data/boards` | Upload directory |
| `CHAN_COOKIE_SECRET` | *(from settings.toml)* | CSRF + IP hashing key |
| `CHAN_MAX_IMAGE_MB` | `8` | Max image size (MiB) |
| `CHAN_MAX_VIDEO_MB` | `50` | Max video size (MiB) |
| `CHAN_MAX_AUDIO_MB` | `150` | Max audio size (MiB) |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension (px) |
| `CHAN_BUMP_LIMIT` | `500` | Replies before thread stops bumping |
| `CHAN_MAX_THREADS` | `150` | Threads per board before pruning/archiving |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per window per IP |
| `CHAN_RATE_WINDOW` | `60` | Rate window (seconds) |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration (8 hours) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For` |
| `CHAN_HTTPS_COOKIES` | *(mirrors proxy setting)* | Force `Secure` cookies |
| `CHAN_WAL_CHECKPOINT_SECS` | `3600` | WAL checkpoint interval; `0` to disable |
| `CHAN_AUTO_VACUUM_HOURS` | `24` | Scheduled VACUUM interval (hours); `0` to disable |
| `CHAN_POLL_CLEANUP_HOURS` | `72` | Expired poll vote cleanup interval (hours) |
| `CHAN_DB_WARN_MB` | `2048` | DB file size threshold for admin panel warning (MiB) |
| `CHAN_JOB_QUEUE_CAPACITY` | `1000` | Max pending background jobs; excess dropped with a warning |
| `CHAN_FFMPEG_TIMEOUT_SECS` | `120` | Max duration for a single ffmpeg job (seconds) |
| `CHAN_WAVEFORM_CACHE_MB` | `200` | Max waveform/thumbnail cache per board's `thumbs/` directory (MiB) |
| `CHAN_BLOCKING_THREADS` | `cpus × 4` | Tokio blocking thread pool size (tune down on RAM-constrained hardware) |
| `CHAN_ARCHIVE_BEFORE_PRUNE` | `true` | Archive globally before any hard-delete, even on boards without per-board archiving |
| `CHAN_TOR_HOSTNAME_FILE` | *(auto-detected)* | Override path to Tor `hostname` file |
| `RUST_LOG` | `rustchan-cli=info` | Log verbosity |

---

## Admin Panel

Log in at `/admin`. All moderation and configuration is available from the web panel. The panel is organised in the following order:

**Site Settings** → **Boards** → **Moderation Log** → **Report Inbox** → **Moderation** (ban appeals, active bans, word filters) → **Full Site Backup & Restore** → **Board Backup & Restore** → **Database Maintenance** → **Active Onion Address**

### Key Features

- **Site Settings** — update forum name, subtitle, and default theme without restarting
- **Board settings** — click any board to edit its name, limits, and feature toggles (video embeds, PoW CAPTCHA, editing, archiving) without restarting
- **Ban + Delete** — every post shows a ⛔ button in admin view; one click to ban the IP hash and delete the post
- **Ban appeals** — banned users can submit appeals; review them under the **Moderation** section with dismiss or accept+unban
- **IP history** — click 🔍 on any post to see all posts from that IP hash across all boards
- **Reports** — user-submitted reports appear in the report inbox with resolve and resolve+ban actions
- **Moderation log** — append-only audit trail of all admin actions
- **Word filters** — plain-text substring match with optional replacement
- **Database maintenance** — one-click VACUUM with before/after size display; red warning banner when the database exceeds `db_warn_threshold_mb`
- **Full Site Backup & Restore** — streaming backup creation and restore; no RAM buffering regardless of instance size
- **Board Backup & Restore** — self-contained per-board backup and restore

---

## Backups

### From the Admin Panel (Recommended)

All backup operations are available in the admin panel — no shell access needed.

**Full Site Backups** include a consistent database snapshot and all uploaded files. All I/O streams in 64 KiB chunks so peak RAM overhead is roughly 64 KiB regardless of instance size. Backups are written as temp files with an atomic rename, so partial backups never appear in the saved list.

**Board backups** are self-contained (manifest + uploads) and can move a single board between instances.

Restore behaviour:
- Existing board → content wiped and replaced
- Missing board → created from scratch
- Row IDs are remapped to prevent collisions
- Restore uploads are capped at 512 MiB

### Automated Shell Backups

```bash
sudo cp deploy/backup.sh /usr/local/bin/rustchan-cli-backup
sudo chmod +x /usr/local/bin/rustchan-cli-backup

# Schedule daily at 03:00
(sudo crontab -l; echo "0 3 * * * /usr/local/bin/rustchan-cli-backup >> /var/log/rustchan-cli-backup.log 2>&1") | sudo crontab -
```

### Manual Backup

```bash
sudo systemctl stop rustchan-cli
sudo -u chan sqlite3 /var/lib/chan/rustchan-data/chan.db ".backup /var/backup/chan/chan-$(date +%F).db"
sudo tar czf /var/backup/chan/boards-$(date +%F).tar.gz -C /var/lib/chan/rustchan-data boards
sudo systemctl start rustchan-cli
```

---

## Raspberry Pi Tips

### Move Database to USB Storage

Storing the database on a USB drive rather than the SD card extends card life significantly and improves write throughput.

```bash
sudo mkfs.ext4 /dev/sda1
sudo mkdir -p /mnt/rustchan-data
sudo mount /dev/sda1 /mnt/rustchan-data
sudo chown -R chan:chan /mnt/rustchan-data

sudo systemctl stop rustchan-cli
sudo rsync -av /var/lib/chan/rustchan-data/ /mnt/rustchan-data/
sudo systemctl start rustchan-cli
```

Update systemd override:

```ini
[Service]
Environment=CHAN_DB=/mnt/rustchan-data/chan.db
Environment=CHAN_UPLOADS=/mnt/rustchan-data/boards
```

Add to `/etc/fstab` for persistence.

### Reduce SD Card Writes

- Set journal storage to volatile: `Storage=volatile` in `/etc/systemd/journald.conf`
- Add `noatime` to root partition mount options in `/etc/fstab`

### Tune the Blocking Thread Pool

The default blocking thread pool (`logical_cpus × 4`) can exhaust RAM on a Raspberry Pi 4 under heavy transcoding load. Set a lower ceiling:

```ini
[Service]
Environment=CHAN_BLOCKING_THREADS=8
```

Adjust based on available RAM and expected concurrent uploads. 8 is a reasonable starting point for a Pi 4 with 4 GiB RAM.

### Waveform Cache

With limited storage on SD cards, consider lowering `waveform_cache_max_mb`:

```toml
waveform_cache_max_mb = 50
```

The background eviction task will keep the `thumbs/` directories under this limit automatically.

---

## Security Checklist

Before exposing your instance to the internet:

- [ ] `CHAN_COOKIE_SECRET` set to a unique value (`openssl rand -hex 32`)
- [ ] Default admin password changed immediately after first login
- [ ] Running as `chan` user, not root
- [ ] Port 8080 firewalled from external access (`ufw deny 8080/tcp`)
- [ ] nginx configured with HTTPS (Let's Encrypt)
- [ ] `CHAN_BEHIND_PROXY=true` set — required for bans and rate limits to work with nginx
- [ ] `client_max_body_size` in nginx matches your video size limit
- [ ] Rate limits tuned for your audience
- [ ] Automated backups scheduled and restore tested
- [ ] systemd hardening directives active (`NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`)
- [ ] Tor hidden service directory owned by `tor` user with mode `700` (if applicable)
- [ ] `db_warn_threshold_mb` set to a value appropriate for your disk (default: 2048 MiB)
- [ ] `auto_vacuum_interval_hours` enabled (default: 24) to prevent unbounded DB growth
- [ ] Log monitoring in place (`journalctl -u rustchan-cli -f`)

---

## Updating

```bash
git pull
cargo build --release

sudo systemctl stop rustchan-cli

# Back up before upgrading
sudo -u chan sqlite3 /var/lib/chan/rustchan-data/chan.db \
    ".backup /var/backup/chan/pre-upgrade-$(date +%F).db"

# Install new binary and assets
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli
sudo cp static/style.css /var/lib/chan/static/style.css
sudo chown chan:chan /var/lib/chan/static/style.css

sudo systemctl start rustchan-cli
sudo journalctl -u rustchan-cli -n 20
```

Database migrations run automatically on startup — no manual SQL is ever needed when upgrading.

---

## Troubleshooting

**Service won't start:**
```bash
sudo journalctl -u rustchan-cli -n 50 --no-pager
```
Common causes: path doesn't exist or wrong ownership, port already in use, wrong architecture.

**ffmpeg not detected:**
```bash
which ffmpeg && ffmpeg -version
```
If installed to a custom path, add it to PATH in the systemd override. If large videos time out during transcoding, increase `ffmpeg_timeout_secs` in `settings.toml`.

**Tor address not showing:**
1. Verify Tor is running: `sudo systemctl status tor`
2. Check hostname file exists: `sudo cat /var/lib/tor/rustchan/hostname`
3. Verify `enable_tor_support = true` in `settings.toml`
4. Restart RustChan
5. If the hostname file is in a non-standard location, set `CHAN_TOR_HOSTNAME_FILE`

**Uploads failing:**
```bash
ls -la /var/lib/chan/rustchan-data/boards/   # check ownership
sudo nginx -T | grep client_max_body_size    # check nginx limit
```
Large uploads rejected mid-stream indicate the nginx `client_max_body_size` is lower than RustChan's configured limit.

**Admin login fails:**
```bash
sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin reset-password admin "NewPassword!"
```
If the login page shows a lockout message, wait for the progressive delay to expire (up to a few minutes) before retrying.

**Bans and rate limits not working:**
Ensure `CHAN_BEHIND_PROXY=true` is set in the systemd override. Without it, RustChan sees all requests as coming from `127.0.0.1`.

**Background jobs not processing:**
Check `RUST_LOG=rustchan-cli=debug` output for job queue warnings. If jobs are being dropped with "queue at capacity", increase `job_queue_capacity`. If ffmpeg jobs are timing out, increase `ffmpeg_timeout_secs`.

**Database integrity:**
```bash
sqlite3 /var/lib/chan/rustchan-data/chan.db "PRAGMA integrity_check;"
# Expected: ok
```

**DB growing unboundedly:**
Verify `auto_vacuum_interval_hours` is non-zero and check the admin panel Database Maintenance section. If the DB exceeds `db_warn_threshold_mb` a red banner will appear. Run a manual VACUUM from the panel after bulk deletions.

**Memory usage:** Typical idle footprint is 30–60 MiB. Connection pool under load uses ~32 MiB. Image processing may spike to ~64 MiB temporarily. Backup I/O peaks at ~64 KiB regardless of backup size. Well within Raspberry Pi 4 limits when `blocking_threads` is tuned appropriately.
