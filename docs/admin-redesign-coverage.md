# Admin redesign coverage and evidence

Specification: September 30, 2026 attached `goal-objective.md`. **Complete: implementation, functional verification and final validation all passed.** The supplied inventory remains untouched. No deployment, production access or production changes have been made.

## Milestones

1. Source recheck, parser support, atomic persistence, proxy and browsing policy: complete.
2. All TLS, Tor, media, maintenance and system controls: complete.
3. Accounts, storage/secret workflows, logging, useful operator policies and legacy consolidation: complete.
4. Task navigation/search and existing live-save correctness: complete.
5. Required final Rust validation and complete evidence audit: complete.

## Original 70 runtime settings

Every inventory leaf is listed below. All 70 leaves have working controls or a justified dedicated management workflow. Evidence abbreviations below refer to the exact commands and retained local artifacts in the validation section.

| Setting | Inventory key | Interface/workflow | Persistence/application | Evidence/status |
|---|---|---|---|---|
| Trust reverse-proxy headers | behind_proxy | `#network-security` → `#setting-behind_proxy` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Trusted proxy networks | trusted_proxy_cidrs | `#network-security` → `#setting-trusted_proxy_cidrs` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Public hostnames | public_hosts | `#network-security` → `#setting-public_hosts` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Secure authentication cookies | https_cookies | `#network-security` → `#setting-https_cookies` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Primary HTTP port | port | `#network-security` → `#setting-port` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Listen address / interface | bind_addr | `#network-security` → `#setting-bind_addr` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Browsing request allowance | rate_limit_gets | `#network-security` → `#setting-rate_limit_gets` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Browsing rate-limit window | rate_limit_window | `#network-security` → `#setting-rate_limit_window` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Administrator session lifetime | session_duration | `#network-security` → `#setting-session_duration` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Public detailed readiness | public_readiness_details | `#network-security` → `#setting-public_readiness_details` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Public Prometheus metrics | public_metrics_enabled | `#network-security` → `#setting-public_metrics_enabled` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Native HTTPS listener | tls.enabled | HTTPS & Certificates → `tls.enabled` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| Disable plaintext application access | tls.require_https | HTTPS & Certificates → `tls.require_https` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| HTTPS port | tls.port | HTTPS & Certificates → `tls.port` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| HTTP-to-HTTPS redirect listener | tls.redirect_http | HTTPS & Certificates → `tls.redirect_http` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| HTTP redirect port | tls.http_port | HTTPS & Certificates → `tls.http_port` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| Automatic ACME certificates | tls.acme.enabled | HTTPS & Certificates → `tls.acme.enabled` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| Certificate domain names | tls.acme.domains | HTTPS & Certificates → `tls.acme.domains` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| ACME account contact | tls.acme.email | HTTPS & Certificates → `tls.acme.email` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| ACME staging/test issuer | tls.acme.staging | HTTPS & Certificates → `tls.acme.staging` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| ACME state directory | tls.acme.cache_dir | HTTPS & Certificates → `tls.acme.cache_dir` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| Manual certificate chain path | tls.manual_cert.cert_path | HTTPS & Certificates → `tls.manual_cert.cert_path` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| Manual private-key path | tls.manual_cert.key_path | HTTPS & Certificates → `tls.manual_cert.key_path` | Atomic nested TOML; validated certificate source, listener conflicts and feature support; restart | TLS unit preflight/negative persistence tests + JS/no-JS native HTTPS save/restart/browser cases pass |
| Built-in onion service | enable_tor_support | `#tor` → `#setting-enable_tor_support` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Tor-only listener binding | tor_only | `#tor` → `#setting-tor_only` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Tor bootstrap timeout | tor_bootstrap_timeout_secs | `#tor` → `#setting-tor_bootstrap_timeout_secs` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Concurrent Tor streams | tor_max_concurrent_streams | `#tor` → `#setting-tor_max_concurrent_streams` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Onion-service identity nickname | tor_service_nickname | `#tor` → `#setting-tor_service_nickname` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| New-board image upload default | max_image_size_mb | `#media` → `#setting-max_image_size_mb` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| New-board video upload default | max_video_size_mb | `#media` → `#setting-max_video_size_mb` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| New-board audio upload default | max_audio_size_mb | `#media` → `#setting-max_audio_size_mb` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Master arbitrary-file upload gate | enable_any_file_uploads_feature | `#media` → `#setting-enable_any_file_uploads_feature` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Require media tools at startup | require_ffmpeg | `#media` → `#setting-require_ffmpeg` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| FFmpeg executable | ffmpeg_path | `#media` → `#setting-ffmpeg_path` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| FFprobe executable | ffprobe_path | `#media` → `#setting-ffprobe_path` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Generated thumbnail dimension | thumb_size | `#media` → `#setting-thumb_size` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Background job queue capacity | job_queue_capacity | `#media` → `#setting-job_queue_capacity` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Thumbnail/waveform cache budget | waveform_cache_max_mb | `#media` → `#setting-waveform_cache_max_mb` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Always archive overflow threads | archive_before_prune | `#media` → `#setting-archive_before_prune` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Schedule safe reconciliation repairs | media_reconcile_repair_enabled | `#media` → `#setting-media_reconcile_repair_enabled` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Reconciliation audit interval | media_reconcile_interval_hours | `#media` → `#setting-media_reconcile_interval_hours` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Filesystem entries per pass | media_reconcile_files_per_pass | `#media` → `#setting-media_reconcile_files_per_pass` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Reference rows per pass | media_reconcile_database_rows_per_pass | `#media` → `#setting-media_reconcile_database_rows_per_pass` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Bytes hashed per pass | media_reconcile_hash_bytes_per_pass | `#media` → `#setting-media_reconcile_hash_bytes_per_pass` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Repair attempts per pass | media_reconcile_repairs_per_pass | `#media` → `#setting-media_reconcile_repairs_per_pass` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| WAL checkpoint interval | wal_checkpoint_interval_secs | `#schedules` → `#setting-wal_checkpoint_interval_secs` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Automatic VACUUM interval | auto_vacuum_interval_hours | `#schedules` → `#setting-auto_vacuum_interval_hours` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Expired poll-vote cleanup interval | poll_cleanup_interval_hours | `#schedules` → `#setting-poll_cleanup_interval_hours` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Database size warning threshold | db_warn_threshold_mb | `#schedules` → `#setting-db_warn_threshold_mb` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Tokio blocking-pool size | blocking_threads | `#system` → `#setting-blocking_threads` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| SQLite connection pool size | db_pool_size | `#system` → `#setting-db_pool_size` | Atomic root TOML; active/saved/source/next state; environment/CLI overrides; restart | 24 config-admin tests pass; 38 new JS/no-JS browser cases pass (save/restart/override/negative cases, proxy simulations, all nine themes) |
| Database file location | database_path | Storage & secrets → effective/saved/next path table and offline relocation procedure | Secret: atomic new material, applies on restart and revokes sessions; paths: service-host relocation, original data retained | Storage provenance unit test; real table/offline procedure exercised by desktop/mobile JS/no-JS navigation cases; migration remains an explicit stopped-service procedure |
| Board media root | upload_dir | Storage & secrets → effective/saved/next path table and offline relocation procedure | Secret: atomic new material, applies on restart and revokes sessions; paths: service-host relocation, original data retained | Storage provenance unit test; real table/offline procedure exercised by desktop/mobile JS/no-JS navigation cases; migration remains an explicit stopped-service procedure |
| Cookie/CSRF/IP-hashing secret | cookie_secret | Storage & secrets → reauthenticated rotation | Secret: atomic new material, applies on restart and revokes sessions; paths: service-host relocation, original data retained | JS/no-JS rotation/redaction/restart/session-revocation cases pass; transactional startup revocation verified by full Rust suite |
| Site name | forum_name / DB site_name | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Homepage subtitle | site_subtitle | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Homepage new-thread badges | homepage_new_thread_badges_enabled | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Homepage new-reply badges | homepage_new_reply_badges_enabled | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Thread-card new-reply badges | thread_new_reply_badges_enabled | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Default visitor theme | default_theme | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Enabled built-in themes | enabled_builtin_themes | `#appearance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| FFmpeg job timeout | ffmpeg_timeout_secs | `#maintenance` existing form + Application state | DB transaction + live timeout; environment wins at restart | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Automatic post-media pruning | media_auto_prune_enabled | `#maintenance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Active post-media storage cap | media_max_active_content_size_bytes | `#maintenance` existing form + Application state | Single DB transaction, applies live; file/environment seed only | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Saved backup directory | backup_directory | `#backups` existing form + Application state | Atomic TOML, restart; existing backups stay in place | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Automatic backup interval | auto_full_backup_interval_hours | `#backups` existing form + Application state | Atomic TOML, live shared worker; environment wins at restart | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Backup retention count | auto_full_backup_copies_to_keep | `#backups` existing form + Application state | Atomic TOML, live shared worker; environment wins at restart | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Include onion-service identity keys | auto_full_backup_include_tor_hidden_service_keys | `#backups` existing form + Application state | Atomic TOML, live shared worker; environment wins at restart | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Automatic backup format | auto_full_backup_storage_mode | `#backups` existing form + Application state | Atomic TOML, live shared worker; environment wins at restart | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |
| Split ZIP part size | auto_full_backup_split_zip_part_size_gib | `#backups` existing form + Application state | Atomic TOML, live shared worker; environment wins at restart | 32 existing Chromium + 8 no-JS regressions pass; injected SQL rollback and backup file-I/O failure cases pass; full Rust suite passes |

