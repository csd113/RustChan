# Chan Imageboard — Complete Setup Guide

A production-ready 4chan-style imageboard written in Rust.
Designed to run on Raspberry Pi 4 (and up) with minimal resources.

---

## Security Overview

Before any code: here is how each security feature works.

| Feature | Implementation |
|---|---|
| **CSRF protection** | Double-submit cookie pattern. Every form includes a hidden `_csrf` field. The server generates a random token, stores it in a `SameSite=Strict` cookie, and verifies the form field matches on every POST. Cross-origin requests cannot read the cookie, so they cannot forge valid submissions. |
| **Input sanitisation / XSS prevention** | Every user-supplied string is passed through `escape_html()` before rendering, replacing `<`, `>`, `&`, `"`, `'` with HTML entities. Post body markup (greentext, links, reply refs) is applied AFTER escaping, so no raw HTML from users ever reaches the browser. |
| **File type validation** | Two-layer check: (1) the MIME type from the `Content-Type` header, (2) magic byte inspection of the actual file content. We NEVER trust the extension. Only JPEG, PNG, GIF, and WebP are accepted. |
| **Per-IP rate limiting** | In-memory sliding window (DashMap) per hashed IP. Default: max 10 POSTs per 60 seconds per IP. Configurable via environment variables. |
| **Admin authentication** | Argon2id password hashing (memory-hard, GPU-resistant). Sessions stored in DB with expiry. Session cookie is `HttpOnly`, `SameSite=Strict`, path-scoped to `/admin`. |
| **IP privacy** | IP addresses are NEVER stored. A salted SHA-256 hash is stored instead. The salt is the `CHAN_COOKIE_SECRET`, so hashes can't be rainbow-tabled from a leaked DB. |
| **Secure admin password hashing** | Argon2id with `t=2, m=65536, p=2` — costs ~65 MiB RAM and ~200ms per hash on Pi 4. Brute-force resistant. |
| **Logging** | Structured tracing to stdout → journald. Logs include: new threads, new replies, post deletions, admin login attempts. IPs are not logged directly; only ip_hash is referenced. |

---

## Prerequisites

```bash
# On Raspberry Pi OS (64-bit recommended) or Debian/Ubuntu ARM
sudo apt update && sudo apt upgrade -y

# Install Rust (no package manager version — always use rustup)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Verify
rustc --version   # should be 1.75+
cargo --version
```

---

## Building

### Build on the Pi directly

```bash
git clone https://your-repo/chan.git
cd chan
cargo build --release
```

The binary will be at `target/release/chan` and `target/release/chan-admin`.
Binary size is typically 10–20 MiB after stripping (controlled by `[profile.release]`).

### Cross-compile from x86 Linux to Pi ARM64

```bash
# Install the ARM64 target
rustup target add aarch64-unknown-linux-gnu

# Install the cross-linker
sudo apt install gcc-aarch64-linux-gnu

# Configure cargo to use the linker (add to ~/.cargo/config.toml):
# [target.aarch64-unknown-linux-gnu]
# linker = "aarch64-linux-gnu-gcc"

cargo build --release --target aarch64-unknown-linux-gnu
# Binary at: target/aarch64-unknown-linux-gnu/release/chan
```

---

## Installation on Raspberry Pi

```bash
# 1. Create a dedicated unprivileged user
sudo useradd -r -s /usr/sbin/nologin -d /var/lib/chan chan

# 2. Create data directories
sudo mkdir -p /var/lib/chan/uploads/thumbs
sudo chown -R chan:chan /var/lib/chan

# 3. Install binaries
sudo cp target/release/chan        /usr/local/bin/chan
sudo cp target/release/chan-admin  /usr/local/bin/chan-admin
sudo chmod +x /usr/local/bin/chan /usr/local/bin/chan-admin
sudo chown root:root /usr/local/bin/chan /usr/local/bin/chan-admin

# 4. Install static assets
sudo mkdir -p /var/lib/chan/static
sudo cp -r static/* /var/lib/chan/static/
sudo chown -R chan:chan /var/lib/chan/static

# 5. Install systemd service
sudo cp deploy/chan.service /etc/systemd/system/
sudo systemctl daemon-reload

# 6. IMPORTANT: Set a unique cookie secret
# Generate a strong random secret:
openssl rand -hex 32
# Paste the output as CHAN_COOKIE_SECRET in the service file:
sudo systemctl edit chan
# Add:
# [Service]
# Environment=CHAN_COOKIE_SECRET=<your-random-hex-string>
```

---

## First-Time Setup

```bash
# Create the first admin user
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin create-admin admin "YourSecurePassword123!"

# Create your boards
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin create-board tech "Technology" "Programming, hardware, software"

sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin create-board b "Random" "Anything goes"

sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin create-board meta "Meta" "Discussion about this board"

# Start the service
sudo systemctl enable chan
sudo systemctl start chan

# Verify it's running
sudo systemctl status chan
sudo journalctl -u chan -f   # follow logs
```

