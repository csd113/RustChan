// handlers/board.rs
//
// Handles:
//   GET  /                    — board list
//   GET  /:board/             — board index (thread list)
//   POST /:board/             — create new thread
//   GET  /:board/catalog      — catalog view
//   GET  /:board/search       — search results
//   POST /delete              — user post deletion

use crate::{
    config::CONFIG,
    db::{self, NewPost},
    error::{AppError, Result},
    handlers::parse_post_multipart,
    middleware::{validate_csrf, AppState},
    models::*,
    templates,
    utils::{
        // FIX[LOW-8]: sha256_hex now lives in utils::crypto (deduplicated)
        crypto::{hash_ip, new_csrf_token, new_deletion_token, sha256_hex, verify_pow},
        files::save_upload,
        sanitize::{
            // FIX[MEDIUM-8]: apply_word_filters now runs before escape_html
            apply_word_filters,
            escape_html,
            render_post_body,
            validate_body,
            validate_body_with_file,
            validate_name,
            validate_subject,
        },
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use std::collections::HashMap;
use tracing::info;

const PREVIEW_REPLIES: i64 = 3;
const THREADS_PER_PAGE: i64 = 10;

// ─── GET / — board list ───────────────────────────────────────────────────────

pub async fn index(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let (board_stats, site_stats) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Vec<crate::models::BoardStats>, crate::models::SiteStats)> {
            let conn = pool.get()?;
            let boards = db::get_all_boards_with_stats(&conn)?;
            let stats = db::get_site_stats(&conn).unwrap_or_default();
            Ok((boards, stats))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Read the tor onion address from the hostname file if tor is enabled.
    let onion_address: Option<String> = if crate::config::CONFIG.enable_tor_support {
        let data_dir = std::path::PathBuf::from(&crate::config::CONFIG.database_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let hostname_path = data_dir.join("tor_hidden_service").join("hostname");
        std::fs::read_to_string(&hostname_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    Ok((
        jar,
        Html(templates::index_page(
            &board_stats,
            &site_stats,
            &csrf,
            onion_address.as_deref(),
        )),
    ))
}

// ─── GET /:board/ — board index ───────────────────────────────────────────────

pub async fn board_index(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        // FIX[HIGH-2]: is_admin_session check moved inside spawn_blocking so
        // the blocking DB call does not stall the Tokio worker thread.
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<String> {
            let conn = pool.get()?;

            // Resolve admin status inside the blocking task
            let is_admin = jar_session
                .as_deref()
                .map(|sid| db::get_session(&conn, sid).ok().flatten().is_some())
                .unwrap_or(false);

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let total = db::count_threads_for_board(&conn, board.id)?;
            let pagination = Pagination::new(page, THREADS_PER_PAGE, total);
            let threads =
                db::get_threads_for_board(&conn, board.id, THREADS_PER_PAGE, pagination.offset())?;

            let mut summaries = Vec::with_capacity(threads.len());
            for thread in threads {
                let total_replies = thread.reply_count;
                let preview = db::get_preview_posts(&conn, thread.id, PREVIEW_REPLIES)?;
                let omitted = (total_replies - preview.len() as i64).max(0);
                summaries.push(ThreadSummary {
                    thread,
                    preview_posts: preview,
                    omitted,
                });
            }

            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::board_page(
                &board,
                &summaries,
                &pagination,
                &csrf_clone,
                &all_boards,
                is_admin,
                None,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /:board/ — create new thread ───────────────────────────────────────

pub async fn create_thread(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    multipart: Multipart,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    let form = parse_post_multipart(multipart, csrf_cookie.as_deref()).await?;

    if !form.csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let raw_body = form.body;

    let upload_dir = CONFIG.upload_dir.clone();
    let thumb_size = CONFIG.thumb_size;
    let max_image_size = CONFIG.max_image_size;
    let max_video_size = CONFIG.max_video_size;
    let max_audio_size = CONFIG.max_audio_size;
    let ffmpeg_available = state.ffmpeg_available;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data = form.file;
    let audio_file_data = form.audio_file;
    let name_val = form.name;
    let subject_val = form.subject;
    let del_token_val = form.deletion_token;
    let poll_question = form.poll_question;
    let poll_options = form.poll_options;
    let poll_duration = form.poll_duration_secs;
    let pow_nonce = form.pow_nonce;

    let board_short_err = board_short.clone();
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let job_queue = state.job_queue.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let ip_hash = hash_ip(&client_ip, &cookie_secret);
            if let Some(reason) = db::is_banned(&conn, &ip_hash)? {
                return Err(AppError::Forbidden(format!(
                    "You are banned. Reason: {}",
                    if reason.is_empty() {
                        "No reason given".to_string()
                    } else {
                        reason
                    }
                )));
            }

            // PoW CAPTCHA — verified only when the board has it enabled
            if board.allow_captcha && !verify_pow(&board_short, &pow_nonce) {
                return Err(AppError::BadRequest(
                    "CAPTCHA verification failed. Please wait for the solver to complete before posting.".into()
                ));
            }

            let filters: Vec<(String, String)> = db::get_word_filters(&conn)?
                .into_iter()
                .map(|f| (f.pattern, f.replacement))
                .collect();

            let (name, tripcode) = parse_name_tripcode(&validate_name(&name_val));
            // Respect per-board tripcode setting
            let tripcode = if board.allow_tripcodes {
                tripcode
            } else {
                None
            };
            let subject = validate_subject(&subject_val);

            // Validate body: if the board allows media uploads a file may substitute
            // for text, but at least one of the two must be non-empty.
            let board_allows_media = board.allow_images || board.allow_video || board.allow_audio;
            let has_file = file_data.is_some();
            let body_text = if board_allows_media {
                validate_body_with_file(&raw_body, has_file).map_err(AppError::BadRequest)?
            } else {
                validate_body(&raw_body)
                    .map_err(AppError::BadRequest)?
                    .to_string()
            };

            // FIX[MEDIUM-8]: Apply word filters BEFORE HTML escaping so that
            // filter patterns are plain text, not HTML-entity strings.
            let filtered_body = apply_word_filters(&body_text, &filters);
            let escaped_body = escape_html(&filtered_body);
            let body_html = render_post_body(&escaped_body);

            let uploaded = if let Some((data, fname)) = file_data {
                // Detect media type from magic bytes to enforce per-board toggles.
                // We call detect_mime_type here for a quick classification without
                // doing a full save; the real detection runs again in save_upload.
                let detected_mime = crate::utils::files::detect_mime_type(&data)
                    .map_err(|e| AppError::BadRequest(e.to_string()))?;
                let detected_media = crate::models::MediaType::from_mime(detected_mime)
                    .ok_or_else(|| AppError::BadRequest("Unsupported file type.".into()))?;

                match detected_media {
                    crate::models::MediaType::Image if !board.allow_images => {
                        return Err(AppError::BadRequest(
                            "Image uploads are disabled on this board.".into(),
                        ))
                    }
                    crate::models::MediaType::Video if !board.allow_video => {
                        return Err(AppError::BadRequest(
                            "Video uploads are disabled on this board.".into(),
                        ))
                    }
                    crate::models::MediaType::Audio if !board.allow_audio => {
                        return Err(AppError::BadRequest(
                            "Audio uploads are disabled on this board.".into(),
                        ))
                    }
                    _ => {}
                }

                // SHA-256 deduplication — FIX[LOW-8]: use sha256_hex from crypto module
                let hash = sha256_hex(&data);
                if let Some(cached) = db::find_file_by_hash(&conn, &hash)? {
                    let cached_media = crate::models::MediaType::from_mime(&cached.mime_type)
                        .unwrap_or(crate::models::MediaType::Image);
                    Some(crate::utils::files::UploadedFile {
                        file_path: cached.file_path,
                        thumb_path: cached.thumb_path,
                        original_name: crate::utils::sanitize::sanitize_filename(&fname),
                        mime_type: cached.mime_type,
                        file_size: data.len() as i64,
                        media_type: cached_media,
                        processing_pending: false, // cached = already fully processed
                    })
                } else {
                    let f = save_upload(
                        &data,
                        &fname,
                        &upload_dir,
                        &board.short_name,
                        thumb_size,
                        max_image_size,
                        max_video_size,
                        max_audio_size,
                        ffmpeg_available,
                    )
                    .map_err(crate::handlers::classify_upload_error)?;
                    db::record_file_hash(&conn, &hash, &f.file_path, &f.thumb_path, &f.mime_type)?;
                    Some(f)
                }
            } else {
                None
            };

            // ── Image+audio combo ─────────────────────────────────────────────
            // If an audio file was also submitted alongside an image, and the
            // board permits both, save the audio file using the image's thumb.
            let audio_uploaded: Option<crate::utils::files::UploadedFile> =
                if let Some((aud_data, aud_fname)) = audio_file_data {
                    // Validate that the board allows audio
                    if !board.allow_audio {
                        return Err(AppError::BadRequest(
                            "Audio uploads are disabled on this board.".into(),
                        ));
                    }
                    // The primary file must be an image for the combo to be valid
                    let primary_is_image = uploaded
                        .as_ref()
                        .map(|u| matches!(u.media_type, crate::models::MediaType::Image))
                        .unwrap_or(false);
                    if !primary_is_image {
                        return Err(AppError::BadRequest(
                            "Audio can only be combined with an image upload.".into(),
                        ));
                    }
                    // Confirm it's actually audio via magic bytes
                    let aud_mime = crate::utils::files::detect_mime_type(&aud_data)
                        .map_err(|e| AppError::BadRequest(e.to_string()))?;
                    let aud_media = crate::models::MediaType::from_mime(aud_mime)
                        .ok_or_else(|| AppError::BadRequest("Unsupported audio type.".into()))?;
                    if !matches!(aud_media, crate::models::MediaType::Audio) {
                        return Err(AppError::BadRequest(
                            "The audio slot only accepts audio files.".into(),
                        ));
                    }

                    let mut aud_file = crate::utils::files::save_audio_with_image_thumb(
                        &aud_data,
                        &aud_fname,
                        &upload_dir,
                        &board.short_name,
                        max_audio_size,
                    )
                    .map_err(crate::handlers::classify_upload_error)?;

                    // Use the image thumbnail as the audio's thumbnail
                    if let Some(ref img) = uploaded {
                        aud_file.thumb_path = img.thumb_path.clone();
                    }
                    Some(aud_file)
                } else {
                    None
                };

            let deletion_token = if del_token_val.trim().is_empty() {
                new_deletion_token()
            } else {
                // Cap at 64 chars to prevent abuse; anything longer is almost
                // certainly not a legitimate user-chosen token.
                del_token_val.trim().chars().take(64).collect()
            };

            // FIX[MEDIUM-3]: Thread creation and OP post insertion are now
            // wrapped in a single transaction via create_thread_with_op.
            // Previously, a crash between the two calls left an orphaned thread.
            let new_post = NewPost {
                thread_id: 0, // will be overwritten by create_thread_with_op
                board_id: board.id,
                name,
                tripcode,
                subject: subject.clone(),
                body: body_text.clone(),
                body_html,
                ip_hash: ip_hash.clone(),
                file_path: uploaded.as_ref().map(|u| u.file_path.clone()),
                file_name: uploaded.as_ref().map(|u| u.original_name.clone()),
                file_size: uploaded.as_ref().map(|u| u.file_size),
                thumb_path: uploaded.as_ref().map(|u| u.thumb_path.clone()),
                mime_type: uploaded.as_ref().map(|u| u.mime_type.clone()),
                media_type: uploaded.as_ref().map(|u| u.media_type.as_str().to_string()),
                audio_file_path: audio_uploaded.as_ref().map(|u| u.file_path.clone()),
                audio_file_name: audio_uploaded.as_ref().map(|u| u.original_name.clone()),
                audio_file_size: audio_uploaded.as_ref().map(|u| u.file_size),
                audio_mime_type: audio_uploaded.as_ref().map(|u| u.mime_type.clone()),
                deletion_token,
                is_op: true,
            };
            let (thread_id, post_id) =
                db::create_thread_with_op(&conn, board.id, subject.as_deref(), &new_post)?;

            // Create poll if question + at least 2 options were supplied
            let q = poll_question.trim().to_string();
            let valid_opts: Vec<String> = poll_options
                .iter()
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect();
            if !q.is_empty() && valid_opts.len() >= 2 {
                if let Some(secs) = poll_duration {
                    let secs = secs.clamp(60, 30 * 24 * 3600); // clamp 1 min..30 days
                    let expires_at = chrono::Utc::now().timestamp() + secs;
                    db::create_poll(&conn, thread_id, &q, &valid_opts, expires_at)?;
                }
            }

            // ── Background jobs ───────────────────────────────────────────────
            // 1. Media post-processing (video transcode / audio waveform)
            if let Some(ref up) = uploaded {
                if up.processing_pending {
                    let job = match up.media_type {
                        crate::models::MediaType::Video => {
                            Some(crate::workers::Job::VideoTranscode {
                                post_id,
                                file_path: up.file_path.clone(),
                                board_short: board.short_name.clone(),
                            })
                        }
                        crate::models::MediaType::Audio => {
                            Some(crate::workers::Job::AudioWaveform {
                                post_id,
                                file_path: up.file_path.clone(),
                                board_short: board.short_name.clone(),
                            })
                        }
                        _ => None,
                    };
                    if let Some(j) = job {
                        if let Err(e) = job_queue.enqueue(&j) {
                            tracing::warn!("Failed to enqueue media job: {}", e);
                        }
                    }
                }
            }

            // 2. Spam analysis
            let _ = job_queue.enqueue(&crate::workers::Job::SpamCheck {
                post_id,
                ip_hash: ip_hash.clone(),
                body_len: body_text.len(),
            });

            // 3. Thread pruning — now async so HTTP response returns immediately.
            let max_threads = board.max_threads;
            let _ = job_queue.enqueue(&crate::workers::Job::ThreadPrune {
                board_id: board.id,
                board_short: board.short_name.clone(),
                max_threads,
                allow_archive: board.allow_archive,
            });

            info!("New thread {} created in /{}/", thread_id, board.short_name);
            Ok(format!("/{}/thread/{}", board.short_name, thread_id))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → re-render the board index with an inline error banner.
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
            let html = tokio::task::spawn_blocking({
                let pool = state.db.clone();
                let csrf_err = csrf_cookie.clone().unwrap_or_default();
                let board_short = board_short_err.clone();
                let msg = msg.clone();
                move || -> String {
                    let conn = match pool.get() {
                        Ok(c) => c,
                        Err(_) => return String::new(),
                    };
                    let board = match db::get_board_by_short(&conn, &board_short) {
                        Ok(Some(b)) => b,
                        _ => return String::new(),
                    };
                    let all_boards = db::get_all_boards(&conn).unwrap_or_default();
                    let total = db::count_threads_for_board(&conn, board.id).unwrap_or(0);
                    let pagination = crate::models::Pagination::new(1, 10, total);
                    let threads =
                        db::get_threads_for_board(&conn, board.id, 10, 0).unwrap_or_default();
                    let summaries: Vec<crate::models::ThreadSummary> = threads
                        .into_iter()
                        .map(|t| {
                            let preview = db::get_preview_posts(&conn, t.id, 3).unwrap_or_default();
                            let omitted = (t.reply_count - preview.len() as i64).max(0);
                            crate::models::ThreadSummary {
                                thread: t,
                                preview_posts: preview,
                                omitted,
                            }
                        })
                        .collect();
                    templates::board_page(
                        &board,
                        &summaries,
                        &pagination,
                        &csrf_err,
                        &all_boards,
                        false,
                        Some(&msg),
                        db::get_collapse_greentext(&conn),
                    )
                }
            })
            .await
            .unwrap_or_default();

            if !html.is_empty() {
                return Ok((jar, Html(html)).into_response());
            }
            return Err(AppError::BadRequest(msg));
        }
        Err(e) => return Err(e),
    };

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── GET /:board/catalog ──────────────────────────────────────────────────────

pub async fn catalog(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<String> {
            let conn = pool.get()?;
            let is_admin = jar_session
                .as_deref()
                .map(|sid| db::get_session(&conn, sid).ok().flatten().is_some())
                .unwrap_or(false);
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::catalog_page(
                &board,
                &threads,
                &csrf_clone,
                &all_boards,
                is_admin,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── GET /:board/archive ──────────────────────────────────────────────────────

pub async fn board_archive(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);
    const ARCHIVE_PER_PAGE: i64 = 20;

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            if !board.allow_archive {
                return Err(AppError::NotFound(format!(
                    "/{board_short}/ does not have an archive."
                )));
            }

            let total = db::count_archived_threads_for_board(&conn, board.id)?;
            let pagination = Pagination::new(page, ARCHIVE_PER_PAGE, total);
            let threads = db::get_archived_threads_for_board(
                &conn,
                board.id,
                ARCHIVE_PER_PAGE,
                pagination.offset(),
            )?;

            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::archive_page(
                &board,
                &threads,
                &pagination,
                &csrf_clone,
                &all_boards,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── GET /:board/search ───────────────────────────────────────────────────────

pub async fn search(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(q): Query<SearchQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);
    const SEARCH_PER_PAGE: i64 = 20;

    // Cap query length to prevent excessively large LIKE pattern scans.
    let query_str: String = q.q.trim().chars().take(256).collect();
    let page = q.page.max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let total = db::count_search_results(&conn, board.id, &query_str)?;
            let pagination = Pagination::new(page, SEARCH_PER_PAGE, total);
            let posts = db::search_posts(
                &conn,
                board.id,
                &query_str,
                SEARCH_PER_PAGE,
                pagination.offset(),
            )?;

            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::search_page(
                &board,
                &query_str,
                &posts,
                &pagination,
                &csrf_clone,
                &all_boards,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── CSRF cookie helper ───────────────────────────────────────────────────────

/// Ensure the CSRF token cookie is set. Returns (updated_jar, token_string).
pub fn ensure_csrf(jar: CookieJar) -> (CookieJar, String) {
    if let Some(cookie) = jar.get("csrf_token") {
        let token = cookie.value().to_string();
        if !token.is_empty() {
            return (jar, token);
        }
    }
    let token = new_csrf_token();
    let mut cookie = Cookie::new("csrf_token", token.clone());
    // http_only=false is intentional for the double-submit CSRF pattern —
    // the token must be readable by the page so forms can embed it.
    // XSS is mitigated by SameSite=Strict and thorough HTML escaping.
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    // FIX[MEDIUM-11]: set Secure flag based on config (true when behind proxy / HTTPS)
    cookie.set_secure(CONFIG.https_cookies);
    (jar.add(cookie), token)
}

// ─── POST /report ─────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct ReportForm {
    pub post_id: i64,
    pub thread_id: i64,
    pub board: String,
    pub reason: Option<String>,
    pub _csrf: Option<String>,
}

pub async fn file_report(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<ReportForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form._csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let ip_hash = hash_ip(&client_ip, &CONFIG.cookie_secret);
    let reason = form
        .reason
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    let post_id = form.post_id;
    let thread_id = form.thread_id;
    let board_raw = form
        .board
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_raw)?
                .ok_or_else(|| AppError::NotFound("Board not found.".into()))?;
            // Verify post exists and belongs to this board to prevent spoofed reports.
            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            if post.board_id != board.id {
                return Err(AppError::BadRequest(
                    "Post does not belong to this board.".into(),
                ));
            }
            db::file_report(&conn, post_id, thread_id, board.id, &reason, &ip_hash)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Redirect back to the thread the reported post lives in.
    let safe_board = form
        .board
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();
    Ok(Redirect::to(&format!(
        "/{}/thread/{}#p{}",
        safe_board, form.thread_id, form.post_id
    ))
    .into_response())
}

// ─── GET /boards/{*media_path} — serve media with mp4→webm redirect ──────────
//
// Replaces the former nest_service(ServeDir) so we can intercept stale .mp4
// links (created before the background transcoder replaced them with .webm)
// and issue a permanent redirect. All other paths are served via ServeFile.

pub async fn serve_board_media(
    Path(media_path): Path<String>,
    req: axum::extract::Request,
) -> Response {
    use axum::http::StatusCode;
    use std::path::PathBuf;
    use tower::ServiceExt;
    use tower_http::services::ServeFile;

    // Reject path-traversal attempts.
    if media_path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let base = PathBuf::from(&CONFIG.upload_dir);
    let target = base.join(&media_path);

    if target.exists() {
        // File present — forward the real request (with Range, ETag, etc.) to
        // ServeFile so it can respond with 206 Partial Content when needed.
        // iOS Safari requires Range request support to play video — dropping
        // the request headers caused it to receive 200 instead of 206 and
        // refuse playback on videos it tried to stream in chunks.
        let req = req.map(|_| axum::body::Body::empty());
        match ServeFile::new(&target).oneshot(req).await {
            Ok(resp) => resp.map(axum::body::Body::new).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    } else if media_path.ends_with(".mp4") {
        // MP4 was transcoded away — redirect permanently to the .webm sibling.
        let webm_path_str = format!("{}.webm", &media_path[..media_path.len() - 4]);
        let webm_abs = base.join(&webm_path_str);
        if webm_abs.exists() {
            Redirect::permanent(&format!("/boards/{}", webm_path_str)).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

// ─── GET /api/post/{board}/{post_id} ──────────────────────────────────────────
//
// Lightweight JSON endpoint for cross-board quotelink hover previews.
//
// `post_id` is the **global** post ID (the AUTOINCREMENT primary key of the
// `posts` table).  The board name is used only to validate ownership — a link
// like >>>/tech/12345 will 404 if post 12345 actually lives on /b/, preventing
// cross-board information leakage.
//
// Response on success:
//   { "html": "<div class=\"post …\">…</div>", "thread_id": 42 }
// The `thread_id` field lets the client update the link's href to the canonical
// /{board}/thread/{thread_id}#p{post_id} URL after the first hover.
//
// Response on failure: 404 { "error": "not found" }

pub async fn api_post_preview(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
) -> impl axum::response::IntoResponse {
    use axum::http::header;

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<Option<(String, i64)>> {
            let conn = pool.get()?;

            // Fetch the post, validating it belongs to this board.
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            match post {
                None => Ok(None),
                Some(p) => {
                    let thread_id = p.thread_id;
                    let html = crate::templates::render_post(
                        &p,
                        &board_short,
                        "",    // no CSRF needed — preview is read-only
                        false, // no delete controls
                        false, // not admin
                        true,  // show media thumbnail
                        0,     // no edit window
                    );
                    Ok(Some((html, thread_id)))
                }
            }
        }
    })
    .await;

    let json_ct = [(header::CONTENT_TYPE, "application/json")];

    match result {
        Ok(Ok(Some((html, thread_id)))) => {
            let body =
                serde_json::to_string(&serde_json::json!({ "html": html, "thread_id": thread_id }))
                    .unwrap_or_else(|_| r#"{"html":"","thread_id":0}"#.to_string());
            (axum::http::StatusCode::OK, json_ct, body).into_response()
        }
        Ok(Ok(None)) => {
            let body = r#"{"error":"not found"}"#.to_string();
            (axum::http::StatusCode::NOT_FOUND, json_ct, body).into_response()
        }
        _ => {
            let body = r#"{"error":"internal error"}"#.to_string();
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, json_ct, body).into_response()
        }
    }
}

// ─── GET /{board}/post/{post_id} ──────────────────────────────────────────────
//
// Canonical redirect for `>>>/board/N` links.  Resolves the global post ID to
// its containing thread and issues a 302 to /{board}/thread/{thread_id}#p{post_id}.
//
// Users clicking a cross-board quotelink land here on the first click; after
// the first hover preview the JS upgrades the href in-place so subsequent
// clicks go directly to the thread anchor without a server round-trip.

pub async fn redirect_to_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
) -> impl axum::response::IntoResponse {
    use axum::response::Redirect;

    let board_short_for_url = board_short.clone();
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<Option<i64>> {
            let conn = pool.get()?;
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            Ok(post.map(|p| p.thread_id))
        }
    })
    .await;

    match result {
        Ok(Ok(Some(thread_id))) => {
            let url = format!("/{}/thread/{}#p{}", board_short_for_url, thread_id, post_id);
            Redirect::to(&url).into_response()
        }
        _ => {
            // Post not found or wrong board — return a plain 404.
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
    }
}

// ─── POST /appeal ─────────────────────────────────────────────────────────────
// Banned users submit a brief appeal message here.
// Appeals appear in the admin panel under // ban appeals.

#[derive(serde::Deserialize)]
pub struct AppealForm {
    pub reason: String,
    pub _csrf: Option<String>,
}

pub async fn submit_appeal(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<AppealForm>,
) -> impl axum::response::IntoResponse {
    use axum::response::Html;

    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form._csrf.as_deref().unwrap_or(""),
    ) {
        return Html(crate::templates::error_page(403, "CSRF token mismatch.")).into_response();
    }

    let ip_hash = hash_ip(&client_ip, &CONFIG.cookie_secret);
    let reason = form.reason.trim().chars().take(512).collect::<String>();
    if reason.is_empty() {
        return Html(crate::templates::error_page(
            400,
            "Appeal message cannot be empty.",
        ))
        .into_response();
    }

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<&'static str> {
            let conn = pool.get()?;
            // Rate-limit: one appeal per IP per 24 hours
            if db::has_recent_appeal(&conn, &ip_hash)? {
                return Ok("already_filed");
            }
            // Only allow appeals from actually-banned IPs
            if db::is_banned(&conn, &ip_hash)?.is_none() {
                return Ok("not_banned");
            }
            db::file_ban_appeal(&conn, &ip_hash, &reason)?;
            Ok("ok")
        }
    })
    .await;

    let msg = match result {
        Ok(Ok("ok")) => "Your appeal has been submitted. An admin will review it.",
        Ok(Ok("already_filed")) => "You have already filed an appeal in the last 24 hours.",
        Ok(Ok("not_banned")) => "Your IP is not currently banned.",
        _ => "An error occurred. Please try again.",
    };

    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Appeal Submitted</title>
<link rel="stylesheet" href="/static/style.css">
</head><body><div class="page-box error-page">
<h1>appeal submitted</h1>
<p>{msg}</p>
<p><a href="/">return home</a></p>
</div></body></html>"#,
        msg = crate::utils::sanitize::escape_html(msg)
    );
    Html(html).into_response()
}