## Additional operator choices and legacy fields

| Item | Interface | Authoritative behavior | Evidence/status |
|---|---|---|---|
| CLI data directory | Storage & secrets | Effective path + stopped-service copy/verify/rollback procedure; launcher remains external | Rendered workflow and disposable browser assertions; passed |
| Accounts/passwords | Accounts | Current-password reauthentication; immediate DB transaction; target sessions revoked | DB account transaction test passes; browser create/reset/revocation passes |
| Logging filter | Logging → `log_filter` | Validated EnvFilter persisted to TOML, loaded before tracing; valid RUST_LOG wins; restart | Operator/config tests; actual save/restart browser case passes |
| Limiter counting policy | Network & Security → `rate_limit_policy` | Default legacy all-method policy preserved; optional GET/HEAD policy; existing route exemptions and posting controls retained | Limiter unit tests + isolated trusted/untrusted proxy browser cases passed |
| Admin failure allowance/window | Access policies | `admin_login_fail_limit`, `admin_login_fail_window_secs`; defaults 5/900 preserved; restart | Runtime getters + validated bounds; browser save/restart passes |
| Board-password failure allowance/window | Access policies | `board_password_fail_limit`, `board_password_fail_window_secs`; defaults 5/900 preserved; restart | Existing password tests + registry/bounds tests; full Rust suite passes |
| Board grant browser lifetime | Access policies | `board_access_cookie_days`; default 30; affects browser expiry of newly issued cookies; existing cookies retain expiry | Runtime cookie issuance wired; signed grants retain existing validation semantics |
| Self edit/delete ownership window | Access policies | `self_action_window_secs`; default 60; board permissions still gate operations; restart | Existing self-action tests use actual policy getter; full Rust suite passes |
| Index page/reply preview counts | Index display | `index_threads_per_page`, `index_reply_previews`; defaults 10/3; restart | Actual queries/renderers wired; browser save/restart passes |
| Read/write deadlines | Request deadlines | `read_timeout_secs`, `write_timeout_secs`; defaults 30/300; existing exemptions retained; restart | Middleware wiring + bounds; browser save/restart passes |
| Legacy combined badge default | Appearance; three separate badge toggles | Compatibility alias retained for seed fallback; DB-owned individual choices win | Loader/appearance tests and existing regression; full Rust suite passes |
| `default_hide_nsfw_boards` | Appearance → Visitor defaults | Newly effective live DB default; explicit visitor cookie preference wins | Default getter/startup/setup wired; browser live/restart/cookie-priority case passes |
| `setup_public_url` | Setup labels it a note; Network public hosts is authoritative | Old field retained as metadata, never advertised as trust configuration | Template/source checks; setup regressions pass |
| `setup_backup_destination` | Setup directs to Backups | Existing backup root authoritative; metadata retained | Source/template checks; backup regressions pass |
| `setup_pdf_upload_limit_bytes` | Setup + board PDF cap | Per-board operational value; metadata retained | Existing setup/board tests; existing regressions pass |
| `boards.edit_window_secs` | Access policies explains global ownership window | Legacy column retained for restore/schema compatibility; actual configured global window used | Self-action Rust regressions pass |

