# RustChan — Production Setup Guide

This guide covers a full production deployment on a Linux server or Raspberry Pi. Follow the sections in order for a clean, secure install.

---

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Building the Binary](#building-the-binary)
3. [System User and Directory Layout](#system-user-and-directory-layout)
4. [Installing the Service](#installing-the-service)
5. [First-Run Configuration](#first-run-configuration)
6. [nginx Reverse Proxy and TLS](#nginx-reverse-proxy-and-tls)
7. [Configuration Reference](#configuration-reference)
8. [Admin Panel Usage](#admin-panel-usage)
9. [Backup and Recovery](#backup-and-recovery)
10. [Raspberry Pi — SD Card Wear Reduction](#raspberry-pi--sd-card-wear-reduction)
11. [Security Hardening Checklist](#security-hardening-checklist)
12. [Troubleshooting](#troubleshooting)

---

## Prerequisites

### Supported Platforms

RustChan compiles and runs on any platform with a Rust toolchain. Tested targets:

- Linux x86-64 (Debian, Ubuntu, Fedora)
- Linux ARM64 (Raspberry Pi OS 64-bit, Ubuntu 22.04+ for Pi)
- macOS (Apple Silicon and Intel, for local development)

### Install Rust

Always install Rust via `rustup` — do not use your distribution's package manager version.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version   # should be 1.75 or newer
```

### System Packages

```bash
# Debian / Ubuntu / Raspberry Pi OS
sudo apt update && sudo apt install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    sqlite3 \
    nginx \
    certbot \
    python3-certbot-nginx

# Fedora / RHEL
sudo dnf install -y gcc openssl-devel sqlite nginx certbot python3-certbot-nginx
```

---

## Building the Binary

### Build on the target machine

```bash
git clone https://your-repo/rustchan.git
cd rustchan
cargo build --release
```

The binary is at `target/release/chan`. It is fully self-contained — copy it anywhere.

### Cross-compile from x86-64 to ARM64 (Raspberry Pi)

```bash
# Install the target
rustup target add aarch64-unknown-linux-gnu

# Option A: use 'cross' (Docker-based, easiest)
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu

# Option B: use the system cross-linker manually
sudo apt install gcc-aarch64-linux-gnu
# Add to ~/.cargo/config.toml:
#   [target.aarch64-unknown-linux-gnu]
#   linker = "aarch64-linux-gnu-gcc"
cargo build --release --target aarch64-unknown-linux-gnu

# Binary at:
target/aarch64-unknown-linux-gnu/release/chan
```

Transfer the binary to your server:

```bash
scp target/aarch64-unknown-linux-gnu/release/chan pi@raspberrypi.local:~/chan
```

---

## System User and Directory Layout

Run RustChan as a dedicated unprivileged user. Never run it as root.

```bash
# Create a system user with no login shell and a fixed home directory
sudo useradd --system --shell /usr/sbin/nologin --home /var/lib/chan --create-home chan

# Create required directories
sudo mkdir -p /var/lib/chan/chan-data/uploads/thumbs
sudo chown -R chan:chan /var/lib/chan

# Install the binary
sudo install -o root -g root -m 0755 target/release/chan /usr/local/bin/chan

# Verify
/usr/local/bin/chan --version
```

### Static Assets

The CSS is served from `static/style.css`, which must be readable by the process at runtime:

```bash
sudo mkdir -p /var/lib/chan/static
sudo cp static/style.css /var/lib/chan/static/
sudo chown -R chan:chan /var/lib/chan/static
```

---

## Installing the Service

### Generate a Secret Key

Before starting the service, generate a unique secret key. This value is used for CSRF tokens and IP hashing. **Do not change it after your instance has posts** — existing IP hashes (and therefore bans) will become invalid.

```bash
openssl rand -hex 32
# Example output: a3f8c1d2e4b56789...
```

### systemd Service

Copy the provided unit file and configure your secret:

```bash
sudo cp deploy/chan.service /etc/systemd/system/chan.service
```

Create a drop-in override to supply the secret without editing the base unit file:

```bash
sudo systemctl edit chan
```

Add the following (replace `<your-secret>` with the output of the `openssl` command above):

```ini
[Service]
Environment=CHAN_COOKIE_SECRET=<your-secret>
Environment=CHAN_DB=/var/lib/chan/chan-data/chan.db
Environment=CHAN_UPLOADS=/var/lib/chan/chan-data/uploads
WorkingDirectory=/var/lib/chan
```

Reload and enable:

```bash
sudo systemctl daemon-reload
sudo systemctl enable chan
sudo systemctl start chan
sudo systemctl status chan
```

Follow logs:

```bash
sudo journalctl -u chan -f
```

### Provided systemd Unit (deploy/chan.service)

The included unit file sets:

- `User=chan` and `Group=chan` — runs as the unprivileged user
- `Restart=on-failure` — automatically restarts on crash
- `NoNewPrivileges=true` — prevents privilege escalation
- `PrivateTmp=true` — isolated /tmp namespace
- `ProtectSystem=strict` with `ReadWritePaths` for the data directory

---

## First-Run Configuration

On first start, `chan-data/settings.toml` is auto-generated with a random `cookie_secret`. Review and edit it before creating content:

```bash
sudo -u chan nano /var/lib/chan/chan-data/settings.toml
```

Key settings to review:

```toml
forum_name = "My Chan"       # Change to your site name
port = 8080                  # Internal port (nginx will proxy to this)
max_image_size_mb = 8        # Raise if you want larger image uploads
max_video_size_mb = 50       # Raise for longer video uploads
```

Restart after editing:

```bash
sudo systemctl restart chan
```

### Create the First Admin Account

```bash
sudo -u chan \
    CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin create-admin admin "YourSecurePassword123!"
```

Use a strong password. You can change it at any time:

```bash
sudo -u chan \
    CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin reset-password admin "NewPassword456!"
```

### Create Boards

```bash
# SFW boards
sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin create-board b    "Random"     "General discussion"

sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin create-board tech "Technology" "Programming and hardware"

sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin create-board meta "Meta"       "About this site"

# NSFW board (shown separately on the home page)
sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin create-board nsfw "NSFW"       "Adult content" --nsfw
```

Boards can also be created and managed in the web-based admin panel at `/admin/panel`.

---

## nginx Reverse Proxy and TLS

For internet-facing deployments, put RustChan behind nginx with a TLS certificate from Let's Encrypt.

### Install nginx Config

```bash
sudo cp deploy/nginx.conf /etc/nginx/sites-available/chan
sudo nano /etc/nginx/sites-available/chan   # set your domain name
sudo ln -sf /etc/nginx/sites-available/chan /etc/nginx/sites-enabled/chan
sudo nginx -t && sudo systemctl reload nginx
```

The key directives in `deploy/nginx.conf`:

```nginx
server {
    listen 80;
    server_name your-domain.com;

    # Must be at least max_video_size_mb + headroom
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

### Obtain a TLS Certificate

```bash
# Your domain must already point to this server's IP address
sudo certbot --nginx -d your-domain.com

# Certbot will modify the nginx config and set up auto-renewal.
# Verify renewal works:
sudo certbot renew --dry-run
```

### Enable Proxy Mode

Tell RustChan to trust the `X-Forwarded-For` header from nginx:

```bash
sudo systemctl edit chan
```

Add:

```ini
[Service]
Environment=CHAN_BEHIND_PROXY=true
```

With `CHAN_BEHIND_PROXY=true`, RustChan also automatically sets `Secure=true` on all cookies (since TLS is expected). You can override this independently with `CHAN_HTTPS_COOKIES`.

Restart:

```bash
sudo systemctl restart chan
```

### Firewall

If using nginx as a reverse proxy, block direct access to port 8080:

```bash
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw deny 8080/tcp
sudo ufw enable
```

---

## Configuration Reference

All settings available as environment variables. Set them in the systemd override file (`sudo systemctl edit chan`) to avoid editing the base unit file.

| Variable | Default | Description |
|---|---|---|
| `CHAN_FORUM_NAME` | `RustChan` | Site display name |
| `CHAN_BIND` | `0.0.0.0:8080` | TCP bind address |
| `CHAN_PORT` | `8080` | Port only (used if `CHAN_BIND` not set) |
| `CHAN_DB` | `<exe-dir>/chan-data/chan.db` | SQLite database path |
| `CHAN_UPLOADS` | `<exe-dir>/chan-data/uploads` | Upload storage directory |
| `CHAN_COOKIE_SECRET` | *(from settings.toml)* | Secret for CSRF tokens and IP hashing. **Required in production.** |
| `CHAN_MAX_IMAGE_MB` | `8` | Max image upload in MiB (JPEG, PNG, GIF, WebP) |
| `CHAN_MAX_VIDEO_MB` | `50` | Max video upload in MiB (MP4, WebM) |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension in pixels |
| `CHAN_BUMP_LIMIT` | `500` | Replies before a thread stops being bumped |
| `CHAN_MAX_THREADS` | `150` | Threads per board before oldest is pruned |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per rate window per IP hash |
| `CHAN_RATE_WINDOW` | `60` | Rate-limit window in seconds |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration (default: 8 hours) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For`; enables `Secure` cookies |
| `CHAN_HTTPS_COOKIES` | *(same as `CHAN_BEHIND_PROXY`)* | Force `Secure` flag on cookies independently |
| `RUST_LOG` | `chan=info` | Tracing log level. Use `chan=debug` for verbose output. |

---

## Admin Panel Usage

Log in at `/admin` with the credentials created during setup. All moderation actions are available from `/admin/panel`.

### Board Settings

Click any board name to expand its settings card. You can change the display name, description, bump limit, max threads, and NSFW status without restarting the server. Changes take effect immediately.

### Post and Thread Moderation

From any board index or thread page while logged in as admin, per-post and per-thread action buttons are shown:

- **Delete post** — removes the post and its uploaded file.
- **Sticky thread** — pins the thread to the top of the board index.
- **Lock thread** — prevents new replies.
- **Delete thread** — removes the thread and all its replies and files.

### Banning Users

Find the `ip_hash` of the post you want to ban via the admin panel or directly from the database:

```bash
sqlite3 /var/lib/chan/chan-data/chan.db \
    "SELECT id, ip_hash, created_at FROM posts ORDER BY id DESC LIMIT 20;"
```

Then apply the ban via CLI or the admin panel Ban Management section:

```bash
# Temporary ban (hours)
sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin ban <ip_hash> "Spam" 24

# Permanent ban
sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin ban <ip_hash> "Repeated violations"

# List active bans
sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin list-bans

# Remove a ban by ID
sudo -u chan CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin unban <ban_id>
```

### Word Filters

Add filters from the admin panel under **Word Filters**. Patterns are plain-text substring matches. The replacement field can be left empty to remove the matched text entirely.

---

## Backup and Recovery

### Automated Backups

The included `deploy/backup.sh` script performs a live SQLite hot backup using the `.backup` command, which is safe to run while the server is active.

```bash
# Install the script
sudo cp deploy/backup.sh /usr/local/bin/chan-backup
sudo chmod +x /usr/local/bin/chan-backup

# Test it
sudo -u chan /usr/local/bin/chan-backup

# Schedule daily at 03:00
sudo crontab -e
```

Add the following line:

```
0 3 * * * /usr/local/bin/chan-backup >> /var/log/chan-backup.log 2>&1
```

By default the script writes backups to `/var/backup/chan/` and retains the last 7 daily backups. Edit the script to change the destination or retention period.

### Manual Backup

```bash
sudo systemctl stop chan
sudo -u chan sqlite3 /var/lib/chan/chan-data/chan.db \
    ".backup /var/backup/chan/chan-manual.db"
sudo systemctl start chan

# Also back up uploads
sudo tar czf /var/backup/chan/uploads-$(date +%F).tar.gz \
    -C /var/lib/chan/chan-data uploads
```

### Restore from Backup

```bash
sudo systemctl stop chan
sudo cp /var/backup/chan/chan_YYYY-MM-DD.db \
        /var/lib/chan/chan-data/chan.db
sudo chown chan:chan /var/lib/chan/chan-data/chan.db
sudo systemctl start chan
sudo journalctl -u chan -n 20   # verify clean startup
```

---

## Raspberry Pi — SD Card Wear Reduction

SQLite writes are already minimised by RustChan's WAL + `synchronous=NORMAL` settings. For long-running deployments, follow these additional steps.

### Move the Database to USB Storage

SD cards have a limited write cycle count. A USB SSD or quality flash drive will outlast the SD card significantly under database write load.

```bash
# Format a USB drive (replace sda1 with your device)
sudo mkfs.ext4 /dev/sda1
sudo mkdir -p /mnt/chan-data

# Mount it
sudo mount /dev/sda1 /mnt/chan-data
sudo mkdir -p /mnt/chan-data/uploads/thumbs
sudo chown -R chan:chan /mnt/chan-data

# Copy existing data
sudo systemctl stop chan
sudo rsync -av /var/lib/chan/chan-data/ /mnt/chan-data/
sudo systemctl start chan   # verify with new paths first

# Persist the mount across reboots
echo "UUID=$(blkid -s UUID -o value /dev/sda1) /mnt/chan-data ext4 defaults,noatime 0 2" \
    | sudo tee -a /etc/fstab
```

Update the systemd override to use the new paths:

```bash
sudo systemctl edit chan
```

```ini
[Service]
Environment=CHAN_DB=/mnt/chan-data/chan.db
Environment=CHAN_UPLOADS=/mnt/chan-data/uploads
```

### Keep System Logs in RAM

```bash
sudo nano /etc/systemd/journald.conf
```

Set:

```
Storage=volatile
RuntimeMaxUse=50M
```

This keeps journal logs in RAM (`/run/log/journal`) and discards them on reboot. Application logs from `chan` are still available with `journalctl -u chan` while the system is running. Restart journald:

```bash
sudo systemctl restart systemd-journald
```

### Enable `noatime` on the SD Card Root

```bash
sudo nano /etc/fstab
# Add 'noatime' to the options for the root partition, e.g.:
# /dev/mmcblk0p2  /  ext4  defaults,noatime  0  1
```

---

## Security Hardening Checklist

Work through this list before exposing your instance to the internet.

- [ ] `CHAN_COOKIE_SECRET` is set to a unique random value (`openssl rand -hex 32`)
- [ ] The default admin password has been changed
- [ ] The service runs as the `chan` user, not root
- [ ] Port 8080 is firewalled from external access (`sudo ufw deny 8080/tcp`)
- [ ] nginx is configured with HTTPS via Let's Encrypt
- [ ] `CHAN_BEHIND_PROXY=true` is set in the systemd override
- [ ] `client_max_body_size` in nginx matches or exceeds your configured video limit
- [ ] `CHAN_RATE_POSTS` and `CHAN_RATE_WINDOW` are tuned for your expected audience
- [ ] Automated daily backups are scheduled and the restore procedure has been tested
- [ ] Database is on USB storage (for Raspberry Pi deployments)
- [ ] systemd unit hardening directives are active (`NoNewPrivileges`, `PrivateTmp`)
- [ ] Log monitoring is in place: `sudo journalctl -u chan --since "1 hour ago"`

---

## Troubleshooting

### Service fails to start

```bash
sudo journalctl -u chan -n 50 --no-pager
```

Common causes:

- `CHAN_DB` or `CHAN_UPLOADS` path does not exist or is not owned by `chan`
- `CHAN_BIND` port is already in use (`sudo ss -tlnp | grep 8080`)
- Binary is not executable or was built for the wrong architecture

### Image/video uploads fail

```bash
# Verify directory ownership and permissions
ls -la /var/lib/chan/chan-data/uploads/
# Should be: drwxr-xr-x chan chan

sudo chown -R chan:chan /var/lib/chan/chan-data/uploads/

# Check nginx client_max_body_size if uploads are rejected before reaching chan
sudo nginx -T | grep client_max_body_size
```

### Admin login fails

```bash
# Reset the password
sudo -u chan \
    CHAN_DB=/var/lib/chan/chan-data/chan.db \
    /usr/local/bin/chan admin reset-password admin "NewPassword123!"
```

### Database integrity check

```bash
sqlite3 /var/lib/chan/chan-data/chan.db "PRAGMA integrity_check;"
# Expected: "ok"

# If corruption is suspected, restore from backup:
sudo systemctl stop chan
cp /var/backup/chan/chan_LATEST.db /var/lib/chan/chan-data/chan.db
sudo chown chan:chan /var/lib/chan/chan-data/chan.db
sudo systemctl start chan
```

### High memory usage

Expected memory footprint under load:

- Database connection pool (8 connections × ~4 MiB cache): ~32 MiB
- Image processing during uploads: temporary spike up to ~64 MiB
- Steady-state idle: 30–60 MiB

Total expected footprint is well within the limits of a Raspberry Pi 4 (2 GB+). If memory usage grows unbounded, check for a large number of simultaneous upload requests and consider reducing `CHAN_RATE_POSTS`.

### Check disk usage

```bash
# Database size
du -sh /var/lib/chan/chan-data/chan.db

# Upload directory size
du -sh /var/lib/chan/chan-data/uploads/

# Available space
df -h /var/lib/chan
```

---

## Updating RustChan

```bash
# 1. Pull and build the new version
git pull
cargo build --release

# 2. Stop the service
sudo systemctl stop chan

# 3. Take a backup before upgrading
sudo -u chan sqlite3 /var/lib/chan/chan-data/chan.db \
    ".backup /var/backup/chan/pre-upgrade-$(date +%F).db"

# 4. Install the new binary
sudo install -o root -g root -m 0755 target/release/chan /usr/local/bin/chan

# 5. Restart
sudo systemctl start chan
sudo journalctl -u chan -n 30   # verify clean startup
```

If the update includes static asset changes, also copy the new CSS:

```bash
sudo cp static/style.css /var/lib/chan/static/style.css
sudo chown chan:chan /var/lib/chan/static/style.css
```
