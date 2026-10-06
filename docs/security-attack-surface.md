# RustChan request attack surface — 2026-10-06

Inventory completed against the unmodified route composition before implementation changes. This file records existing controls and review targets; the companion audit report records final changes and results. All GET routes also accept HEAD. No RSS, external JSON posting, legacy posting, or separate upload endpoint exists. Unknown routes use the 404 fallback. Board media and public posting are shared paths; no alternate posting parser was found.

## Shared boundary

All main-server routes consume bounded headers (64 KiB aggregate, 32 KiB per value), reject Transfer-Encoding, pass through transport/client identity, timeout, response-header and browsing-rate middleware. Client identity is derived from ConnectInfo; forwarding headers are used only with behind_proxy and a CIDR-trusted immediate peer. Cookies are untrusted and parsed by CookieJar. Host, Origin, Referer and fetch metadata are consumed by origin/cookie handling; Range and validators are used by media/static serving; X-Requested-With selects JSON post-error responses. Before this audit there was no explicit service-level URI limit or general streaming form cap beyond Axum's 2 MiB extractor default.

## Control and cost profiles

| Profile | Authentication / authorization / CSRF | Parameters and bodies | Database / filesystem / CPU / memory / rate controls / output |
|---|---|---|---|
| Public read | Anonymous; board password and admin bypass where applicable; no mutation CSRF | Board slug, numeric IDs; page query, theme/activity cookies, ETag; empty expected body | Board/post/poll/preference reads; HTML text/attributes and generated URLs; escaped metadata, prepared body_html. Browsing budget; catalog/hidden/thread/archive materialization merits review. |
| Search | Anonymous; board viewing permission | board; q (256 Unicode scalars after trimming), page i64; empty body | Two parameterized FTS prefix queries (12 alphanumeric terms), count plus sort and 20-result page. Browsing budget only before audit; SQL cost independent of result size. Query echo HTML/URL escaped. |
| Posting | Board posting permission, bans, optional CAPTCHA; public CSRF; cooldown and submission-token checks at commit | board / thread id; multipart _csrf, submission_token, name, subject, body, deletion_token, sage, captcha_id/answer, file/audio_file/image_file, poll fields | Stream to RAII temporary files; 512 MiB aggregate data, 516 MiB envelope, 64 fields, 64 KiB text/unknown fields, 64 KiB preamble/part headers, 200-byte boundary; board media limits; 10-minute body deadline. One concurrent public media request. Content sniffing, hashing, decoding/transcoding, thumbnails; staged filesystem publication and short IMMEDIATE transaction; job scheduling. Post limits and escaped metadata/markup. Browsing policy legacy includes writes, reads policy excludes writes. |
| Public form | Anonymous; board access, ownership grants or ban checks as required; CSRF for actions (menu actions also bind cookie/origin) | Form _csrf plus route-specific fields: report post/thread/board/reason; appeal reason; vote poll/option/board; edit body; delete ownership; unlock password/return_to; preference flags/return_to | Axum default 2 MiB unless route sets 64 KiB. Report reason 256 scalars; appeal 512; report limit 20/hour per identity with open-report dedup; one appeal/day; poll unique voter; transactional target validation. Cookie preferences and internal redirects. Unlock Argon2 and failure counters. |
| CAPTCHA | Anonymous; syntactically valid board/id before audit | id 32 hex; query board ASCII identifier; empty body | PNG generation/filtering and challenge map; five-minute retention; browsing budget only; synchronous CPU and full-map expiry scan before audit. Output server-generated PNG. |
| Media | Board view permission including media password gate | wildcard media_path; v; Range, If-* | Checked board-relative canonical containment; no arbitrary filesystem path; ServeFile streaming/ranges and MIME/nosniff; UUID media identifiers; browsing exemption; DB authorization lookup and filesystem metadata. |
| Setup | Uninitialized setup or administrator-authorized reopen; scoped CSRF and pending setup grant | preset/site metadata/board policies/admin credentials/theme, password token, _csrf | Default form cap; configuration validators; password hashing, SQLite setup/config writes and staged settings persistence. Rendering escapes submitted metadata; no unauthenticated reopen. |
| Admin read | Valid expiring admin session except login entry; no CSRF for reads | Dashboard filters/page/domain/search, logs after/max bytes, jobs selection, mod-log page, ip_hash; empty body | DB reads and diagnostic/filesystem/log reads, bounded log tail; private no-store; HTML escapes stored posts/reports/reasons. No distinct moderator role: admin accounts hold full privilege. Update/restart progress endpoints require session. |
| Admin form | Admin session (login accepts anonymous scoped login CSRF) + session-scoped CSRF + same-origin policy (valid CSRF fallback where designed); sensitive account operations reauthenticate | _csrf and typed/allowlisted per-handler fields; board/thread/post IDs; config sections/actions; ban reason/filter/metadata/settings/secrets | Default form cap; config and board validators; parameterized writes/mod log, controlled filesystem/config mutations, restart/update coordinator, maintenance gate; HTML/redirect outputs escaped/encoded. Password hashing and maintenance can be expensive. |
| Admin asset | Admin session originally checked after multipart iteration; scoped CSRF and origin | Multipart _csrf, target board and file/image/icon metadata; favicon 5 MiB body, banner 8 MiB body | Limited bytes per part, image content validation and thumbnail/storage writes; rejected-file cleanup; scalar/part-count behavior merits review. |
| Restore | Admin session and origin checked before streaming; maintenance gate; scoped CSRF before restore mutation | Multipart archive/password/_csrf/target/options; route extractor 20 GiB, actual stream 8 GiB; archive expansion total 8 GiB; bounded manifest and entry paths/counts | RAII temp archive, archive-layout/preflight validation, SQL schema/trust validation, recomputed body_html, staged rollback/persistent recovery; password verification and disk/database replacement expensive. Timeout exemptions deserve review. |
| Admin backup read | Admin session; GET creates downloadable artifact but does not alter board content | board; inline/attachment choice; kind/filename for saved download | Validated backup names and containment; maintenance coordination, SQLite snapshots/archive creation or streaming download; expensive, timeout-exempt where applicable; private cache. |