## Existing capabilities retained

| Capability | Final task / preserved surface | Evidence/status |
|---|---|---|
| Board create/edit/order/delete; identity, NSFW, retention, upload caps/types, posting/access/self-actions | Boards; existing routes, IDs and field names | Existing Rust tests; actual nav browser pass; existing UI regressions pass |
| Per-board theme/banner/favicon, global favicon | Appearance and Boards | Existing forms preserved; browser regressions pass |
| Theme catalog/order/custom CSS and guided builder | Appearance | Existing hooks/routes preserved; all nine built-in themes verified in browser and visually inspected; existing regressions pass |
| Home/global/board banners, placement/order/link/rotation policies | Appearance | Existing hooks/routes preserved; browser regressions pass |
| Reports/appeals, word filters, bans, moderation actions | Moderation | Existing routes/auth and fixtures preserved; destructive fixture and permission regressions pass |
| Full/per-board create/download/delete/restore, split ZIP and identities | Backups | Existing forms/routes preserved; disposable backup/restore regressions pass |
| Manual DB maintenance, recovery/integrity, media prune, diagnostics | Maintenance | Existing handlers preserved; disposable regressions pass |
| Dashboard, site health, server log tail/download/live log | Overview | Existing hooks retained; nav test passes JS/no-JS desktop/mobile; dashboard regressions pass |
| Reopen setup / initial setup | Maintenance | Setup controls preserved; inactive labels clarified; setup regressions pass |

