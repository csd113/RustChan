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
# Valid values: terminal, aero, dorfic, fluorogrid, neoncubicle, chanclassic
default_theme = "fluorogrid"


# ── Network / web server ──────────────────────────────────────────────────────
# Main HTTP port. The server binds to 0.0.0.0:<port> unless Tor-only mode is enabled.
port = 8080


# ── Upload limits ─────────────────────────────────────────────────────────────
# Maximum size for image uploads in MiB (jpg, png, gif, webp).
max_image_size_mb = 8

# Maximum size for video uploads in MiB (mp4, webm).
max_video_size_mb = 50

# Maximum size for audio uploads in MiB (mp3, ogg, flac, wav, m4a, aac).
max_audio_size_mb = 150


# ── Security ──────────────────────────────────────────────────────────────────
# AUTO-GENERATED on first run.
# Do not change this after the site is live unless you also intend to invalidate
# existing CSRF tokens, IP hashes, and ban lookups.
cookie_secret = "{secret}"


# ── Media processing / feature flags ──────────────────────────────────────────
{server_section}


# ── Federation / ChanNet gateway ──────────────────────────────────────────────
# Uncomment these only if you are using the ChanNet / RustWave integration.

# Base URL of the connected RustWave instance.
# Must begin with http:// or https://.
# rustwave_url = "http://localhost:7071"

# Address for the secondary ChanNet TCP listener.
# Keep this on loopback unless RustWave runs on another machine.
# chan_net_bind = "127.0.0.1:7070"


{tls_section}"#,
        server_section = settings_template_server_section(),
        tls_section = settings_template_tls_section(),
    )
}

const fn settings_template_server_section() -> &'static str {
    r#"
# Tor Onion Service support (powered by Arti — no system tor required).
# When true, Arti bootstraps at startup and hosts a .onion hidden service.
# First run downloads ~2 MB of directory data and takes ~30 s.
# The service keypair lives in rustchan-data/arti_state/keys/ — back it up.
# Delete that directory to rotate to a new .onion address.
enable_tor_support = true

# When true, the HTTP server binds exclusively to 127.0.0.1 so the site is
# reachable ONLY through the Tor hidden service — clearnet access is blocked.
# Requires enable_tor_support = true. Default: false (dual-stack: both
# clearnet and Tor are active simultaneously).
# tor_only = false

# Seconds to wait for Tor to connect to the network before giving up and
# retrying. The default (120 s) works on open networks. On censored networks
# or when using bridges, increase this to 300 or more.
# tor_bootstrap_timeout_secs = 120

# Maximum number of simultaneous inbound Tor connections.
# Each connection holds one file descriptor. Reduce if you hit FD limits.
# tor_max_concurrent_streams = 512

# Nickname for this instance's Tor hidden service key.
# Only needs changing when multiple rustchan instances share the same
# rustchan-data/arti_state/ directory — identical nicknames cause key
# collisions and one instance will fail to start its onion service.
# tor_service_nickname = "rustchan"

# Set to true to hard-exit at startup when ffmpeg is not found.
# When false (default), the server starts normally and video thumbnails
# are replaced with SVG placeholders.
require_ffmpeg = false

# Optional explicit ffmpeg binary path. Leave unset to use PATH lookup.
# ffmpeg_path = "/usr/local/bin/ffmpeg"

# Optional explicit ffprobe binary path. Leave unset to use PATH lookup.
# ffprobe_path = "/usr/local/bin/ffprobe"

# Master switch for arbitrary file uploads. Default: false.
# When false, boards cannot enable non-media uploads at all.
enable_any_file_uploads_feature = false

# How often (in seconds) to run PRAGMA wal_checkpoint(TRUNCATE) to keep
# the SQLite WAL file from growing unbounded under write load.
# Set to 0 to disable. Default: 3600 (hourly).
wal_checkpoint_interval_secs = 3600

# How often (in hours) to run VACUUM automatically to reclaim disk space
# freed by deleted posts and threads. Set to 0 to disable. Default: 24.
auto_vacuum_interval_hours = 24

# How often (in hours) to purge vote records for polls that have expired.
# The poll question and options are kept for display; only per-IP vote rows
# are deleted. Set to 0 to disable. Default: 72.
poll_cleanup_interval_hours = 72

# Database file size (MiB) above which a warning banner appears in the admin
# panel. Set to 0 to disable. Default: 2048 (2 GiB).
db_warn_threshold_mb = 2048

# Maximum number of pending background jobs (video transcode, waveform, etc.)
# allowed in the queue at once. When this limit is reached, new jobs are
# silently dropped (with a warning log) rather than accepted. Default: 1000.
job_queue_capacity = 1000

# Maximum seconds a single FFmpeg transcode or waveform job may run before
# it is killed. Prevents pathological media files from stalling the worker
# pool indefinitely. Default: 120.
ffmpeg_timeout_secs = 120

# When true, threads that would be hard-deleted by the prune worker are instead
# moved to the archive table, even on boards where archiving is disabled. This
# acts as a global safety net against silent data loss when a board hits its
# thread limit. Default: true.
archive_before_prune = true

# Maximum total size (MiB) of all thumbnail/waveform cache files across all
# boards. A background task periodically evicts the oldest files when the
# total exceeds this value. Set to 0 to disable. Default: 200.
waveform_cache_max_mb = 200

# Number of threads in Tokio's blocking pool (spawn_blocking). Every page
# render and DB write goes through this pool; sizing it to CPUs × 4 prevents
# it from becoming a bottleneck under concurrent load.
# Default: logical CPUs × 4 (auto-detected at startup; leave 0 for auto).
blocking_threads = 0
"#
}

const fn settings_template_tls_section() -> &'static str {
    r#"
# ── TLS / HTTPS ───────────────────────────────────────────────────────────────
# HTTPS is enabled by default on port 8443. On first run a self-signed
# localhost certificate is generated automatically in rustchan-data/tls/dev/.
# For production, configure [tls.acme] (Let's Encrypt) or [tls.manual_cert].
[tls]
enabled = true
port = 8443

# Redirect plain HTTP → HTTPS (binds an extra listener on http_port).
# redirect_http = true
# http_port = 8080

# Let's Encrypt via ACME (requires the tls-acme Cargo feature):
# [tls.acme]
# enabled = true
# staging = true
# domains = ["example.com"]
# email = "admin@example.com"
# cache_dir = "tls/acme"
"#
}
