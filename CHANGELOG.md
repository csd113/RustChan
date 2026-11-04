# Changelog

Release highlights and upgrade cautions. For current operating instructions, use
[setup](SETUP.md), [backups](docs/backups.md), and
[software updates](docs/software-updates.md). Earlier implementation details
remain in Git history.

## RustChan 1.7.5 — 2026-11-03

- Enabled HTTPS protocol negotiation for HTTP/2 with HTTP/1.1 fallback on
  manual, development and ACME certificate paths.
- Check final media destinations before committing posts. Late publication
  failures now identify the saved post instead of falsely confirming upload success;
  retries do not confirm success while required media is missing.

- Fixed private setup by collecting a separate board password and keeping its
  reviewed hash out of the HTML. Invalid upload limits now fail before review
  instead of causing an internal error when setup finishes. Removed the
  nonfunctional read-only setup toggle; password-required posting remains available.
- Fixed setup silently disabling video, audio, and PDF uploads when image
  uploads were unchecked. Each media setting now applies independently.
- Made advertised BMP and TIFF uploads selectable in posting file pickers.
- Fixed banner and favicon uploads rejected by conflicting document and HTTP
  referrer policies. Thanks to @jfkn1ght for reporting the banner problem in #123.
- Tightened CSRF checks for forms, login redirects, backups, and browser origins.
- Replaced the automatic updater with a version check and a GitHub download link.
  Upgrades and settings restarts now use your normal launcher or deployment tools.
- Added an Intel Mac download alongside Apple Silicon and the existing Linux and
  Windows builds. Issue reports now ask only for version, platform, and the issue.

- Corrected upload rejection status mapping, including secondary audio type and
  size errors, while keeping filesystem and decoder diagnostics private.

- Corrected Tor descriptor-timeout warning counts across dependency-only and
  shared outputs, including concurrent events and suppression-window summaries.

- Updated the supported toolchain to Rust 1.99.1 and refreshed Rust dependencies,
  including Hayro 0.8, ZIP 9, and Tor Arti 2.8.0 (0.48.0 crates).
- Added manual CI validation and native Linux x86_64 builds alongside ARM64,
  Apple Silicon, and Windows.