## Source recheck and compatibility

- All 54 originally missing controls/workflows and 16 existing runtime controls now have implementations. The static startup registry has 63 leaves: 39 original root settings, 12 original TLS leaves, limiter policy and 11 new operator/logging leaves. Storage/secret management and 16 existing application fields have separate authoritative state renderers.
- The inventory incorrectly described CLI port as CHAN_PORT. Actual previous server code replaced the final socket port after CONFIG validation. The redesign now registers `--port/-p` before CONFIG loading, preserves that highest port precedence and validates/displays the real listener. CHAN_BIND still controls interface/address composition; CHAN_HOST overrides a saved/default interface; Tor-only forces loopback. Pure file previews exclude CLI overrides.
- Site identity, badges/themes and pruning are database-owned after seeding. Their regular forms now use one DB transaction and update live caches only after commit. Redundant TOML mirroring was removed to avoid dual-store partial saves. FFmpeg timeout is persisted with media DB settings, applies live, and CHAN_FFMPEG_TIMEOUT_SECS still wins at restart. Backup worker changes persist successfully before live application.
- Certificate management never returns PEM/key contents or contacts ACME during saves. File pairs are checked with rustls, paths/bounds/features/domain/port relationships are validated. Requiring HTTPS uses a two-step enable/restart/test/confirm workflow on the already active TLS port.
- ACME staging retains existing loader semantics: absent whole ACME table yields false from derived Default, while present table with omitted staging uses true; generated settings explicitly use true. No default was silently changed.
- Trusted forwarding accepts only IP literals, canonicalizes IPv6, ignores untrusted peers and falls back to the peer for malformed trusted headers. Historical hashes from unusually spelled proxy IPv6 values may change; canonical addresses retain hashes. Rate window boundary and exemptions remain unchanged; counter arithmetic now saturates.
- Storage moves are deliberately offline workflows: an open database/media tree and external service launcher cannot be safely relocated by an ordinary HTTP form. Active/saved/next paths and rollback instructions are visible. Rotation is staged, secrets stay redacted, and startup revokes administrator sessions transactionally after detecting changed secret material.
- Original admin fragment anchors, form routes/IDs/hooks remain. Modern CSS selects one task through native fragments, with a readable all-task fallback in browsers without :has support. Server ?open navigation is preserved without JavaScript.

## Validation evidence

Required checks on final Rust changes:

