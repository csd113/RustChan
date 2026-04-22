#[allow(clippy::too_many_lines)]
pub(super) fn settings_template(secret: &str) -> String {
    format!(
        r#"# RustChan settings.toml
# Restart RustChan after changing this file.
# Environment variables still override these values.

# ── Site identity ─────────────────────────────────────────────────────────────
# Name shown in the browser tab, page header, and home page title.
forum_name = "RustChan"

# Subtitle shown below the site name on the home page.
# Can also be changed later from Admin -> Site Settings.
site_subtitle = "select board to proceed"

# Default theme for first-time visitors before they pick their own.
# Valid values: built-in or admin-created custom theme slugs.
default_theme = "forest"

# Built-in themes enabled when the theme catalog is first seeded.
# After first startup, use Admin -> Theme Catalog to enable or disable themes.
enabled_builtin_themes = ["forest", "blue-sky", "deep-orbit", "terminal", "dorfic", "chanclassic", "aero", "neoncubicle", "fluorogrid"]


# ── Network / web server ──────────────────────────────────────────────────────
# Main HTTP port. The server binds to 0.0.0.0:<port> unless Tor-only mode is enabled.
port = 8080

# Trusted proxy CIDR allowlist for forwarded headers.
# The defaults cover local reverse proxies on the same machine.
trusted_proxy_cidrs = ["127.0.0.1/32", "::1/128"]

# Public hostnames accepted by the HTTP -> HTTPS redirect listener.
# Usually only needed when binding to 0.0.0.0/:: with a manual certificate.
# public_hosts = ["example.com", "www.example.com"]

# Built-in HTTPS listener. On first run a self-signed localhost certificate is
# generated automatically in rustchan-data/runtime/tls/dev/.
# For production, configure [tls.acme] (Let's Encrypt) or [tls.manual_cert].
[tls]
enabled = true
port = 8443
redirect_http = false
http_port = 8080

# Let's Encrypt via ACME (requires the tls-acme Cargo feature):
# [tls.acme]
# enabled = true
# staging = true
# domains = ["example.com"]
# email = "admin@example.com"
# cache_dir = "runtime/tls/acme"

# Manual certificate files:
# [tls.manual_cert]
# cert_path = "runtime/tls/fullchain.pem"
# key_path = "runtime/tls/privkey.pem"


# ── Upload limits ─────────────────────────────────────────────────────────────
# Maximum size for image uploads in MiB (jpg, png, gif, webp, heic).
max_image_size_mb = 8

# Maximum size for video uploads in MiB (mp4, webm).
max_video_size_mb = 50

# Maximum size for audio uploads in MiB (mp3, ogg, flac, wav, m4a, aac).
max_audio_size_mb = 150

# Master switch for arbitrary file uploads.
# When false, boards cannot enable non-media uploads at all.
enable_any_file_uploads_feature = false


# ── Security ──────────────────────────────────────────────────────────────────
# AUTO-GENERATED on first run.
# Do not change this after the site is live unless you also intend to invalidate
# existing CSRF tokens, IP hashes, and ban lookups.
cookie_secret = "{secret}"


# ── Tor Onion Service ─────────────────────────────────────────────────────────
# Built-in Onion Service support (powered by Arti — no system tor required).
# First run downloads ~2 MB of directory data and can take ~30 s.
# The service keypair lives in rustchan-data/runtime/tor/state/keys/ — back it up.
# Delete that directory to rotate to a new .onion address.
enable_tor_support = true

# When true, the HTTP server binds exclusively to loopback so the site is
# reachable only through the Onion Service.
# Requires enable_tor_support = true.
tor_only = false

# Seconds to wait for Tor to connect to the network before timing out and retrying.
# Increase this on censored networks or when using bridges.
tor_bootstrap_timeout_secs = 120

# Maximum number of simultaneous inbound Tor connections.
# Each connection holds one file descriptor.
tor_max_concurrent_streams = 512

# Nickname for this instance's Onion Service key.
# Change this only when multiple RustChan instances share the same
# rustchan-data/runtime/tor/state/ directory.
tor_service_nickname = "rustchan"


# ── Media / external tools ────────────────────────────────────────────────────
# Set to true to hard-exit at startup when ffmpeg is not found.
# When false, the server still starts and video thumbnails fall back to placeholders.
require_ffmpeg = false

# Maximum seconds a single FFmpeg transcode or waveform job may run before
# it is killed. Prevents pathological media files from stalling the worker pool.
ffmpeg_timeout_secs = 120

# Optional explicit ffmpeg binary path. Leave unset to use PATH lookup.
# ffmpeg_path = "/usr/local/bin/ffmpeg"

# Optional explicit ffprobe binary path. Leave unset to use PATH lookup.
# ffprobe_path = "/usr/local/bin/ffprobe"


# ── Maintenance / performance ────────────────────────────────────────────────
# How often (in seconds) to run PRAGMA wal_checkpoint(TRUNCATE) to keep
# the SQLite WAL file from growing unbounded under write load.
# Set to 0 to disable.
wal_checkpoint_interval_secs = 3600

# How often (in hours) to run VACUUM automatically to reclaim disk space
# freed by deleted posts and threads. Set to 0 to disable.
auto_vacuum_interval_hours = 24

# How often (in hours) to create a saved full-site backup automatically.
# These backups are stored in rustchan-data/backups/full/. Set to 0 to disable.
auto_full_backup_interval_hours = 24

# How many saved full-site backups to keep on disk after a new saved full
# backup completes. Older saved full backups beyond this limit are deleted.
# Minimum: 1.
auto_full_backup_copies_to_keep = 1

# How often (in hours) to purge vote records for polls that have expired.
# The poll question and options are kept for display; only per-IP vote rows
# are deleted. Set to 0 to disable.
poll_cleanup_interval_hours = 72

# Database file size (MiB) above which a warning banner appears in the admin panel.
# Set to 0 to disable.
db_warn_threshold_mb = 2048

# Maximum number of pending background jobs (video transcode, waveform, etc.)
# allowed in the queue at once. When this limit is reached, new jobs are
# dropped with a warning instead of accepted.
job_queue_capacity = 1000

# When true, threads that would be hard-deleted by the prune worker are instead
# moved to the archive table, even on boards where archiving is disabled.
archive_before_prune = true

# Maximum total size (MiB) of all thumbnail/waveform cache files across all boards.
# A background task evicts the oldest files when the total exceeds this value.
# Set to 0 to disable.
waveform_cache_max_mb = 200

# Number of threads in Tokio's blocking pool (spawn_blocking).
# Leave 0 for automatic sizing (logical CPUs x 4).
blocking_threads = 0

# SQLite connection pool size.
# Increase this on high-traffic deployments; each connection uses extra memory.
db_pool_size = 8


# ── Federation / ChanNet gateway ──────────────────────────────────────────────
# Uncomment these only if you are using the ChanNet / RustWave integration.

# Base URL of the connected RustWave instance.
# Must begin with http:// or https://.
# rustwave_url = "http://localhost:7071"

# Address for the secondary ChanNet TCP listener.
# Keep this on loopback unless RustWave runs on another machine.
# chan_net_bind = "127.0.0.1:7070"

# Pre-shared API key for /chan/refresh and /chan/poll.
# Must be at least 32 characters. Leave unset to disable those endpoints.
# chan_net_api_key = "replace-with-a-long-random-secret"
"#
    )
}