Visit `http://<pi-ip>:8080` in your browser.
Log into the admin panel at `http://<pi-ip>:8080/admin`.

---

## Configuration Reference

All settings via environment variables. Set them in the systemd override file:
`sudo systemctl edit chan` — creates `/etc/systemd/system/chan.service.d/override.conf`

| Variable | Default | Description |
|---|---|---|
| `CHAN_BIND` | `0.0.0.0:8080` | TCP address to listen on |
| `CHAN_DB` | `/var/lib/chan/chan.db` | SQLite database path |
| `CHAN_UPLOADS` | `/var/lib/chan/uploads` | Upload storage directory |
| `CHAN_COOKIE_SECRET` | (weak default) | **MUST change** — secret for CSRF/IP hashing |
| `CHAN_MAX_FILE_SIZE` | `8388608` | Max upload size in bytes (8 MiB) |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension in pixels |
| `CHAN_BUMP_LIMIT` | `500` | Replies before thread stops bumping |
| `CHAN_MAX_THREADS` | `150` | Threads per board before old ones pruned |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per rate window per IP |
| `CHAN_RATE_WINDOW` | `60` | Rate limit window in seconds |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration (8 hours) |
| `CHAN_BEHIND_PROXY` | `false` | Set `true` if behind nginx/Caddy |
| `RUST_LOG` | `chan=info` | Log level (use `chan=debug` for verbose) |

---

## Admin Panel Usage

Go to `/admin` → log in → `/admin/panel`.

### Board Management
- **Create board**: Set a short name (e.g. `tech`), full name, description.
- **Delete board**: Permanently deletes the board and ALL its content. Irreversible.

### Thread Moderation
To sticky/lock a thread, post a reply form with admin actions from the thread page.
Alternatively add moderation buttons by posting to `/admin/thread/action`:

```bash
# Sticky a thread via curl (example)
curl -X POST http://localhost:8080/admin/thread/action \
  -b "chan_admin_session=YOUR_SESSION" \
  -d "_csrf=YOUR_CSRF&thread_id=42&board=tech&action=sticky"
```

### Banning Users
Find the `ip_hash` from the database:
```bash
sqlite3 /var/lib/chan/chan.db \
  "SELECT id, ip_hash, created_at FROM posts ORDER BY id DESC LIMIT 20;"
```

Then ban via CLI:
```bash
# Ban for 24 hours
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin ban <ip_hash> "Spamming" 24

# Permanent ban
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin ban <ip_hash> "Repeated violations"

# Unban (get ban ID from list-bans)
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin unban 5
```

### Word Filters
Add via Admin Panel → Word Filters section.
Pattern is a plain text substring match. Replacement can be empty to remove the word.
Example: pattern `badword`, replacement `[removed]`.

---

## Reset Admin Password

```bash
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin reset-password admin "NewSecurePassword456!"
```

Password must be at least 8 characters. The CLI validates and hashes with Argon2id.

---

## Configure Boards

```bash
# List current boards
sudo -u chan CHAN_DB=/var/lib/chan/chan.db /usr/local/bin/chan-admin list-boards

# Add a board
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin create-board art "Artwork" "Original art and photography"

# Add an NSFW board
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin create-board nsfw "NSFW" "Adult content" --nsfw

# Delete a board (requires confirmation)
sudo -u chan CHAN_DB=/var/lib/chan/chan.db /usr/local/bin/chan-admin delete-board oldb
```

Boards can also be created/deleted in the Admin Panel web UI at `/admin/panel`.

---

## Raspberry Pi SD Card Wear Reduction

The SQLite database is configured to minimise writes:
- **WAL mode** — writes go to a WAL file, checkpointed in bulk. Fewer individual syncs.
- **synchronous=NORMAL** — safe with WAL, skips some fsyncs that WAL already guarantees.
- **64 MiB mmap** — reads go through kernel page cache where possible.

**Recommended**: Store the database on a USB SSD or USB flash drive, not the SD card:
```bash
# Format a USB drive
sudo mkfs.ext4 /dev/sda1
sudo mount /dev/sda1 /mnt/chan-data
sudo mkdir -p /mnt/chan-data/uploads

# Update systemd service
sudo systemctl edit chan
# Add:
# Environment=CHAN_DB=/mnt/chan-data/chan.db
# Environment=CHAN_UPLOADS=/mnt/chan-data/uploads

# Persist the mount in /etc/fstab:
echo "UUID=$(blkid -s UUID -o value /dev/sda1) /mnt/chan-data ext4 defaults,noatime 0 2" \
    | sudo tee -a /etc/fstab
```

**Also recommended**: Move `/var/log` and systemd journal to a tmpfs (RAM disk):
```bash
sudo nano /etc/systemd/journald.conf
# Set: Storage=volatile
# This keeps logs in RAM only — lost on reboot, but SD card survives longer.
```

