# Chan Imageboard — Single Binary

A complete 4chan-style imageboard compiled to one self-contained binary.  
No runtime dependencies. No `apt install`. Drop it and run.

---

## Quick Start

```bash
# 1. Build
cargo build --release

# 2. Copy binary wherever you want
cp target/release/chan ~/chan

# 3. Create your first admin + boards
./chan admin create-admin admin "YourPassword123!"
./chan admin create-board b     "Random"     "Anything goes"
./chan admin create-board tech  "Technology" "Programming and hardware"

# 4. Start the server
./chan
```

Open `http://localhost:8080` in your browser.

All data is stored in `./chan-data/` next to the binary:
```
chan                  ← the binary
chan-data/
  chan.db             ← SQLite database (auto-created)
  uploads/            ← user images
  uploads/thumbs/     ← thumbnails
```

---

## Cross-Compiling

### For Raspberry Pi 4 (ARM64) from macOS/Linux

```bash
# Install the ARM64 target
rustup target add aarch64-unknown-linux-gnu

# macOS — install cross-linker via Homebrew
brew install arm-linux-gnueabihf-binutils
# Or use the 'cross' tool (recommended, uses Docker):
cargo install cross

# Build with cross
cross build --release --target aarch64-unknown-linux-gnu

# Binary is at:
target/aarch64-unknown-linux-gnu/release/chan

# SCP to Pi:
scp target/aarch64-unknown-linux-gnu/release/chan pi@raspberrypi.local:~/chan
```

### For Apple Silicon (M1/M2) macOS

```bash
# Just build natively on the Mac:
cargo build --release
# Binary: target/release/chan
```

---

## Admin Commands

```bash
# User management
chan admin create-admin  <username> <password>
chan admin reset-password <username> <new-password>
chan admin list-admins

# Board management
chan admin create-board  <short> <name> [description] [--nsfw]
chan admin delete-board  <short>
chan admin list-boards

# Bans
chan admin ban     <ip_hash> <reason> [duration_hours]
chan admin unban   <ban_id>
chan admin list-bans
```

Example full setup:
```bash
chan admin create-admin admin "Secur3P@ss"
chan admin create-board b    "Random"     "General discussion"
chan admin create-board tech "Technology" "Programming, hardware"
chan admin create-board meta "Meta"       "About this board"
```

---

## Configuration

All settings via environment variables — defaults just work out of the box.

| Variable            | Default                      | Description                              |
|---------------------|------------------------------|------------------------------------------|
| `CHAN_BIND`         | `0.0.0.0:8080`               | TCP bind address                         |
| `CHAN_DB`           | `<exe-dir>/chan-data/chan.db` | SQLite database path                     |
| `CHAN_UPLOADS`      | `<exe-dir>/chan-data/uploads` | Upload directory                         |
| `CHAN_COOKIE_SECRET`| (weak default)               | **Change this in production!**           |
| `CHAN_MAX_FILE_SIZE`| `8388608` (8 MiB)            | Max upload size                          |
| `CHAN_THUMB_SIZE`   | `250`                        | Thumbnail max dimension (px)             |
| `CHAN_BUMP_LIMIT`   | `500`                        | Replies before thread stops bumping      |
| `CHAN_MAX_THREADS`  | `150`                        | Threads per board before oldest pruned   |
| `CHAN_RATE_POSTS`   | `10`                         | Max POSTs per rate window                |
| `CHAN_RATE_WINDOW`  | `60`                         | Rate limit window (seconds)              |
| `CHAN_SESSION_SECS` | `28800` (8 hours)            | Admin session duration                   |
| `CHAN_BEHIND_PROXY` | `false`                      | Set `true` behind nginx/Caddy            |
| `RUST_LOG`         | `chan=info`                  | Log level                                |

---

## Running as a systemd Service (Raspberry Pi)

```ini
# /etc/systemd/system/chan.service
[Unit]
Description=Chan Imageboard
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi
ExecStart=/home/pi/chan
Restart=on-failure
Environment=CHAN_COOKIE_SECRET=<run: openssl rand -hex 32>

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now chan
sudo journalctl -u chan -f
```

---

## Terminal Monitoring

Every 60 seconds the server prints a stats line:

```
── STATS  uptime 2h05m  │  requests 1234  │  boards 3  threads 89  posts 412  │  db 2048 KiB  uploads 15.3 MiB ──
```

---

## Security Checklist

- [ ] Set `CHAN_COOKIE_SECRET` to a random 64-char hex string
- [ ] Change default admin password immediately
- [ ] Run as an unprivileged user (not root)
- [ ] For internet exposure: put behind nginx + Let's Encrypt, set `CHAN_BEHIND_PROXY=true`
- [ ] Block port 8080 externally if using a reverse proxy

---

## Binary Size

After `cargo build --release`, the binary is typically:
- ~12–18 MiB (includes bundled SQLite + image processing + all templates)
- No separate runtime files needed except the `chan-data/` directory it creates itself
