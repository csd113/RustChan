# RustChan — Production Setup Guide

This guide covers a full production deployment on Linux, macOS, or Windows. Follow the sections in order for a clean, secure install.

---

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Installing ffmpeg](#installing-ffmpeg)
3. [Installing Tor](#installing-tor)
4. [Building the Binary](#building-the-binary)
5. [System User and Directory Layout](#system-user-and-directory-layout)
6. [Installing the Service](#installing-the-service)
7. [First-Run Configuration](#first-run-configuration)
8. [nginx Reverse Proxy and TLS](#nginx-reverse-proxy-and-tls)
9. [Configuring a Tor Hidden Service](#configuring-a-tor-hidden-service)
10. [Configuration Reference](#configuration-reference)
11. [Admin Panel Usage](#admin-panel-usage)
12. [Backup and Recovery](#backup-and-recovery)
13. [Raspberry Pi — SD Card Wear Reduction](#raspberry-pi--sd-card-wear-reduction)
14. [Security Hardening Checklist](#security-hardening-checklist)
15. [Troubleshooting](#troubleshooting)

---

## Prerequisites

### Supported Platforms

RustChan compiles and runs on any platform with a Rust toolchain. Tested targets:

- Linux x86-64 (Debian, Ubuntu, Fedora)
- Linux ARM64 (Raspberry Pi OS 64-bit, Ubuntu 22.04+ for Pi)
- macOS 13+ (Apple Silicon and Intel)
- Windows 10/11 x86-64

### Install Rust

Always install Rust via `rustup` — do not use your distribution's package manager version.

```bash
# Linux / macOS
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version   # should be 1.75 or newer

# Windows — download and run rustup-init.exe from https://rustup.rs
# Then open a new terminal and run:
rustc --version
```

### System Packages (Linux)

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

## Installing ffmpeg

ffmpeg is **optional**. When present on `PATH`, RustChan uses it for:

- Transcoding MP4 uploads to VP9+Opus WebM automatically
- Re-encoding AV1 WebM to VP9 for broad browser compatibility
- Generating audio waveform PNG thumbnails for audio posts
- Generating video thumbnails from the first frame of uploaded videos

Without ffmpeg, RustChan serves video files in their original uploaded format and shows a generic icon for audio posts. A warning is logged at startup but the server runs normally.

### Linux

**Debian / Ubuntu / Raspberry Pi OS:**

```bash
sudo apt update && sudo apt install -y ffmpeg
ffmpeg -version   # verify install
```

**Fedora / RHEL / CentOS Stream:**

```bash
# Enable RPM Fusion for the full ffmpeg build (includes proprietary codecs)
sudo dnf install -y \
    https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm \
    https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install -y ffmpeg
ffmpeg -version
```

**Alpine Linux (e.g. minimal VPS):**

```bash
apk add ffmpeg
ffmpeg -version
```

After installing, restart RustChan so it detects ffmpeg at the next startup probe:

```bash
sudo systemctl restart rustchan-cli
sudo journalctl -u rustchan-cli -n 20   # look for "ffmpeg detected"
```

### macOS

The easiest method is [Homebrew](https://brew.sh). If you don't have Homebrew, install it first:

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

Then install ffmpeg:

```bash
brew install ffmpeg
ffmpeg -version
```

Homebrew installs to `/opt/homebrew/bin/ffmpeg` on Apple Silicon and `/usr/local/bin/ffmpeg` on Intel Macs. Both locations are on the default PATH after a Homebrew install. RustChan probes both paths at startup.

**Keeping ffmpeg up to date:**

```bash
brew upgrade ffmpeg
```

### Windows

**Option A — winget (Windows 10/11, recommended):**

```powershell
winget install --id Gyan.FFmpeg -e
```

After install, ffmpeg will be at `C:\Program Files\FFmpeg\bin\ffmpeg.exe`. You need to add it to your PATH:

1. Open **System Properties** → **Advanced** → **Environment Variables**
2. Under **System variables**, select **Path** and click **Edit**
3. Click **New** and add: `C:\Program Files\FFmpeg\bin`
4. Click **OK** on all dialogs
5. Open a new terminal and verify: `ffmpeg -version`

**Option B — Manual install:**

1. Download a Windows build from [https://www.gyan.dev/ffmpeg/builds/](https://www.gyan.dev/ffmpeg/builds/) (choose the `ffmpeg-release-essentials` zip)
2. Extract to `C:\ffmpeg`
3. Add `C:\ffmpeg\bin` to your PATH using the steps above
4. Verify: `ffmpeg -version`

**Running RustChan on Windows:** start the server from a terminal where ffmpeg is on PATH. If using a service manager like NSSM, ensure the PATH includes the ffmpeg bin directory in the service's environment.

---

## Installing Tor

Tor is **optional**. When `enable_tor_support = true` is set in `settings.toml` and a Tor daemon is running, RustChan reads your `.onion` address from the hidden-service hostname file and displays it on the home page and admin panel.

> **How it works:** You configure Tor as a hidden service pointing at RustChan's port. Tor handles all onion routing. RustChan only reads the resulting `hostname` file to display the address — it never communicates with the Tor network directly.

### Linux

**Debian / Ubuntu / Raspberry Pi OS:**

```bash
sudo apt update && sudo apt install -y tor
sudo systemctl enable --now tor
tor --version   # verify
```

**Fedora / RHEL:**

```bash
sudo dnf install -y tor
sudo systemctl enable --now tor
tor --version
```

**Verify Tor is running:**

```bash
sudo systemctl status tor
```

You will configure the hidden service in the [Configuring a Tor Hidden Service](#configuring-a-tor-hidden-service) section below.

### macOS

**Via Homebrew:**

```bash
brew install tor
```

To start Tor automatically at login:

```bash
brew services start tor
```

To start it only for the current session:

```bash
tor &
```

Verify: `tor --version`

The Homebrew Tor configuration file lives at:
- Apple Silicon: `/opt/homebrew/etc/tor/torrc`
- Intel Mac: `/usr/local/etc/tor/torrc`

RustChan's startup probe checks `/opt/homebrew/bin/tor` and `/usr/local/bin/tor` in addition to bare `tor` on PATH, so Homebrew installs are detected automatically.

### Windows

**Option A — Tor Expert Bundle (recommended for server use):**

1. Download the **Tor Expert Bundle** from [https://www.torproject.org/download/tor/](https://www.torproject.org/download/tor/)
2. Extract to `C:\tor`
3. Add `C:\tor` to your PATH (same process as described for ffmpeg above)
4. Verify: `tor --version`

To run Tor as a Windows service, use the included service installer:

```powershell
# From an elevated (Administrator) PowerShell
cd C:\tor
.\tor.exe --service install --options -f C:\tor\torrc
.\tor.exe --service start
```

**Option B — Tor Browser bundle (development/testing only):**

The Tor Browser includes a copy of Tor that can be run headlessly, but the Expert Bundle above is cleaner for server deployments.

**Configuration file:** create or edit `C:\tor\torrc` — see the [hidden service section](#configuring-a-tor-hidden-service) below for the directives to add.

---

## Building the Binary

### Build on the target machine

```bash
git clone https://your-repo/rustchan.git
cd rustchan
cargo build --release
```

The binary is at `target/release/rustchan-cli` (or `rustchan-cli.exe` on Windows). It is fully self-contained — copy it anywhere.

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
target/aarch64-unknown-linux-gnu/release/rustchan-cli
```

Transfer to your server:

```bash
scp target/aarch64-unknown-linux-gnu/release/rustchan-cli pi@raspberrypi.local:~/rustchan-cli
```

---

## System User and Directory Layout

Run RustChan as a dedicated unprivileged user. Never run it as root.

```bash
# Create a system user with no login shell and a fixed home directory
sudo useradd --system --shell /usr/sbin/nologin --home /var/lib/chan --create-home chan

# Create required directories
sudo mkdir -p /var/lib/chan/rustchan-data/boards/thumbs
sudo chown -R chan:chan /var/lib/chan

# Install the binary
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli

# Verify
/usr/local/bin/rustchan-cli --version
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

Before starting the service, generate a unique secret key. This value is used for CSRF tokens and IP hashing. **Do not change it after your instance has posts** — existing IP hashes and bans will become invalid.

```bash
openssl rand -hex 32
# Example output: a3f8c1d2e4b56789...
```

### systemd Service (Linux)

Copy the provided unit file and configure your secret:

```bash
sudo cp deploy/rustchan-cli.service /etc/systemd/system/rustchan-cli.service
```

Create a drop-in override to supply the secret without editing the base unit file:

```bash
sudo systemctl edit rustchan-cli
```

Add the following (replace `<your-secret>` with the output of the `openssl` command above):

```ini
[Service]
Environment=CHAN_COOKIE_SECRET=<your-secret>
Environment=CHAN_DB=/var/lib/chan/rustchan-data/chan.db
Environment=CHAN_UPLOADS=/var/lib/chan/rustchan-data/boards
WorkingDirectory=/var/lib/chan
```

Reload and enable:

```bash
sudo systemctl daemon-reload
sudo systemctl enable rustchan-cli
sudo systemctl start rustchan-cli
sudo systemctl status rustchan-cli
```

Follow logs:

```bash
sudo journalctl -u rustchan-cli -f
```

### Provided systemd Unit (deploy/rustchan-cli.service)

The included unit file sets:

- `User=chan` and `Group=chan` — runs as the unprivileged user
- `Restart=on-failure` — automatically restarts on crash
- `NoNewPrivileges=true` — prevents privilege escalation
- `PrivateTmp=true` — isolated /tmp namespace
- `ProtectSystem=strict` with `ReadWritePaths` for the data directory

### Windows Service

Use [NSSM](https://nssm.cc) (Non-Sucking Service Manager) to run RustChan as a Windows service:

```powershell
# Download nssm.exe and place it on PATH, then:
nssm install RustChan "C:\path\to\rustchan-cli.exe"
nssm set RustChan AppDirectory "C:\rustchan"
nssm set RustChan AppEnvironmentExtra "CHAN_COOKIE_SECRET=<your-secret>"
nssm start RustChan
```

---

## First-Run Configuration

On first start, `rustchan-data/settings.toml` is auto-generated with a random `cookie_secret`. Review and edit it before creating content:

```bash
sudo -u chan nano /var/lib/chan/rustchan-data/settings.toml
```

Key settings to review:

```toml
forum_name = "My Chan"       # Change to your site name
port = 8080                  # Internal port (nginx will proxy to this)
max_image_size_mb = 8        # Raise if you want larger image uploads
max_video_size_mb = 50       # Raise for longer video uploads
enable_tor_support = true    # Set true if you are running a Tor hidden service
require_ffmpeg = false       # Set true to fail hard if ffmpeg is missing
```

Restart after editing:

```bash
sudo systemctl restart rustchan-cli
```

### Create the First Admin Account

```bash
sudo -u chan \
    CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-admin admin "YourSecurePassword123!"
```

### Create Boards

```bash
sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-board b    "Random"     "General discussion"

sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-board tech "Technology" "Programming and hardware"

sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-board meta "Meta"       "About this site"

# NSFW board (shown separately on the home page)
sudo -u chan CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin create-board nsfw "NSFW"       "Adult content" --nsfw
```

Boards can also be created and managed in the web-based admin panel at `/admin/panel`.

---

## nginx Reverse Proxy and TLS

For internet-facing deployments, put RustChan behind nginx with a TLS certificate from Let's Encrypt.

### Install nginx Config

```bash
sudo cp deploy/nginx.conf /etc/nginx/sites-available/rustchan-cli
sudo nano /etc/nginx/sites-available/rustchan-cli   # set your domain name
sudo ln -sf /etc/nginx/sites-available/rustchan-cli /etc/nginx/sites-enabled/rustchan-cli
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

# Verify renewal works:
sudo certbot renew --dry-run
```

### Enable Proxy Mode

Tell RustChan to trust the `X-Forwarded-For` header from nginx:

```bash
sudo systemctl edit rustchan-cli
```

Add:

```ini
[Service]
Environment=CHAN_BEHIND_PROXY=true
```

With `CHAN_BEHIND_PROXY=true`, RustChan also automatically sets `Secure=true` on all cookies. Restart:

```bash
sudo systemctl restart rustchan-cli
```

### Firewall

```bash
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw deny 8080/tcp
sudo ufw enable
```

---

## Configuring a Tor Hidden Service

This section assumes you have already installed Tor using the instructions above and that RustChan is running and reachable on its local port (default `8080`).

### Linux

Edit the Tor configuration file:

```bash
sudo nano /etc/tor/torrc
```

Add the following two lines anywhere in the file (create the directory if it does not exist):

```
HiddenServiceDir /var/lib/tor/rustchan/
HiddenServicePort 80 127.0.0.1:8080
```

This tells Tor to create a hidden service that forwards `.onion` port 80 traffic to RustChan on `127.0.0.1:8080`.

Save, then restart Tor:

```bash
sudo systemctl restart tor
```

After a moment, Tor generates your onion address:

```bash
sudo cat /var/lib/tor/rustchan/hostname
# Output: abc123xyz...onion
```

That address is your `.onion` URL. RustChan reads it automatically at startup when `enable_tor_support = true` is set in `settings.toml`, and displays it on the home page and admin panel.

**Permissions:** the hidden-service directory must be owned by the `debian-tor` (or `tor`) user and have mode `700`. Tor sets this automatically — do not change it or Tor will refuse to use the directory.

```bash
ls -la /var/lib/tor/rustchan/
# drwx------ debian-tor debian-tor ...
```

### macOS

Edit the Tor configuration file:

```bash
# Apple Silicon (Homebrew)
nano /opt/homebrew/etc/tor/torrc

# Intel Mac (Homebrew)
nano /usr/local/etc/tor/torrc
```

Add:

```
HiddenServiceDir /usr/local/var/lib/tor/rustchan/
HiddenServicePort 80 127.0.0.1:8080
```

Create the directory and set permissions:

```bash
mkdir -p /usr/local/var/lib/tor/rustchan
chmod 700 /usr/local/var/lib/tor/rustchan
```

Restart Tor:

```bash
brew services restart tor
```

Read your onion address:

```bash
cat /usr/local/var/lib/tor/rustchan/hostname
```

Update `settings.toml` to set `enable_tor_support = true` and restart RustChan so it picks up the address.

### Windows

Edit (or create) `C:\tor\torrc` in a text editor run as Administrator. Add:

```
HiddenServiceDir C:\tor\hidden_service\rustchan
HiddenServicePort 80 127.0.0.1:8080
```

Create the directory:

```powershell
mkdir C:\tor\hidden_service\rustchan
```

Restart the Tor service:

```powershell
# If installed as a service:
nssm restart tor

# If running manually:
# Stop the existing process and re-run tor.exe
```

Read your onion address:

```powershell
type C:\tor\hidden_service\rustchan\hostname
```

Set `enable_tor_support = true` in `rustchan-data\settings.toml` and restart RustChan. It will probe for the `hostname` file and display the address on startup.

### Pointing RustChan at the hostname file

By default, RustChan probes the standard Tor hidden-service paths for each platform. If your `HiddenServiceDir` is in a non-standard location, set the `CHAN_TOR_HOSTNAME_FILE` environment variable to the full path of the `hostname` file:

```bash
# systemd override example
Environment=CHAN_TOR_HOSTNAME_FILE=/custom/path/tor/rustchan/hostname
```

---

## Configuration Reference

All settings available as environment variables. Set them in the systemd override file (`sudo systemctl edit rustchan-cli`) to avoid editing the base unit file.

| Variable | Default | Description |
|---|---|---|
| `CHAN_FORUM_NAME` | `RustChan` | Site display name |
| `CHAN_BIND` | `0.0.0.0:8080` | TCP bind address |
| `CHAN_PORT` | `8080` | Port only (used if `CHAN_BIND` not set) |
| `CHAN_DB` | `<exe-dir>/rustchan-data/chan.db` | SQLite database path |
| `CHAN_UPLOADS` | `<exe-dir>/rustchan-data/boards` | Upload storage directory |
| `CHAN_COOKIE_SECRET` | *(from settings.toml)* | Secret for CSRF tokens and IP hashing. **Required in production.** |
| `CHAN_MAX_IMAGE_MB` | `8` | Max image upload in MiB (JPEG, PNG, GIF, WebP) |
| `CHAN_MAX_VIDEO_MB` | `50` | Max video upload in MiB (MP4, WebM) |
| `CHAN_MAX_AUDIO_MB` | `150` | Max audio upload in MiB |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension in pixels |
| `CHAN_BUMP_LIMIT` | `500` | Replies before a thread stops being bumped |
| `CHAN_MAX_THREADS` | `150` | Threads per board before oldest is pruned/archived |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per rate window per IP hash |
| `CHAN_RATE_WINDOW` | `60` | Rate-limit window in seconds |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration (default: 8 hours) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For`; enables `Secure` cookies |
| `CHAN_HTTPS_COOKIES` | *(same as `CHAN_BEHIND_PROXY`)* | Force `Secure` flag on cookies independently |
| `CHAN_WAL_CHECKPOINT_SECS` | `3600` | WAL checkpoint interval in seconds; `0` to disable |
| `RUST_LOG` | `rustchan-cli=info` | Tracing log level. Use `=debug` for verbose output. |

---

## Admin Panel Usage

Log in at `/admin` with the credentials created during setup. All moderation actions are available from `/admin/panel`.

### Board Settings

Click any board name to expand its settings card. You can change the display name, description, bump limit, max threads, NSFW status, and feature toggles (video embeds, PoW CAPTCHA, post editing, archive) without restarting the server.

### Per-Post Inline Ban+Delete

While logged in as admin, every post in thread view shows a ⛔ **ban+del** button. Clicking it opens a browser prompt for the ban reason and duration (hours; enter `0` for permanent). Confirming simultaneously bans the poster's IP hash and deletes the post — or the entire thread if the post is the OP.

### Ban Appeals

When a banned user tries to post, they see the ban reason and a textarea to submit an appeal. Open appeals appear in the **// ban appeals** section of the admin panel. Use **✕ dismiss** to reject an appeal without lifting the ban, or **✓ accept + unban** to remove the ban and close the appeal.

### IP History

Every post shown in admin view has a 🔍 **ip history** link that opens a paginated list of every post that IP hash has ever made across all boards. Useful for evaluating ban appeals or identifying ban-evading users.

### Word Filters

Add filters from the admin panel under **Word Filters**. Patterns are plain-text substring matches. The replacement field can be left empty to silently remove the matched text.

---

## Backup and Recovery

### Web-Based Backup (Recommended)

All backup operations are available directly from the admin panel — no shell access required. Full backups include the database and all uploaded files. Per-board backups are self-contained and can be used to move a single board to another instance.

### Automated Shell Backups

```bash
# Install the script
sudo cp deploy/backup.sh /usr/local/bin/rustchan-cli-backup
sudo chmod +x /usr/local/bin/rustchan-cli-backup

# Schedule daily at 03:00
sudo crontab -e
```

Add:

```
0 3 * * * /usr/local/bin/rustchan-cli-backup >> /var/log/rustchan-cli-backup.log 2>&1
```

### Manual Backup

```bash
sudo systemctl stop rustchan-cli
sudo -u chan sqlite3 /var/lib/chan/rustchan-data/chan.db \
    ".backup /var/backup/chan/chan-manual.db"
sudo systemctl start rustchan-cli

# Also back up uploads
sudo tar czf /var/backup/chan/boards-$(date +%F).tar.gz \
    -C /var/lib/chan/rustchan-data boards
```

### Restore from Backup

```bash
sudo systemctl stop rustchan-cli
sudo cp /var/backup/chan/chan_YYYY-MM-DD.db \
        /var/lib/chan/rustchan-data/chan.db
sudo chown chan:chan /var/lib/chan/rustchan-data/chan.db
sudo systemctl start rustchan-cli
sudo journalctl -u rustchan-cli -n 20   # verify clean startup
```

---

## Raspberry Pi — SD Card Wear Reduction

SQLite writes are already minimised by RustChan's WAL + `synchronous=NORMAL` settings. For long-running deployments, follow these additional steps.

### Move the Database to USB Storage

```bash
sudo mkfs.ext4 /dev/sda1
sudo mkdir -p /mnt/rustchan-data
sudo mount /dev/sda1 /mnt/rustchan-data
sudo mkdir -p /mnt/rustchan-data/boards/thumbs
sudo chown -R chan:chan /mnt/rustchan-data

sudo systemctl stop rustchan-cli
sudo rsync -av /var/lib/chan/rustchan-data/ /mnt/rustchan-data/
sudo systemctl start rustchan-cli

echo "UUID=$(blkid -s UUID -o value /dev/sda1) /mnt/rustchan-data ext4 defaults,noatime 0 2" \
    | sudo tee -a /etc/fstab
```

Update the systemd override:

```ini
[Service]
Environment=CHAN_DB=/mnt/rustchan-data/chan.db
Environment=CHAN_UPLOADS=/mnt/rustchan-data/boards
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

```bash
sudo systemctl restart systemd-journald
```

### Enable `noatime` on the SD Card Root

```bash
sudo nano /etc/fstab
# Add 'noatime' to the root partition options:
# /dev/mmcblk0p2  /  ext4  defaults,noatime  0  1
```

---

## Security Hardening Checklist

Work through this list before exposing your instance to the internet.

- [ ] `CHAN_COOKIE_SECRET` is set to a unique random value (`openssl rand -hex 32`)
- [ ] The default admin password has been changed to something strong
- [ ] The service runs as the `chan` user, not root
- [ ] Port 8080 is firewalled from external access (`sudo ufw deny 8080/tcp`)
- [ ] nginx is configured with HTTPS via Let's Encrypt
- [ ] `CHAN_BEHIND_PROXY=true` is set in the systemd override
- [ ] `client_max_body_size` in nginx matches or exceeds your configured video size limit
- [ ] `CHAN_RATE_POSTS` and `CHAN_RATE_WINDOW` are tuned for your expected audience size
- [ ] Automated daily backups are scheduled and the restore procedure has been tested
- [ ] Database is on USB storage (Raspberry Pi deployments)
- [ ] systemd hardening directives are active (`NoNewPrivileges`, `PrivateTmp`)
- [ ] If running a Tor hidden service, the `HiddenServiceDir` is owned by the `tor` user with mode `700`
- [ ] Log monitoring is in place: `sudo journalctl -u rustchan-cli --since "1 hour ago"`

---

## Troubleshooting

### Service fails to start

```bash
sudo journalctl -u rustchan-cli -n 50 --no-pager
```

Common causes:

- `CHAN_DB` or `CHAN_UPLOADS` path does not exist or is not owned by `chan`
- `CHAN_BIND` port is already in use (`sudo ss -tlnp | grep 8080`)
- Binary is not executable or was built for the wrong architecture

### ffmpeg not detected

RustChan logs `ffmpeg not found` at startup if it cannot locate the binary. Check:

```bash
which ffmpeg          # should return a path
ffmpeg -version       # should print version info
```

If ffmpeg is installed but not on PATH (e.g. a custom install location), add its `bin` directory to the PATH in your systemd override:

```ini
[Service]
Environment=PATH=/usr/local/bin:/usr/bin:/bin:/path/to/ffmpeg/bin
```

On macOS with Homebrew, ensure your shell profile exports the Homebrew path:

```bash
# Add to ~/.zprofile or ~/.bash_profile
eval "$(/opt/homebrew/bin/brew shellenv)"   # Apple Silicon
```

### Tor onion address not showing

1. Verify Tor is running: `sudo systemctl status tor`
2. Verify the hidden service directory exists and contains a `hostname` file:
   ```bash
   sudo ls -la /var/lib/tor/rustchan/
   sudo cat /var/lib/tor/rustchan/hostname
   ```
3. Verify `enable_tor_support = true` is set in `settings.toml`
4. Restart RustChan so it re-probes: `sudo systemctl restart rustchan-cli`
5. Check the startup log for Tor-related lines: `sudo journalctl -u rustchan-cli -n 30`

If the `hostname` file exists but RustChan can't read it, the process may lack permission. The safest fix is to run the startup probe as a user that can read the file, or copy the hostname to a location readable by the `chan` user:

```bash
# One-time copy (re-run after Tor regenerates the key)
sudo cp /var/lib/tor/rustchan/hostname /var/lib/chan/rustchan-data/tor-hostname
sudo chown chan:chan /var/lib/chan/rustchan-data/tor-hostname
```

Then set in your systemd override:

```ini
Environment=CHAN_TOR_HOSTNAME_FILE=/var/lib/chan/rustchan-data/tor-hostname
```

### Image/video uploads fail

```bash
# Verify directory ownership
ls -la /var/lib/chan/rustchan-data/boards/
sudo chown -R chan:chan /var/lib/chan/rustchan-data/boards/

# Check nginx client_max_body_size
sudo nginx -T | grep client_max_body_size
```

### Admin login fails

```bash
sudo -u chan \
    CHAN_DB=/var/lib/chan/rustchan-data/chan.db \
    /usr/local/bin/rustchan-cli admin reset-password admin "NewPassword123!"
```

### Database integrity check

```bash
sqlite3 /var/lib/chan/rustchan-data/chan.db "PRAGMA integrity_check;"
# Expected output: ok
```

### High memory usage

Expected footprint under load:

- Database connection pool (8 connections × ~4 MiB cache): ~32 MiB
- Image processing during uploads: temporary spike up to ~64 MiB
- Steady-state idle: 30–60 MiB

Well within the limits of a Raspberry Pi 4 (2 GB+). If memory grows unbounded, check for a large number of simultaneous upload requests and consider lowering `CHAN_RATE_POSTS`.

---

## Updating RustChan

```bash
# 1. Pull and build
git pull
cargo build --release

# 2. Stop the service
sudo systemctl stop rustchan-cli

# 3. Back up before upgrading
sudo -u chan sqlite3 /var/lib/chan/rustchan-data/chan.db \
    ".backup /var/backup/chan/pre-upgrade-$(date +%F).db"

# 4. Install the new binary
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli

# 5. Also update static assets if they changed
sudo cp static/style.css /var/lib/chan/static/style.css
sudo chown chan:chan /var/lib/chan/static/style.css

# 6. Restart
sudo systemctl start rustchan-cli
sudo journalctl -u rustchan-cli -n 30   # verify clean startup
```

Database schema migrations are additive and run automatically on startup — no manual SQL is ever required when upgrading.
