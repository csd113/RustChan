# RustChan Setup Guide

Current setup and deployment guide for Linux, macOS, and Windows.

This guide reflects the current RustChan architecture:

- Tor onion hosting is built in via Arti. You do not install or manage a separate `tor` service.
- `ffmpeg` is optional, but strongly recommended if you want WebP thumbnails, WebM transcoding, video thumbnails, and audio waveforms.

## Contents

1. [What RustChan Needs](#what-rustchan-needs)
2. [Quick Start](#quick-start)
3. [Install Rust](#install-rust)
4. [Install ffmpeg](#install-ffmpeg)
5. [Verify WebP and WebM Support](#verify-webp-and-webm-support)
6. [Build and Run](#build-and-run)
7. [First-Run Files and Layout](#first-run-files-and-layout)
8. [Important settings.toml Options](#important-settingstoml-options)
9. [Tor Onion Service](#tor-onion-service)
10. [HTTPS and TLS](#https-and-tls)
11. [Linux Service Setup](#linux-service-setup)
12. [Reverse Proxy Notes](#reverse-proxy-notes)
13. [Admin Bootstrapping](#admin-bootstrapping)
14. [Banner Artwork Requirements](#banner-artwork-requirements)
15. [Updating](#updating)
16. [Troubleshooting](#troubleshooting)

## What RustChan Needs

RustChan is a single Rust binary. A basic install only needs:

- Rust toolchain to build it
- a writable working directory
- `ffmpeg` if you want the enhanced media pipeline

RustChan does not require:

- Docker
- Postgres or MySQL
- Redis
- a separate Tor daemon

## Quick Start

```bash
git clone https://github.com/csd113/RustChan.git
cd RustChan
cargo build --release
./target/release/rustchan-cli
```

On first launch RustChan creates `rustchan-data/settings.toml`, `rustchan-data/logs/`, and the rest of its runtime directories next to the binary.

Then in another terminal:

```bash
./target/release/rustchan-cli admin create-admin admin "ChangeThisPasswordNow"
./target/release/rustchan-cli admin create-board b "Random" "General discussion"
```

Open:

- `http://localhost:8080`
- admin panel: `http://localhost:8080/admin`

If TLS is enabled in `settings.toml`, RustChan also serves HTTPS on port `8443` by default.

## Install Rust

### Linux and macOS

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version
cargo --version
```

### Windows

Install Rust with `rustup-init.exe` from [rustup.rs](https://rustup.rs), then open a new PowerShell window and verify:

```powershell
rustc --version
cargo --version
```

## Install ffmpeg

`ffmpeg` is optional, but RustChan is significantly better with it.

When `ffmpeg` is available, RustChan can:

- extract video thumbnails
- generate audio waveform thumbnails
- convert supported image thumbnails to WebP
- transcode MP4 uploads to WebM when VP9 and Opus are available

Without `ffmpeg`, RustChan still runs, but video and audio handling degrades gracefully.

### Debian / Ubuntu / Raspberry Pi OS

```bash
sudo apt update
sudo apt install -y ffmpeg
```

### Fedora

```bash
sudo dnf install -y ffmpeg
```

If your base Fedora repos do not provide the codec-enabled build you want, use RPM Fusion.

### macOS

```bash
brew install ffmpeg
```

### Windows

```powershell
winget install --id Gyan.FFmpeg -e
```

Then make sure the FFmpeg `bin` directory is on `PATH`.

### Verify ffmpeg Exists

```bash
ffmpeg -version
ffprobe -version
```

If you want RustChan to refuse startup when `ffmpeg` is missing, set:

```toml
require_ffmpeg = true
```

## Verify WebP and WebM Support

RustChan checks more than just whether `ffmpeg` exists. It also checks whether your build includes:

- `libwebp` for WebP image thumbnails and conversions
- `libvpx-vp9` for WebM video encoding
- `libopus` for WebM audio encoding

Use these commands:

```bash
ffmpeg -encoders | rg libwebp
ffmpeg -encoders | rg libvpx-vp9
ffmpeg -encoders | rg libopus
```

If you do not have `rg`, use:

```bash
ffmpeg -encoders | grep libwebp
ffmpeg -encoders | grep libvpx-vp9
ffmpeg -encoders | grep libopus
```

You want all three to appear.

### What Each Encoder Enables

- `libwebp`: WebP thumbnail and image conversion support
- `libvpx-vp9` + `libopus`: MP4 to WebM transcoding support

### Linux Notes

On Debian-family systems, the usual install is:

```bash
sudo apt update
sudo apt install -y ffmpeg libwebp-dev libvpx-dev libopus-dev
```

The important part is still the actual `ffmpeg -encoders` output. Package names alone do not guarantee your installed FFmpeg binary was built with every encoder enabled.

### macOS Notes

Most Homebrew FFmpeg installs are fine, but verify with:

```bash
ffmpeg -encoders | rg 'libwebp|libvpx-vp9|libopus'
```

If one is missing, reinstall FFmpeg from a build source that includes that codec set.

### Windows Notes

Use a full FFmpeg build rather than a minimal one, then verify with:

```powershell
ffmpeg -encoders | Select-String libwebp
ffmpeg -encoders | Select-String libvpx-vp9
ffmpeg -encoders | Select-String libopus
```

### What RustChan Does If Support Is Missing

RustChan will log warnings and continue:

- missing `libwebp`: image thumbnails stay in original-friendly formats where needed
- missing VP9 or Opus: MP4 uploads are stored as MP4 instead of transcoded to WebM

These warnings appear in the console at startup and in `rustchan-data/logs/`.

## Build and Run

### Build

```bash
cargo build --release
```

Binary:

- Linux/macOS: `target/release/rustchan-cli`
- Windows: `target/release/rustchan-cli.exe`

### Run

```bash
./target/release/rustchan-cli
```

### Optional CLI Flags

```bash
./target/release/rustchan-cli --port 9090
./target/release/rustchan-cli serve --chan-net
```

## First-Run Files and Layout

By default RustChan stores runtime state in `rustchan-data/` next to the binary:

```text
rustchan-data/
├── settings.toml
├── chan.db
├── logs/
│   └── rustchan.YYYY-MM-DD.log
├── backups/
│   ├── full/
│   └── boards/
├── runtime/
│   ├── tls/
│   ├── tor/
│   │   ├── state/
│   │   └── cache/
│   ├── favicon/
│   └── tmp/
└── boards/

## Banner Artwork Requirements

RustChan `1.1.4` adds board banners plus a separate home-page announcement banner.

Banner upload requirements:

- exact `468x60` aspect ratio
- minimum size `468x60`
- recommended size `936x120`
- input can be PNG, JPEG, or WebP
- RustChan converts uploaded banner images to WebP automatically

Board banner placement:

- board index: under the board name/description, above `[Index] [Catalog] [Archive]`
- catalog: under the board name/description, above `Sort By:` and `Show OP Comment:`
- no banner on thread pages
- no banner on archive pages
- no banner on search pages

Home page banner placement:

- separate centered banner box on the home page
- intended for MOTD/news/announcement use

Banner link behavior:

- internal board and internal-path links work directly
- external links can be enabled in the admin panel
- when enabled, external banner clicks go through an on-site warning page before redirecting
```

Important notes:

- `settings.toml` is generated automatically on first run
- `cookie_secret` is generated automatically on first run
- Tor state and onion keys live under `rustchan-data/runtime/tor/state/`
- logs rotate daily under `rustchan-data/logs/`

## Important settings.toml Options

The generated file documents every setting inline. Commonly tuned settings:

```toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
default_theme = "fluorogrid"
enabled_builtin_themes = ["terminal", "aero", "dorfic", "forest", "chanclassic", "neoncubicle", "fluorogrid"]
port = 8080

max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150

enable_tor_support = true
# tor_only = false
# tor_bootstrap_timeout_secs = 120
# tor_max_concurrent_streams = 512
# tor_service_nickname = "rustchan"

require_ffmpeg = false
# ffmpeg_path = "/usr/local/bin/ffmpeg"
# ffprobe_path = "/usr/local/bin/ffprobe"
ffmpeg_timeout_secs = 120

[tls]
enabled = true
port = 8443
# redirect_http = true
# http_port = 8080
```

### A Few High-Impact Settings

- `enable_tor_support = true`: built-in onion service is on
- `tor_only = true`: bind RustChan to loopback and serve only through Tor
- `require_ffmpeg = true`: fail startup if ffmpeg is missing
- `[tls].enabled = true`: enable RustChan's native HTTPS listener
- `ffmpeg_timeout_secs = 120`: max runtime for a single ffmpeg job

## Tor Onion Service

RustChan includes built-in Tor onion service hosting through Arti.

You do not need to:

- install `tor`
- write a `torrc`
- manage a hidden service directory manually

### Default Behavior

On current builds, the generated `settings.toml` enables Tor support by default:

```toml
enable_tor_support = true
```

On first startup with Tor enabled, RustChan:

1. creates the Tor runtime directories
2. bootstraps to the Tor network
3. generates or loads the onion service keypair
4. starts serving the site over `.onion`

The first bootstrap usually takes longer than later boots because Tor directory data has to be downloaded and cached.

### Where the Onion Key Lives

Back up:

```text
rustchan-data/runtime/tor/state/
```

That directory contains the persistent onion identity. If you lose it, the next startup will generate a new onion address.

### Tor-Only Mode

If you want RustChan reachable only through Tor:

```toml
enable_tor_support = true
tor_only = true
```

In this mode RustChan binds to loopback instead of `0.0.0.0`, so clearnet access is blocked.

### Tor Permissions

RustChan creates the Tor state directory with restricted permissions on Unix. If you move the data directory manually, preserve write access for the RustChan service user.

## HTTPS and TLS

RustChan has built-in HTTPS support.

The generated `settings.toml` currently includes:

```toml
[tls]
enabled = true
port = 8443
```

This means:

- HTTP is available on the main app port
- HTTPS is available on `8443`
- on first run, RustChan can generate a local self-signed development certificate

### Common Modes

#### Local or LAN Testing

Keep built-in TLS enabled and use the self-signed certificate.

#### Public Production Reverse Proxy

Many operators still prefer putting nginx or Caddy in front and terminating TLS there.

#### ACME / Let's Encrypt

RustChan also supports ACME-based certificates when built with the `tls-acme` feature and configured in `[tls.acme]`.

## Linux Service Setup

Run RustChan as a dedicated unprivileged user.

### 1. Create a Service User

```bash
sudo useradd --system --home /var/lib/rustchan --create-home --shell /usr/sbin/nologin rustchan
```

### 2. Build and Install the Binary

```bash
cargo build --release
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli
sudo mkdir -p /var/lib/rustchan
sudo chown -R rustchan:rustchan /var/lib/rustchan
```

### 3. First Start as the Service User

This creates `settings.toml` and the runtime layout:

```bash
sudo -u rustchan -H sh -lc 'cd /var/lib/rustchan && /usr/local/bin/rustchan-cli'
```

Stop it after the first start, edit `/var/lib/rustchan/rustchan-data/settings.toml`, then continue.

### 4. Create a systemd Unit

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
ExecStart=/usr/local/bin/rustchan-cli
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

Then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rustchan
sudo journalctl -u rustchan -f
```

### 5. Optional Environment Overrides

You can add overrides with:

```bash
sudo systemctl edit rustchan
```

Example:

```ini
[Service]
Environment=CHAN_BIND=127.0.0.1:8080
Environment=CHAN_REQUIRE_FFMPEG=true
```

## Reverse Proxy Notes

If you put nginx or Caddy in front of RustChan:

- point the proxy at the RustChan HTTP listener
- set `CHAN_BEHIND_PROXY=true` if you want proxy headers trusted
- set `CHAN_TRUSTED_PROXY_CIDRS` to the proxy's loopback or private CIDR when the proxy is not on localhost
- decide whether TLS terminates at the proxy or inside RustChan

Typical loopback setup:

```text
internet -> nginx/caddy -> 127.0.0.1:8080 -> RustChan
```

If you use a reverse proxy and terminate TLS there, make sure your proxy forwards the usual headers and that RustChan is not accidentally exposed directly on the public interface.

## Admin Bootstrapping

Create the first admin account:

```bash
./target/release/rustchan-cli admin create-admin admin "UseAStrongPassword"
```

Create a board:

```bash
./target/release/rustchan-cli admin create-board tech "Technology" "Programming and hardware"
```

Other useful commands:

```bash
./target/release/rustchan-cli admin list-admins
./target/release/rustchan-cli admin list-boards
./target/release/rustchan-cli admin reset-password admin "NewStrongPassword"
```

## Updating

```bash
git pull
cargo build --release
sudo install -o root -g root -m 0755 target/release/rustchan-cli /usr/local/bin/rustchan-cli
sudo systemctl restart rustchan
```

Before major updates, back up:

- `rustchan-data/chan.db`
- `rustchan-data/boards/`
- `rustchan-data/runtime/tor/state/`
- `rustchan-data/settings.toml`

Or use the built-in backup tools from the admin panel.

## Troubleshooting

### The TUI shows ffmpeg warnings

Run:

```bash
ffmpeg -version
ffmpeg -encoders | rg 'libwebp|libvpx-vp9|libopus'
```

If one of those encoders is missing, RustChan will still run but some media features will be downgraded.

### Tor never becomes ready

Check:

- outbound network connectivity
- whether the RustChan service user can write to `rustchan-data/runtime/tor/`
- whether `tor_bootstrap_timeout_secs` needs to be raised

Also review:

```text
rustchan-data/logs/rustchan.YYYY-MM-DD.log
```

### HTTPS fails on startup

Check:

- whether `[tls] enabled = true` is intentional
- whether the configured HTTPS port is available
- whether your ACME or manual cert settings are correct if you use those modes

### The service starts but uploads fail

Make sure the RustChan user can write to:

- `rustchan-data/`
- the uploads directory
- `rustchan-data/runtime/`

### The onion address changed unexpectedly

That usually means the Tor state directory was deleted, replaced, or not persisted:

```text
rustchan-data/runtime/tor/state/
```

Back that directory up if the onion address matters.