- `cargo fmt --all --check`: passed, `output/admin-redesign-fmt.log`.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo`: passed, `output/admin-redesign-clippy.log`.
- `cargo test --workspace --all-features`: passed (1,212 in each library/binary target, five CLI integration tests, one doctest). Clean final log: `output/admin-redesign-tests-verified.log`.
- `cargo build --all-features`: passed, `output/admin-redesign-build.log`; no dependency or lockfile changes.
- `git diff --check`: passed.
- Targeted config admin tests: 24 pass (`output/admin-config-tests.log`); backup storage tests: five pass (`output/admin-backup-storage-tests.log`); template admin tests: 31 pass; account reauthentication/transaction test passes. The full suite includes these protections.

Browser evidence, using only disposable loopback fixtures:

| Matrix | Cases/results | Evidence |
|---|---|---|
| New network/runtime/management, Chromium and Chromium without JS | 38 passed | `output/admin-redesign-browser-complete.log`; `output/playwright/admin-redesign-complete/results.json` |
| Existing admin dashboard/polish/settings, setup, maintenance, banners/favicons, moderation and backup/restore | 32 passed, six unchanged project/manual skips | `output/admin-redesign-regression.log`; `output/playwright/admin-redesign-regression-final/results.json` |
| Existing no-JS dashboard, moderation, plain admin/site/board/lock forms, diagnostics/IP reports and fresh setup | Eight passed | `output/admin-redesign-nojs-regression.log`; `output/playwright/admin-redesign-nojs-regression/results.json` |
| Refreshed backup I/O failure and destructive backup/progress/restore fixture | Two passed after final filesystem guard | `output/admin-redesign-storage-verified.log` |

The six existing matrix skips are deliberate applicability/manual exclusions, not failed checks. Relevant no-JS cases are run separately; mobile task/overview/account layouts are exercised by the new fixed-viewport cases. Fault-injected repair/backup safety has Rust test coverage. Actual full/split/board restore, invalid archives, identity inclusion and post/thread deletions run on disposable fixtures.

Reproduce the browser matrices after `cargo build --all-features` with `RUSTCHAN_E2E_SKIP_BUILD=1`; this prevents the shared harness's default-feature build from replacing the inspected binary. Set a distinct `RUSTCHAN_AUDIT_OUTPUT` for each matrix.

```sh
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/admin-redesign-complete npx playwright test tests/e2e/admin-management-settings.spec.ts tests/e2e/admin-network-settings.spec.ts tests/e2e/admin-runtime-settings.spec.ts --project=chromium --project=chromium-nojs --workers=2
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/admin-redesign-regression-final npx playwright test tests/e2e/admin-dashboard.spec.ts tests/e2e/admin-polish.spec.ts tests/e2e/admin-settings-phase2.spec.ts tests/e2e/maintenance-backup-phase2.spec.ts tests/e2e/setup-wizard.spec.ts tests/e2e/backup-restore.spec.ts tests/e2e/moderation-phase2.spec.ts tests/e2e/admin-ban-delete.spec.ts tests/e2e/audit-admin-ui.spec.ts --project=chromium --workers=2
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/admin-redesign-nojs-regression npx playwright test tests/e2e/admin-dashboard.spec.ts tests/e2e/admin-ban-delete.spec.ts tests/e2e/moderation-phase2.spec.ts tests/e2e/setup-wizard.spec.ts tests/e2e/nojs-enhancements.spec.ts tests/e2e/audit-nojs-parity.spec.ts --project=chromium-nojs --grep 'no-JS dashboard|no-JS fallback|no-JS report fallback|fresh setup|no-JS admin|admin login, a site-setting' --workers=2
```

## Visual review

Actual UI snapshots were inspected for all nine themes: forest, terminal, chanclassic, dorfic, blue-sky, deep-orbit, aero, fluorogrid and neoncubicle. Desktop/mobile controls remain readable and contained. The mobile account form spacing defect and hidden dashboard/validation navigation defects found during QA were fixed and the relevant cases rerun successfully. Keyboard Tab order and visible focus are exercised; native forms/search/tasks work without JavaScript.

Final snapshot directories:

- `output/playwright/admin-redesign-complete/artifacts/admin-management-settings--bc50a-d-mobile-without-JavaScript-chromium/`: overview/accounts desktop and mobile.
- `output/playwright/admin-redesign-complete/artifacts/admin-runtime-settings-new-04d77-theme-on-desktop-and-mobile-chromium/`: all nine network themes, desktop and mobile; equivalent no-JS directory also retained.
- `output/playwright/admin-redesign-complete/artifacts/admin-network-settings-net-4f4f0-nd-apply-only-after-restart-chromium/`: edited/pending/overridden network controls, desktop and mobile.

## Operational limits and compatibility notes

- Restart-bound settings stay pending until the external service is restarted. Storage relocation is an offline, backup/copy/verify/rollback workflow, not a live data move. Real public ACME issuance depends on DNS, firewall and issuer access; no public instance or issuer was contacted in QA. Tor settings are validated without bootstrapping a public onion service during browser QA.
- Standard root keys and conventional TLS tables preserve unrelated TOML/comments. Exotic inline/dotted TLS layouts that cannot be safely transformed are rejected without mutation; repair the file offline rather than risk corrupting it.
- Secret rotation intentionally changes visitor/IP ban identities and revokes signed grants/sessions. The workflow warns about these consequences and preserves a private backup/rollback procedure.
- Setup prepares SQL changes and cache snapshots before file replacement, holds the shared file lock through commit, and restores exact original file bytes on commit failure. Catastrophic process/power failure between independent filesystem and SQLite commits is not a distributed transaction; retain verified backups. Any rollback failure is reported as an explicit recovery error instead of success.
- Backup destination preparation validates paths/children before mutation and cleans up only directories this operation actually created, while still empty. Concurrently populated or pre-existing paths are never recursively deleted.
- Local Playwright tests and screenshots are under the repository's existing ignored paths. They remain available in this workspace; no ignore policy was changed. The supplied inventory is untouched. No publishing, deployment or production changes were made during implementation.

## Files changed

Production and Rust test changes are listed below. The original supplied inventory is excluded because it was not edited.

- `docs/admin-redesign-coverage.md`
- `src/config.rs`
- `src/config/admin.rs`
- `src/config/admin/application.rs`
- `src/config/admin/certificates.rs`
- `src/config/admin/management.rs`
- `src/config/admin/runtime.rs`
- `src/config/backup_storage.rs`
- `src/config/operator.rs`
- `src/config/template.rs`
- `src/db/accounts.rs`
- `src/db/mod.rs`
- `src/handlers/admin/accounts.rs`
- `src/handlers/admin/auth.rs`
- `src/handlers/admin/mod.rs`
- `src/handlers/admin/settings.rs`
- `src/handlers/admin/settings/backup_settings.rs`
- `src/handlers/admin/settings/board.rs`
- `src/handlers/admin/settings/maintenance.rs`
- `src/handlers/admin/settings/network.rs`
- `src/handlers/admin/settings/runtime.rs`
- `src/handlers/admin/settings/site.rs`
- `src/handlers/board.rs`
- `src/handlers/board/access_preferences.rs`
- `src/handlers/board/create_thread.rs`
- `src/handlers/board/pages.rs`
- `src/handlers/board/tests.rs`
- `src/handlers/posting.rs`
- `src/handlers/setup.rs`
- `src/handlers/thread.rs`
- `src/logging.rs`
- `src/main.rs`
- `src/middleware/ip.rs`
- `src/middleware/rate_limit.rs`
- `src/server/server.rs`
- `src/server/server/headers.rs`
- `src/server/server/routes.rs`
- `src/templates/admin.rs`
- `src/templates/admin/accounts.rs`
- `src/templates/admin/appearance.rs`
- `src/templates/admin/application.rs`
- `src/templates/admin/layout.rs`
- `src/templates/admin/management.rs`
- `src/templates/admin/network.rs`
- `src/templates/admin/search.rs`
- `src/templates/mod.rs`
- `static/admin.css`
- `static/admin.js`

Local browser test changes:

- New: `tests/e2e/admin-network-settings.spec.ts`, `tests/e2e/admin-runtime-settings.spec.ts`, `tests/e2e/admin-management-settings.spec.ts`.
- Updated existing expectations/navigation: `tests/e2e/admin-dashboard.spec.ts`, `tests/e2e/admin-polish.spec.ts`, `tests/e2e/admin-settings-phase2.spec.ts`.
- Harness corrections: `tests/e2e/helpers.ts` supports explicitly unset fixture environment variables, verifies login by the actual login form/authenticated panel, and reads byte-offset logs before UTF-8 decoding.