---

## Backup Setup

```bash
# Install backup script
sudo cp deploy/backup.sh /usr/local/bin/chan-backup.sh
sudo chmod +x /usr/local/bin/chan-backup.sh
sudo apt install sqlite3   # needed for .backup command

# Test the backup
sudo -u chan /usr/local/bin/chan-backup.sh

# Schedule daily at 3 AM
sudo crontab -e
# Add: 0 3 * * * /usr/local/bin/chan-backup.sh >> /var/log/chan-backup.log 2>&1
```

---

## Nginx Reverse Proxy (Optional)

For internet exposure with HTTPS:

```bash
sudo apt install nginx certbot python3-certbot-nginx

# Copy and edit the nginx config
sudo cp deploy/nginx.conf /etc/nginx/sites-available/chan
sudo nano /etc/nginx/sites-available/chan   # set your domain name
sudo ln -s /etc/nginx/sites-available/chan /etc/nginx/sites-enabled/

# Get a TLS cert (requires domain pointing to your Pi's IP)
sudo certbot --nginx -d yourdomain.com

# Test and reload
sudo nginx -t
sudo systemctl reload nginx

# Tell Chan it's behind a proxy (so X-Forwarded-For is used for IP)
sudo systemctl edit chan
# Add: Environment=CHAN_BEHIND_PROXY=true
sudo systemctl restart chan
```

For LAN-only use, skip nginx entirely and access `http://<pi-ip>:8080` directly.

---

## Security Hardening Checklist

- [ ] Change `CHAN_COOKIE_SECRET` to a random 64-char hex string (`openssl rand -hex 32`)
- [ ] Change the admin password from the default set during first-time setup
- [ ] Run the service as the `chan` user, not root
- [ ] If exposed to internet: use nginx with HTTPS (certbot)
- [ ] If exposed to internet: set `CHAN_BEHIND_PROXY=true`
- [ ] Firewall: block port 8080 externally if using nginx (`sudo ufw deny 8080`)
- [ ] Review and adjust `CHAN_RATE_POSTS` / `CHAN_RATE_WINDOW` for your audience
- [ ] Set up automated backups (see Backup section above)
- [ ] Consider moving DB and uploads to USB storage (see SD Card section)
- [ ] Monitor disk space: `df -h /var/lib/chan`
- [ ] Read logs: `sudo journalctl -u chan --since "1 hour ago"`

---

## Troubleshooting

**Service won't start:**
```bash
sudo journalctl -u chan -n 50
# Common causes: wrong file permissions, missing uploads dir, bad DB path
```

**Can't upload images:**
```bash
# Check directory permissions
ls -la /var/lib/chan/uploads/
# Should be owned by chan:chan with write permission
sudo chown -R chan:chan /var/lib/chan/uploads/
```

**Admin login doesn't work:**
```bash
# Reset the password
sudo -u chan CHAN_DB=/var/lib/chan/chan.db \
    /usr/local/bin/chan-admin reset-password admin "NewPassword123!"
```

**Database is corrupted:**
```bash
# Integrity check
sqlite3 /var/lib/chan/chan.db "PRAGMA integrity_check;"
# Restore from backup
sudo systemctl stop chan
cp /var/backup/chan/chan_LATEST.db /var/lib/chan/chan.db
sudo systemctl start chan
```

**High memory usage:**
- Each database connection uses ~4 MiB cache. Pool is 8 connections max = ~32 MiB.
- Image processing during uploads may spike to ~64 MiB temporarily.
- Total expected footprint: 50–100 MiB RAM under load. Fine for Pi 4 4GB.

---

## Architecture Notes

```
src/
├── main.rs           — server entry point, router, background tasks
├── lib.rs            — library exports for CLI binary
├── config.rs         — env-var configuration with defaults
├── db.rs             — all SQL queries (no ORM)
├── error.rs          — unified error type with HTTP responses
├── models.rs         — plain data structs (1:1 with DB rows)
├── middleware/
│   └── mod.rs        — rate limiting, CSRF helpers, IP extraction
├── handlers/
│   ├── board.rs      — board index, thread creation, search, delete
│   ├── thread.rs     — thread view, reply posting
│   └── admin.rs      — admin panel, board/ban/filter management
├── templates/
│   └── mod.rs        — pure-Rust HTML rendering (no template engine)
├── utils/
│   ├── crypto.rs     — Argon2 hashing, CSRF tokens, IP hashing
│   ├── files.rs      — upload validation, thumbnail generation
│   ├── sanitize.rs   — HTML escaping, post markup rendering
│   └── tripcode.rs   — SHA-256 tripcode system
└── bin/
    └── chan-admin.rs — CLI admin utility

deploy/
├── chan.service      — systemd service file
├── nginx.conf        — optional reverse proxy config
└── backup.sh         — backup script

static/
└── style.css         — complete CSS (light + dark mode, mobile)
```
