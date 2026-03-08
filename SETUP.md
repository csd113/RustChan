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

Set `require_ffmpeg = true` in `settings.toml` if you want startup to fail when ffmpeg is missing.

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

Save this value — it's used for CSRF tokens and IP hashing. **Do not change it after your instance has posts.**

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
forum_name = "My Chan"
port = 8080
max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150
enable_tor_support = false
require_ffmpeg = false
wal_checkpoint_interval_secs = 3600
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

This tells RustChan to trust `X-Forwarded-For` and automatically sets `Secure` on all cookies. Restart to apply.

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
| `RUST_LOG` | `rustchan-cli=info` | Log verbosity |

---

## Admin Panel

Log in at `/admin`. All moderation is available from the web panel.

### Key Features

- **Board settings** — click any board to edit its name, limits, and feature toggles (video embeds, PoW CAPTCHA, editing, archiving) without restarting
- **Ban + Delete** — every post shows a ⛔ button in admin view; one click to ban the IP hash and delete the post
- **Ban appeals** — banned users can submit appeals; review them under the **ban appeals** section with dismiss or accept+unban
- **IP history** — click 🔍 on any post to see all posts from that IP hash across all boards
- **Reports** — user-submitted reports appear in the report inbox with resolve and resolve+ban actions
- **Moderation log** — append-only audit trail of all admin actions
- **Word filters** — plain-text substring match with optional replacement
- **Database maintenance** — one-click VACUUM with before/after size display
- **Backups** — full and per-board backup/restore directly from the panel

---

## Backups

### From the Admin Panel (Recommended)

All backup operations are available in the admin panel — no shell access needed.

**Full backups** include a consistent database snapshot and all uploaded files.
**Board backups** are self-contained (manifest + uploads) and can move a single board between instances.

Restore behaviour:
- Existing board → content wiped and replaced
- Missing board → created from scratch
- Row IDs are remapped to prevent collisions

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

---

## Security Checklist

Before exposing your instance to the internet:

- [ ] `CHAN_COOKIE_SECRET` set to a unique value (`openssl rand -hex 32`)
- [ ] Default admin password changed
- [ ] Running as `chan` user, not root
- [ ] Port 8080 firewalled from external access
- [ ] nginx configured with HTTPS
- [ ] `CHAN_BEHIND_PROXY=true` set
- [ ] `client_max_body_size` in nginx matches your video size limit
- [ ] Rate limits tuned for your audience
- [ ] Automated backups scheduled and restore tested
- [ ] systemd hardening directives active
- [ ] Tor hidden service directory owned by `tor` user with mode `700` (if applicable)
- [ ] Log monitoring in place

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

Database migrations are automatic — no manual SQL needed when upgrading.

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
If installed to a custom path, add it to PATH in the systemd override.

**Tor address not showing:**
1. Verify Tor is running: `sudo systemctl status tor`
2. Check hostname file exists: `sudo cat /var/lib/tor/rustchan/hostname`
3. Verify `enable_tor_support = true` in `settings.toml`
4. Restart RustChan

**Uploads failing:**
```bash
ls -la /var/lib/chan/rustchan-data/boards/   # check ownership
sudo nginx -T | grep client_max_body_size    # check nginx limit
```

**Admin login fails:**
```bash
sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin reset-password admin "NewPassword!"
```

**Database integrity:**
```bash
sqlite3 /var/lib/chan/rustchan-data/chan.db "PRAGMA integrity_check;"
# Expected: ok
```

**Memory usage:** Typical idle footprint is 30–60 MiB. Connection pool under load uses ~32 MiB. Image processing may spike to ~64 MiB temporarily. Well within Raspberry Pi 4 limits.