## Complete main-router route inventory

Path parameter names appear in braces. Handler structs are the authoritative per-field schema and the named cost profile supplies the remaining trust-boundary columns above.

| Method | Path | Handler | Profile |
|---|---|---|---|
| GET | `/healthz` | `observability::healthz` | Public read |
| GET | `/readyz` | `observability::readyz` | Public read |
| GET | `/metrics` | `observability::metrics` | Public read |
| GET | `/favicon.ico` | `crate::handlers::favicon::serve_favicon_ico` | Public read |
| GET | `/favicon-16x16.png` | `crate::handlers::favicon::serve_favicon_16` | Public read |
| GET | `/favicon-32x32.png` | `crate::handlers::favicon::serve_favicon_32` | Public read |
| GET | `/apple-touch-icon.png` | `crate::handlers::favicon::serve_apple_touch_icon` | Public read |
| GET | `/captcha/{id}` | `crate::handlers::captcha::serve_captcha_image` | CAPTCHA |
| GET | `/android-chrome-192x192.png` | `crate::handlers::favicon::serve_android_chrome_192` | Public read |
| GET | `/android-chrome-512x512.png` | `crate::handlers::favicon::serve_android_chrome_512` | Public read |
| POST | `/nsfw/accept` | `crate::handlers::board::accept_nsfw` | Public form |
| GET | `/theme/{theme}` | `crate::handlers::board::set_theme` | Public read |
| POST | `/preferences` | `crate::handlers::board::set_user_preferences` | Public form |
| GET | `/banned` | `crate::handlers::board::banned_page` | Public read |
| GET | `/theme-css/{theme}` | `crate::handlers::board::serve_theme_css` | Public read |
| GET | `/banner/assets/{id}` | `crate::handlers::banner::serve_banner_asset` | Public read |
| GET | `/banner/external/{id}` | `crate::handlers::banner::external_banner_warning_page` | Public read |
| GET | `/banner/external/{id}/continue` | `crate::handlers::banner::external_banner_continue` | Public read |
| GET | `/setup` | `crate::handlers::setup::setup_get` | Setup |
| POST | `/setup/review` | `crate::handlers::setup::setup_review` | Setup |
| POST | `/setup/finish` | `crate::handlers::setup::setup_finish` | Setup |
| GET | `/` | `crate::handlers::board::index` | Public read |
| GET | `/{board}` | `crate::handlers::board::board_index` | Public read |
| POST | `/{board}` | `crate::handlers::board::create_thread` | Posting |
| GET | `/{board}/unlock` | `crate::handlers::board::board_unlock_page` | Public read |
| POST | `/{board}/unlock` | `crate::handlers::board::unlock_board_access` | Public form |
| GET | `/{board}/catalog` | `crate::handlers::board::catalog` | Public read |
| GET | `/{board}/hidden` | `crate::handlers::board::hidden_threads` | Public read |
| POST | `/{board}/thread-preference` | `crate::handlers::board::update_thread_preference` | Public form |
| GET | `/{board}/archive` | `crate::handlers::board::board_archive` | Public read |
| GET | `/{board}/search` | `crate::handlers::board::search` | Search |
| GET | `/{board}/thread/{id}` | `crate::handlers::thread::view_thread` | Public read |
| POST | `/{board}/thread/{id}` | `crate::handlers::thread::post_reply` | Posting |
| GET | `/{board}/post/{id}/edit` | `crate::handlers::thread::edit_post_get` | Public read |
| POST | `/{board}/post/{id}/edit` | `crate::handlers::thread::edit_post_post` | Public form |
| GET | `/{board}/post/{id}/delete` | `crate::handlers::thread::delete_post_get` | Public read |
| POST | `/{board}/post/{id}/delete` | `crate::handlers::thread::delete_own_post` | Public form |
| POST | `/report` | `crate::handlers::board::file_report` | Public form |
| POST | `/appeal` | `crate::handlers::board::submit_appeal` | Public form |
| POST | `/vote` | `crate::handlers::thread::vote_handler` | Public form |
| GET | `/api/post/{board}/{post_id}` | `crate::handlers::board::api_post_preview` | Public read |
| GET | `/{board}/post/{post_id}` | `crate::handlers::board::redirect_to_post` | Public read |
| GET | `/{board}/thread/{id}/updates` | `crate::handlers::thread::thread_updates` | Public read |
| GET | `/boards/{*media_path}` | `crate::handlers::board::serve_board_media` | Media |
| GET | `/admin/restart/status` | `crate::handlers::admin::restart::status` | Admin read |
| POST | `/admin/restart` | `crate::handlers::admin::restart::restart` | Admin form |
| GET | `/admin/updates/status` | `crate::handlers::admin::updates::status` | Admin read |
| POST | `/admin/updates/check` | `crate::handlers::admin::updates::check` | Admin form |
| POST | `/admin/updates/install` | `crate::handlers::admin::updates::install` | Admin form |
| POST | `/admin/network/settings` | `crate::handlers::admin::update_network_settings` | Admin form |
| POST | `/admin/appearance/defaults` | `crate::handlers::admin::update_visitor_defaults` | Admin form |
| POST | `/admin/secrets/rotate` | `crate::handlers::admin::rotate_secret` | Admin form |
| POST | `/admin/accounts/{action}` | `crate::handlers::admin::update_account` | Admin form |
| POST | `/admin/config/{section}` | `crate::handlers::admin::update_runtime_settings` | Admin form |
| GET | `/admin` | `crate::handlers::admin::admin_index` | Admin read |
| POST | `/admin/login` | `crate::handlers::admin::admin_login` | Admin form |
| POST | `/admin/logout` | `crate::handlers::admin::admin_logout` | Admin form |
| GET | `/admin/panel` | `crate::handlers::admin::admin_panel` | Admin read |
| GET | `/admin/site-health/jobs` | `crate::handlers::admin::admin_site_health_jobs` | Admin read |
| POST | `/admin/site-health/jobs/dismiss` | `crate::handlers::admin::dismiss_failed_site_health_jobs` | Admin form |
| GET | `/admin/log/live` | `crate::handlers::admin::admin_live_log` | Admin read |
| POST | `/admin/board/create` | `crate::handlers::admin::create_board` | Admin form |
| POST | `/admin/board/delete` | `crate::handlers::admin::delete_board` | Admin form |
| POST | `/admin/board/settings` | `crate::handlers::admin::update_board_settings` | Admin form |
| POST | `/admin/board/reorder` | `crate::handlers::admin::reorder_board` | Admin form |
| POST | `/admin/site/favicon` | `crate::handlers::admin::update_site_favicon` | Admin asset |
| POST | `/admin/board/favicon` | `crate::handlers::admin::update_board_favicon` | Admin asset |
| POST | `/admin/board/favicon/clear` | `crate::handlers::admin::clear_board_favicon_override` | Admin form |
| POST | `/admin/site/banner` | `crate::handlers::admin::upload_global_banner` | Admin asset |
| POST | `/admin/home/banner` | `crate::handlers::admin::upload_home_banner` | Admin asset |
| POST | `/admin/board/banner` | `crate::handlers::admin::upload_board_banner` | Admin asset |
| POST | `/admin/board/banner/clear` | `crate::handlers::admin::clear_board_banner_override` | Admin form |
| POST | `/admin/banner/update` | `crate::handlers::admin::update_banner_meta` | Admin form |
| POST | `/admin/banner/delete` | `crate::handlers::admin::delete_banner` | Admin form |
| POST | `/admin/banner/move` | `crate::handlers::admin::move_banner` | Admin form |
| POST | `/admin/thread/action` | `crate::handlers::admin::thread_action` | Admin form |
| POST | `/admin/thread/delete` | `crate::handlers::admin::admin_delete_thread` | Admin form |
| POST | `/admin/post/delete` | `crate::handlers::admin::admin_delete_post` | Admin form |
| POST | `/admin/ban/add` | `crate::handlers::admin::add_ban` | Admin form |
| POST | `/admin/ban/remove` | `crate::handlers::admin::remove_ban` | Admin form |
| POST | `/admin/report/resolve` | `crate::handlers::admin::resolve_report` | Admin form |
| GET | `/admin/mod-log` | `crate::handlers::admin::mod_log_page` | Admin read |
| POST | `/admin/filter/add` | `crate::handlers::admin::add_filter` | Admin form |
| POST | `/admin/filter/remove` | `crate::handlers::admin::remove_filter` | Admin form |
| POST | `/admin/site/settings` | `crate::handlers::admin::update_site_settings` | Admin form |
| POST | `/admin/theme/create` | `crate::handlers::admin::create_theme` | Admin form |
| POST | `/admin/theme/update` | `crate::handlers::admin::update_theme` | Admin form |
| POST | `/admin/theme/delete` | `crate::handlers::admin::delete_theme` | Admin form |
| POST | `/admin/db/check` | `crate::handlers::admin::admin_db_check` | Admin form |
| POST | `/admin/media/settings` | `crate::handlers::admin::update_media_settings` | Admin form |
| POST | `/admin/setup/reopen` | `crate::handlers::setup::admin_reopen_setup` | Admin form |
| POST | `/admin/setup/close` | `crate::handlers::setup::admin_close_setup` | Admin form |
| GET | `/admin/db/repair` | `crate::handlers::admin::admin_db_repair_status` | Admin read |
| POST | `/admin/db/repair` | `crate::handlers::admin::admin_db_repair` | Admin form |
| GET | `/admin/db/repair/status` | `crate::handlers::admin::admin_db_repair_status` | Admin read |
| GET | `/admin/db/repair/progress` | `crate::handlers::admin::admin_db_repair_progress_json` | Admin read |
| POST | `/admin/vacuum` | `crate::handlers::admin::admin_vacuum` | Admin form |
| POST | `/admin/ip/report` | `crate::handlers::admin::admin_ip_report` | Admin form |
| GET | `/admin/ip/{ip_hash}` | `crate::handlers::admin::admin_ip_history` | Admin read |
| POST | `/admin/post/ban-delete` | `crate::handlers::admin::admin_ban_and_delete` | Admin form |
| POST | `/admin/appeal/dismiss` | `crate::handlers::admin::dismiss_appeal` | Admin form |
| POST | `/admin/appeal/accept` | `crate::handlers::admin::accept_appeal` | Admin form |
| GET | `/admin/backup` | `crate::handlers::admin::admin_backup` | Admin backup read |
| GET | `/admin/restore` | `inline redirect` | Admin read |
| POST | `/admin/restore` | `crate::handlers::admin::admin_restore` | Restore |
| GET | `/admin/board/backup/{board}` | `crate::handlers::admin::board_backup` | Admin backup read |
| GET | `/admin/board/restore` | `inline redirect` | Admin read |
| POST | `/admin/board/restore` | `crate::handlers::admin::board_restore` | Restore |
| POST | `/admin/backup/create` | `crate::handlers::admin::create_full_backup` | Admin form |
| POST | `/admin/backup/settings` | `crate::handlers::admin::update_full_backup_settings` | Admin form |
| POST | `/admin/board/backup/create` | `crate::handlers::admin::create_board_backup` | Admin form |
| GET | `/admin/backup/download/{kind}/{filename}` | `crate::handlers::admin::download_backup` | Admin backup read |
| GET | `/admin/backup/progress` | `crate::handlers::admin::backup_progress_json` | Admin read |
| POST | `/admin/backup/delete` | `crate::handlers::admin::delete_backup` | Admin form |
| POST | `/admin/backup/restore-saved` | `crate::handlers::admin::restore_saved_full_backup` | Admin form |
| POST | `/admin/backup/extract-board` | `crate::handlers::admin::extract_board_from_full_backup` | Admin form |
| POST | `/admin/board/backup/restore-saved` | `crate::handlers::admin::restore_saved_board_backup` | Admin form |
| GET | `/static/style.css` | `serve_css` | Public read |
| GET | `/static/main.js` | `serve_main_js` | Public read |
| GET | `/static/admin.css` | `serve_admin_css` | Public read |
| GET | `/static/admin.js` | `serve_admin_js` | Public read |
| GET | `/static/theme-init.js` | `serve_theme_init_js` | Public read |

## Additional listener and fallbacks

The HTTP-to-HTTPS listener accepts any method on `/{*path}` and builds a permanent redirect using configured public hosts/ACME domains or a loopback fallback, never an arbitrary untrusted Host. It also runs request-boundary validation. It performs no database or filesystem work. Static assets are embedded and expose no directory browsing. ACME protocol handling is managed separately by the configured TLS provider. Health/readiness/metrics expose runtime readiness/aggregate telemetry; metrics authorization policy is recorded in the final report. No third-party endpoint is within this audit scope.