Upgrading from 1.7.1: stop RustChan, back up the complete data directory and old
binary, then replace the binary in place. Keep the same data path; startup handles
the database upgrade. Older external updater services need a manual switch;
see the [upgrade guide](https://github.com/csd113/RustChan/blob/main/docs/software-updates.md#manual-upgrade-of-a-source-install).

## RustChan 1.7.1 — 2026-10-09

- Fixed startup migration from earlier releases, including 1.6.6 databases with
  the additive report index. Unknown schema drift still fails closed.
- Back up the complete data directory before upgrading. Successful startup
  verifies the schema before recording 1.7.1 and retains content and admin records.
- Replaces 1.7.0 as stable; 1.7.0 remains a prerelease because of its migration
  failure.

## RustChan 1.7.0 — 2026-10-08

- Improved thumbnail caching, thread controls, archive navigation, and themes.
- Hardened posting, retry handling, retention, recoverable media operations,
  authentication, uploads, backups, request limits, and settings locks.
- Retained signed Linux updates and full data-and-binary rollback; the minimum
  updater is 1.6.6. Back up before upgrading. This release had an upgrade failure
  corrected in 1.7.1.

## RustChan 1.6.6 — 2026-10-03

- Fixed migration of supported earlier databases, including 1.6.0 installations
  missing post-query indexes. Unknown schema drift remains rejected.
- Integrated the Linux update controller and process monitor into the application
  executable, with signed release verification and full rollback. New native
  update packages require the 1.6.6 controller generation.
- Improved initial database connection timing, executable verification, and HTTPS
  readiness. Updated Arti to 0.47 and container images to Debian 13.
- Same-boot loss of writer-exit proof blocks startup and can require a host reboot;
  recovery never assumes that a writer has stopped.

## RustChan 1.6.5 — 2026-10-03

- Moved image processing, supported audio decoding, media probing, and PDF
  previews into Rust; FFmpeg remains available for video and uncovered audio.
- Added verified settings restarts and configuration recovery; improved media
  timing, bounded previews, and SQLite connection handling.

## RustChan 1.6.0 — 2026-10-01

- Added signed native Linux updates, verified pre-upgrade snapshots, full
  persistent-state rollback, and administrator update controls.
- Added Linux amd64/arm64 container publishing and persistent-volume Compose
  deployment. Containers, macOS, Windows, and custom installations use their
  deployment tools for upgrades.
- Expanded terminal and web administration, accessible forms, posting recovery,
  quote navigation, and catalog sorting. Requires Rust 1.99.
- The original separate updater setup was superseded by the integrated 1.6.6
  controller; follow the current software update guide.

## RustChan 1.5.0 — 2026-09-28

- Removed ChanNet and `--chan-net`; remove that flag from launch commands.
  Historical `chan_net_*` tables remain for database and backup compatibility.
- Added configurable backup storage and improved transaction rollback, durable
  jobs, busy responses, readiness checks, and board access handling.
- Preserved and verified earlier database structures. Back up before upgrading.

## RustChan 1.4.1

- Rebuilt terminal administration and hardened transactional schema verification,
  relationship constraints, legacy repairs, and reply-counter reconciliation.

## RustChan 1.4.0

- Added validated `--data-dir`; improved setup, preferences, banners, request
  limits, durable media work, and submission retry handling.
- Fixed secure cookies, trusted-proxy handling, backup maintenance coordination,
  and filesystem-free informational CLI commands.
- TLS remains disabled by default; HTTPS-only access requires explicit opt-in.
  Updated Arti and security-sensitive dependencies, retaining Rustls with Ring.

## RustChan 1.3.0

- Added the first-run setup wizard, schema verification, database status, PDF
  limits, and administrator health summaries.
- Improved media validation, upload failure handling, accessibility, and Tor.
- Made detailed readiness and public metrics opt-in and required CSRF for theme
  changes and board backup actions.

## RustChan 1.2.2

- Replaced browser proof-of-work with server-generated image CAPTCHA.
- Improved cookie security, no-JavaScript actions, upload validation, settings
  validation, responsive layouts, and backup controls.

## [1.2.1]

- Added full-site restore support for Backup v4 transfer ZIPs; fixed board-name
  validation, no-JavaScript posting forms, and VP9 color metadata.

## [1.2.0]

- Added the layered offline banner editor, HEIC/HEIF uploads, per-board media
  limits, Blue Sky and Deep Orbit themes, and short-window self-deletion.
- Improved admin workflows, media rendering, board-access redirects, backup
  navigation, and animated banner handling.
- Made self-signed TLS support an explicit build feature and focused macOS release
  artifacts on Apple Silicon.

## [1.1.4]

- Added global/per-board banner management, timed rotation, home announcements,
  guarded external links, and backup support for banner metadata and assets.

## [1.1.3]

- Added scheduled saved backups with retention and per-board password protection.
- Hardened proxy trust, cookies, uploads, duplicate-submission handling, verified
  backups, schema migration, restore quoting, and media failure reporting.
- Improved mobile media, admin maintenance, observability, and release packaging.

## [1.1.2]

- Added board ordering, upload progress, and database-managed custom themes.
- Consolidated runtime data under `rustchan-data/` with startup migration of older
  directories. Improved archive restoration, search, and database repair.

## [1.1.1]

- Added mobile navigation, per-board archive retention, favicon generation, and
  no-JavaScript theme switching.
- Improved restore coordination, shared-media retention, image orientation, and
  linked image/audio playback.

## [1.1.0]

- Added terminal administration, native HTTPS, stronger Tor support, arbitrary
  download-only file uploads, and operational health endpoints.
- Hardened proxy identity, rate limits, restore validation, and background work.
- HTTPS redirects began requiring configured/trusted hosts. The ChanNet service
  introduced here was removed in 1.5.0.

## [1.0.13] — 2026-03-08

- Added streaming backup operations, ChanClassic, configurable site defaults,
  maintenance scheduling, media back-pressure, and SQLite query improvements.

## [1.0.12] — 2026-03-07

- Hardened CAPTCHA replay, CSP, restore size limits, transaction rollback, and
  redirects. Fixed archive thumbnails, rate limiting, and trailing-slash routes.

## [1.0.11] — 2026-03-06

- Added security headers, session expiry, restore bounds, login throttling, ban
  appeals, video embeds, quote previews, and live thread metadata.

## [1.0.9] — 2026-03-06

- Added per-board editing windows and archive controls; fixed AV1 transcoding.

## [1.0.8] — 2026-03-05

- Added thread archives, mobile replies, dice, sage, editing, draft autosave,
  database maintenance, and administrator post history.

## [1.0.7] — 2026-03-05

- Added image metadata processing, image/audio combination posts, and waveforms.

## [1.0.6] — 2026-03-04

- Added saved backup management, per-board backup/restore, and platform CI.

## [1.0.5] — 2026-03-04

- Added automatic WebM transcoding and homepage stats; improved audio uploads.

## [1.0.4] — 2026-03-03

- Added thread IDs, cross-board links, emoji, spoilers, polls, and image resizing.

## [1.0.3] — 2026-03-03

- Renamed the binary to `rustchan-cli`; added terminal upload progress, statistics,
  activity indicators, and interactive administration.

## [1.0.2] — 2026-03-03

- Added reports, moderation logs, thread updates, background workers, and
  client-side compression.

## [1.0.1] — 2026-03-03

- Added the theme picker and persistent theme preferences.

## [1.0.0] — 2026-03-03

- Initial imageboard release with media, moderation, tripcodes, deletion tokens,
  rate limiting, CSRF, SQLite, and configurable deployment.
